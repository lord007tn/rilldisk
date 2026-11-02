// SPDX-License-Identifier: AGPL-3.0-only
//! Finds AI clients on this machine, adds RillDisk to their MCP configuration,
//! removes it again, and checks that the configured server really answers.
//!
//! Shared by `rilldisk mcp setup|doctor` and the desktop's AI assistants card.
//! The exact scope (which files are written, backups, consent, what is never
//! done) is in `docs/proposals/mcp-setup.md`.
//!
//! Every function takes an [`Env`] instead of reading the process environment,
//! so tests point the home and app-data folders at temporary directories.
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

mod check;
mod diff;
mod edit;

pub use check::{Check, CheckError, DEFAULT_TIMEOUT, check};
pub use diff::unified as diff;

/// Name of RillDisk's entry in every client config. Other entries are never touched.
pub const SERVER_NAME: &str = "rilldisk";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// Where to look. [`Env::from_system`] reads the real environment; tests build
/// one around a temporary directory.
#[derive(Clone, Debug)]
pub struct Env {
    pub os: Os,
    pub home: PathBuf,
    /// Windows roaming app data (`%APPDATA%`).
    pub appdata: PathBuf,
    /// Linux `$XDG_CONFIG_HOME` (default `~/.config`).
    pub xdg_config: PathBuf,
    /// Directories searched for client executables (`PATH`).
    pub path: Vec<PathBuf>,
    /// `CODEX_HOME`, if set.
    pub codex_home: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR`, if set.
    pub claude_config_dir: Option<PathBuf>,
}

impl Env {
    pub fn from_system() -> Self {
        let var = |key: &str| {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let os = Os::current();
        let home = if os == Os::Windows {
            var("USERPROFILE").or_else(|| var("HOME"))
        } else {
            var("HOME")
        }
        .unwrap_or_else(|| PathBuf::from(if os == Os::Windows { "C:\\" } else { "/" }));
        Self {
            os,
            appdata: var("APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming")),
            xdg_config: var("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            path: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
            codex_home: var("CODEX_HOME"),
            claude_config_dir: var("CLAUDE_CONFIG_DIR"),
            home,
        }
    }

    /// A test or capture environment: everything under `home`, nothing on PATH.
    pub fn isolated(home: &Path) -> Self {
        Self {
            os: Os::current(),
            appdata: home.join("AppData").join("Roaming"),
            xdg_config: home.join(".config"),
            path: Vec::new(),
            codex_home: None,
            claude_config_dir: None,
            home: home.to_path_buf(),
        }
    }

    /// First executable called `name` on PATH (`.exe`/`.cmd`/`.bat` on Windows).
    pub fn find_program(&self, name: &str) -> Option<PathBuf> {
        let names: Vec<String> = if self.os == Os::Windows {
            ["exe", "cmd", "bat"]
                .iter()
                .map(|ext| format!("{name}.{ext}"))
                .collect()
        } else {
            vec![name.to_string()]
        };
        self.path
            .iter()
            .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
            .find(|p| p.is_file())
    }

    /// `path` with the home folder shown as `~`, for display.
    pub fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) if !self.home.as_os_str().is_empty() => {
                let sep = if self.os == Os::Windows { "\\" } else { "/" };
                format!("~{sep}{}", rest.display())
            }
            _ => path.display().to_string(),
        }
    }

    fn vscode_user(&self) -> PathBuf {
        match self.os {
            Os::Windows => self.appdata.join("Code").join("User"),
            Os::MacOs => self.app_support().join("Code").join("User"),
            Os::Linux => self.xdg_config.join("Code").join("User"),
        }
    }

    fn app_support(&self) -> PathBuf {
        self.home.join("Library").join("Application Support")
    }
}

/// The MCP server RillDisk adds: an absolute executable path plus arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Server {
    pub command: PathBuf,
    pub args: Vec<String>,
}

impl Server {
    /// `<exe> mcp --root <root>...`. `rilldisk mcp` requires at least one root.
    pub fn rilldisk(exe: PathBuf, roots: &[PathBuf]) -> Self {
        let mut args = vec!["mcp".to_string()];
        for root in roots {
            args.push("--root".into());
            args.push(root.display().to_string());
        }
        Self { command: exe, args }
    }

    fn launch(&self) -> Launch {
        Launch {
            command: self.command.display().to_string(),
            args: self.args.clone(),
            env: Vec::new(),
        }
    }
}

/// The `rilldisk` CLI shipped next to `exe` (the desktop and CLI ship together).
pub fn sibling_cli(exe: &Path) -> PathBuf {
    let name = if cfg!(windows) {
        "rilldisk.exe"
    } else {
        "rilldisk"
    };
    exe.with_file_name(name)
}

/// What a client would run: read back from its config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum ClientId {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Cursor,
    VsCode,
    Windsurf,
    Gemini,
    Zed,
    OpenCode,
    Cline,
}

impl ClientId {
    pub const ALL: [ClientId; 10] = [
        ClientId::ClaudeCode,
        ClientId::ClaudeDesktop,
        ClientId::Codex,
        ClientId::Cursor,
        ClientId::VsCode,
        ClientId::Windsurf,
        ClientId::Gemini,
        ClientId::Zed,
        ClientId::OpenCode,
        ClientId::Cline,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ClientId::ClaudeCode => "Claude Code",
            ClientId::ClaudeDesktop => "Claude Desktop",
            ClientId::Codex => "Codex CLI",
            ClientId::Cursor => "Cursor",
            ClientId::VsCode => "VS Code",
            ClientId::Windsurf => "Windsurf",
            ClientId::Gemini => "Gemini CLI",
            ClientId::Zed => "Zed",
            ClientId::OpenCode => "OpenCode",
            ClientId::Cline => "Cline",
        }
    }

    /// Command-line name (`--client <key>`).
    pub fn key(self) -> &'static str {
        match self {
            ClientId::ClaudeCode => "claude-code",
            ClientId::ClaudeDesktop => "claude-desktop",
            ClientId::Codex => "codex",
            ClientId::Cursor => "cursor",
            ClientId::VsCode => "vscode",
            ClientId::Windsurf => "windsurf",
            ClientId::Gemini => "gemini",
            ClientId::Zed => "zed",
            ClientId::OpenCode => "opencode",
            ClientId::Cline => "cline",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let text = text.to_ascii_lowercase().replace(['_', ' '], "-");
        Self::ALL.into_iter().find(|c| {
            c.key() == text
                || (text == "claude" && *c == ClientId::ClaudeCode)
                || (text == "code" && *c == ClientId::VsCode)
        })
    }

    /// Executable that marks the client as installed and, for Claude Code,
    /// performs the change.
    fn program(self) -> Option<&'static str> {
        Some(match self {
            ClientId::ClaudeCode => "claude",
            ClientId::Codex => "codex",
            ClientId::Cursor => "cursor",
            ClientId::VsCode => "code",
            ClientId::Windsurf => "windsurf",
            ClientId::Gemini => "gemini",
            ClientId::Zed => "zed",
            ClientId::OpenCode => "opencode",
            ClientId::ClaudeDesktop | ClientId::Cline => return None,
        })
    }

    fn format(self) -> Format {
        match self {
            ClientId::Codex => Format::CodexToml,
            ClientId::VsCode => Format::Json("servers", Style::TypedStdio),
            ClientId::Cursor => Format::Json("mcpServers", Style::TypedStdio),
            ClientId::ClaudeCode => Format::Json("mcpServers", Style::ClaudeCode),
            ClientId::Zed => Format::Json("context_servers", Style::WithEnv),
            ClientId::OpenCode => Format::Json("mcp", Style::OpenCode),
            ClientId::ClaudeDesktop | ClientId::Windsurf | ClientId::Gemini | ClientId::Cline => {
                Format::Json("mcpServers", Style::Plain)
            }
        }
    }

    /// The user-level config file this client reads.
    pub fn config_path(self, env: &Env) -> PathBuf {
        match self {
            ClientId::ClaudeCode => env
                .claude_config_dir
                .clone()
                .unwrap_or_else(|| env.home.clone())
                .join(".claude.json"),
            ClientId::ClaudeDesktop => self.data_dir(env).join("claude_desktop_config.json"),
            ClientId::Codex => self.data_dir(env).join("config.toml"),
            ClientId::Cursor => self.data_dir(env).join("mcp.json"),
            ClientId::VsCode => self.data_dir(env).join("mcp.json"),
            ClientId::Windsurf => self.data_dir(env).join("mcp_config.json"),
            ClientId::Gemini => self.data_dir(env).join("settings.json"),
            ClientId::Zed => self.data_dir(env).join("settings.json"),
            ClientId::OpenCode => {
                let dir = self.data_dir(env);
                let jsonc = dir.join("opencode.jsonc");
                if jsonc.is_file() && !dir.join("opencode.json").is_file() {
                    jsonc
                } else {
                    dir.join("opencode.json")
                }
            }
            ClientId::Cline => self
                .data_dir(env)
                .join("settings")
                .join("cline_mcp_settings.json"),
        }
    }

    /// Folder whose presence means the client has run on this machine.
    fn data_dir(self, env: &Env) -> PathBuf {
        match self {
            ClientId::ClaudeCode => env.home.join(".claude"),
            ClientId::ClaudeDesktop => match env.os {
                Os::Windows => env.appdata.join("Claude"),
                Os::MacOs => env.app_support().join("Claude"),
                Os::Linux => env.xdg_config.join("Claude"),
            },
            ClientId::Codex => env
                .codex_home
                .clone()
                .unwrap_or_else(|| env.home.join(".codex")),
            ClientId::Cursor => env.home.join(".cursor"),
            ClientId::VsCode => env.vscode_user(),
            ClientId::Windsurf => env.home.join(".codeium").join("windsurf"),
            ClientId::Gemini => env.home.join(".gemini"),
            ClientId::Zed => match env.os {
                Os::Windows => env.appdata.join("Zed"),
                Os::MacOs => env.home.join(".config").join("zed"),
                Os::Linux => env.xdg_config.join("zed"),
            },
            ClientId::OpenCode => match env.os {
                Os::Linux => env.xdg_config.join("opencode"),
                _ => env.home.join(".config").join("opencode"),
            },
            ClientId::Cline => env
                .vscode_user()
                .join("globalStorage")
                .join("saoudrizwan.claude-dev"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Format {
    /// A JSON or JSONC object under this top-level key, one entry per server.
    Json(&'static str, Style),
    /// `[mcp_servers.<name>]` tables in TOML.
    CodexToml,
}

#[derive(Clone, Copy, Debug)]
enum Style {
    /// `{"command", "args"}`.
    Plain,
    /// `{"type": "stdio", "command", "args"}`.
    TypedStdio,
    /// `{"type": "stdio", "command", "args", "env": {}}`, as `claude mcp add` writes.
    ClaudeCode,
    /// `{"command", "args", "env": {}}`.
    WithEnv,
    /// `{"type": "local", "command": [exe, args...], "enabled": true}`.
    OpenCode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    NotInstalled,
    /// Installed, without a RillDisk entry.
    NotConfigured,
    /// The entry runs this RillDisk.
    Configured,
    /// The entry runs a RillDisk that no longer exists or is another copy.
    Stale {
        command: String,
        missing: bool,
    },
    /// The config file exists but cannot be parsed; RillDisk will not edit it.
    Invalid {
        error: String,
    },
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::NotInstalled => "not installed",
            Status::NotConfigured => "not configured",
            Status::Configured => "configured",
            Status::Stale { missing: true, .. } => "stale (missing exe)",
            Status::Stale { .. } => "stale (other exe)",
            Status::Invalid { .. } => "unreadable config",
        }
    }

    /// Whether a RillDisk entry exists (and can be tested or removed).
    pub fn has_entry(&self) -> bool {
        matches!(self, Status::Configured | Status::Stale { .. })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Detection {
    pub client: ClientId,
    pub name: &'static str,
    pub config: PathBuf,
    pub status: Status,
    /// The client's own CLI, when RillDisk uses it to make the change.
    pub cli: Option<PathBuf>,
    /// What the client would run for RillDisk, read from its config.
    pub launch: Option<Launch>,
}

/// Status of every supported client for `server`.
pub fn detect(env: &Env, server: &Server) -> Vec<Detection> {
    ClientId::ALL
        .into_iter()
        .map(|client| detect_one(env, client, server))
        .collect()
}

pub fn detect_one(env: &Env, client: ClientId, server: &Server) -> Detection {
    let config = client.config_path(env);
    let program = client.program().and_then(|p| env.find_program(p));
    let installed = program.is_some() || config.is_file() || client.data_dir(env).is_dir();
    let cli = program.filter(|_| client == ClientId::ClaudeCode);
    let mut detection = Detection {
        client,
        name: client.name(),
        config: config.clone(),
        status: Status::NotInstalled,
        cli,
        launch: None,
    };
    if !installed {
        return detection;
    }
    let text = match read(&config) {
        Ok(text) => text,
        Err(error) => {
            detection.status = Status::Invalid {
                error: format!("{error:#}"),
            };
            return detection;
        }
    };
    detection.status = match edit::read_entry(client.format(), text.as_deref().unwrap_or("")) {
        Err(error) => Status::Invalid {
            error: format!("{error:#}"),
        },
        Ok(None) => Status::NotConfigured,
        Ok(Some(launch)) => {
            let status = classify(&launch, server);
            detection.launch = Some(launch);
            status
        }
    };
    detection
}

fn classify(launch: &Launch, server: &Server) -> Status {
    let command = Path::new(&launch.command);
    if same_path(command, &server.command) && launch.args.first().map(String::as_str) == Some("mcp")
    {
        return Status::Configured;
    }
    Status::Stale {
        command: launch.command.clone(),
        missing: !command.is_file(),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| {
        let p = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let text = p.display().to_string();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    a == b || key(a) == key(b)
}

/// Reads a config file; `None` when it does not exist.
fn read(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// Add the entry or point it at this executable.
    Add,
    /// Delete the entry.
    Remove,
}

/// A previewed change. Nothing is written until [`apply`].
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub client: ClientId,
    pub change: Change,
    pub path: PathBuf,
    /// File contents when the plan was made (`None`: no file yet).
    pub before: Option<String>,
    /// File contents after the change; equal to `before` when nothing changes.
    pub after: String,
    /// Unified diff of `before` and `after`.
    pub diff: String,
    /// The client's own CLI command, when it makes the change instead of RillDisk.
    pub command: Option<Vec<String>>,
}

impl Plan {
    /// False when the config already says what the change would make it say.
    pub fn changes_anything(&self) -> bool {
        match &self.before {
            Some(before) => *before != self.after,
            None => self.change == Change::Add,
        }
    }
}

/// Prepares a change without writing anything.
pub fn plan(env: &Env, client: ClientId, server: &Server, change: Change) -> Result<Plan> {
    let detection = detect_one(env, client, server);
    if let Status::Invalid { error } = &detection.status {
        bail!("{}: {error}", client.name());
    }
    if detection.status == Status::NotInstalled && change == Change::Add {
        bail!("{} is not installed", client.name());
    }
    let path = detection.config;
    let before = read(&path)?;
    let after = match change {
        Change::Add => edit::add(client.format(), before.as_deref(), &server.launch())?,
        Change::Remove => edit::remove(client.format(), before.as_deref())?,
    };
    let diff = diff::unified(&env.display(&path), before.as_deref().unwrap_or(""), &after);
    let changed = match &before {
        Some(before) => *before != after,
        None => change == Change::Add,
    };
    let command = detection.cli.filter(|_| changed).map(|cli| {
        let mut command = vec![cli.display().to_string(), "mcp".into()];
        match change {
            Change::Add => {
                command.extend(["add", "--scope", "user", SERVER_NAME, "--"].map(String::from));
                command.push(server.command.display().to_string());
                command.extend(server.args.iter().cloned());
            }
            Change::Remove => {
                command.extend(["remove", "--scope", "user", SERVER_NAME].map(String::from))
            }
        }
        command
    });
    Ok(Plan {
        client,
        change,
        path,
        before,
        after,
        diff,
        command,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct Applied {
    /// Whether the file changed.
    pub changed: bool,
    /// Copy of the previous file, kept next to it.
    pub backup: Option<PathBuf>,
}

/// Writes a previewed change: backup, then an atomic replace (or the client's
/// own CLI). Refuses if the file changed since the preview.
pub fn apply(plan: &Plan) -> Result<Applied> {
    let current = read(&plan.path)?;
    if current != plan.before {
        bail!(
            "{} changed since the preview; preview again",
            plan.path.display()
        );
    }
    if !plan.changes_anything() {
        return Ok(Applied {
            changed: false,
            backup: None,
        });
    }
    let backup = match &current {
        Some(_) => Some(backup(&plan.path)?),
        None => None,
    };
    match &plan.command {
        Some(command) => run_client_cli(plan, command)?,
        None => write_atomic(&plan.path, &plan.after)?,
    }
    Ok(Applied {
        changed: true,
        backup,
    })
}

fn run_client_cli(plan: &Plan, command: &[String]) -> Result<()> {
    let run = |args: &[String]| -> Result<()> {
        let mut cmd = Command::new(&command[0]);
        cmd.args(args).stdin(std::process::Stdio::null());
        check::hide_window(&mut cmd);
        let output = cmd
            .output()
            .with_context(|| format!("cannot run {}", command[0]))?;
        if !output.status.success() {
            bail!(
                "{} exited with {}: {}",
                command[0],
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    };
    // `claude mcp add` refuses to overwrite an existing entry.
    let has_entry = plan
        .before
        .as_deref()
        .is_some_and(|text| matches!(edit::read_entry(plan.client.format(), text), Ok(Some(_))));
    if plan.change == Change::Add && has_entry {
        run(&["mcp", "remove", "--scope", "user", SERVER_NAME].map(String::from))?;
    }
    run(&command[1..])
}

/// Copies `path` to `<name>.rilldisk-backup-<UTC timestamp>` beside it.
fn backup(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .context("config path has no file name")?
        .to_string_lossy();
    let stamp = timestamp(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    );
    let mut target = path.with_file_name(format!("{name}.rilldisk-backup-{stamp}"));
    let mut n = 1;
    while target.exists() {
        n += 1;
        target = path.with_file_name(format!("{name}.rilldisk-backup-{stamp}-{n}"));
    }
    fs::copy(path, &target).with_context(|| format!("cannot back up {}", path.display()))?;
    Ok(target)
}

/// Writes a temporary sibling, flushes it, then renames it over `path`.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().context("config path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = dir.join(format!(".{name}.rilldisk-{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("cannot create {}", temp.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // Keep the original's permissions (for example 0600 on Unix).
        if let Ok(meta) = fs::metadata(path) {
            fs::set_permissions(&temp, meta.permissions())?;
        }
        fs::rename(&temp, path).with_context(|| format!("cannot replace {}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// `YYYYMMDD-HHMMSS` in UTC.
fn timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_calendar_dates() {
        assert_eq!(timestamp(0), "19700101-000000");
        assert_eq!(timestamp(1_793_620_800), "20261102-120000");
        assert_eq!(timestamp(951_782_400), "20000229-000000");
    }

    #[test]
    fn client_names_parse() {
        for client in ClientId::ALL {
            assert_eq!(ClientId::parse(client.key()), Some(client));
        }
        assert_eq!(ClientId::parse("Claude"), Some(ClientId::ClaudeCode));
        assert_eq!(ClientId::parse("nope"), None);
    }
}
