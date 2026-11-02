// SPDX-License-Identifier: AGPL-3.0-only
//! `rilldisk mcp setup|doctor` and the connection check against the real
//! binary. Every run gets a temporary home, app-data and PATH, and the
//! CLAUDE_CONFIG_DIR / CODEX_HOME overrides are removed, so this machine's
//! client configs are never read or written.
use rilldisk_mcp_setup::{CheckError, Launch, check};
use std::{fs, path::Path, process::Command, time::Duration};

const EXE: &str = env!("CARGO_BIN_EXE_rilldisk");

fn launch(command: &str, args: &[&str]) -> Launch {
    Launch {
        command: command.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: Vec::new(),
    }
}

#[test]
fn handshake_with_the_built_server_lists_its_tools() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().display().to_string();
    let result = check(
        &launch(EXE, &["mcp", "--root", &root]),
        Duration::from_secs(20),
    )
    .unwrap();
    for tool in [
        "allowed_roots",
        "start_scan",
        "scan_status",
        "query_entries",
    ] {
        assert!(result.tools.iter().any(|t| t == tool), "{:?}", result.tools);
    }
    assert!(result.server.starts_with("rilldisk"), "{}", result.server);
    assert!(!result.protocol_version.is_empty());
}

#[test]
fn check_reports_missing_crashed_protocol_and_timeout() {
    let gone = tempfile::tempdir().unwrap().path().join("rilldisk.exe");
    assert!(matches!(
        check(
            &launch(&gone.display().to_string(), &["mcp"]),
            Duration::from_secs(5)
        ),
        Err(CheckError::Missing { .. })
    ));
    // `rilldisk mcp` without a root exits with a usage error on stderr.
    match check(&launch(EXE, &["mcp"]), Duration::from_secs(20)) {
        Err(CheckError::Exited { code, stderr }) => {
            assert_eq!(code, Some(2));
            assert!(stderr.contains("--root"), "{stderr}");
        }
        other => panic!("{other:?}"),
    }
    // Something that prints a table is not an MCP server.
    assert!(matches!(
        check(&launch(EXE, &["drives"]), Duration::from_secs(20)),
        Err(CheckError::Protocol { .. })
    ));
    // A process that never answers.
    let silent = if cfg!(windows) {
        launch(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep 30",
            ],
        )
    } else {
        launch("sleep", &["30"])
    };
    let started = std::time::Instant::now();
    assert!(matches!(
        check(&silent, Duration::from_secs(1)),
        Err(CheckError::Timeout { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(15));
}

/// The CLI with home, app data and PATH inside `home`.
fn cli(home: &Path, path: &Path, args: &[&str]) -> std::process::Output {
    Command::new(EXE)
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("AppData").join("Roaming"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", path)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .output()
        .unwrap()
}

fn text(output: &std::process::Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const CODEX: &str =
    "# my codex settings\nmodel = \"gpt-5.5\"\n\n[mcp_servers.other]\ncommand = \"other-mcp\"\n";

#[test]
fn setup_previews_applies_checks_and_removes() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let bin = temp.path().join("empty-bin");
    fs::create_dir_all(home.join(".cursor")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let codex = home.join(".codex").join("config.toml");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(&codex, CODEX).unwrap();
    let cursor = home.join(".cursor").join("mcp.json");

    // Without a choice it lists what it found and changes nothing.
    let listed = cli(&home, &bin, &["mcp", "setup"]);
    assert!(!listed.status.success());
    assert!(text(&listed).contains("cursor"), "{}", text(&listed));

    let dry = cli(
        &home,
        &bin,
        &[
            "mcp",
            "setup",
            "--client",
            "cursor",
            "--client",
            "codex",
            "--dry-run",
        ],
    );
    assert!(dry.status.success(), "{}", text(&dry));
    let out = String::from_utf8_lossy(&dry.stdout);
    assert!(out.contains("+[mcp_servers.rilldisk]"), "{out}");
    assert!(out.contains("+    \"rilldisk\": {"), "{out}");
    assert!(out.contains("Dry run: nothing was written."), "{out}");
    assert_eq!(fs::read_to_string(&codex).unwrap(), CODEX);
    assert!(!cursor.exists());

    // Without a terminal and without --yes it refuses.
    let refused = cli(&home, &bin, &["mcp", "setup", "--client", "cursor"]);
    assert!(!refused.status.success());
    assert!(!cursor.exists());

    let applied = cli(
        &home,
        &bin,
        &[
            "mcp", "setup", "--client", "cursor", "--client", "codex", "--yes",
        ],
    );
    assert!(applied.status.success(), "{}", text(&applied));
    let out = String::from_utf8_lossy(&applied.stdout);
    assert_eq!(out.matches("Connection: ok").count(), 2, "{out}");
    let toml = fs::read_to_string(&codex).unwrap();
    assert!(toml.starts_with(CODEX.trim_end()), "{toml}");
    assert!(toml.contains("[mcp_servers.rilldisk]"), "{toml}");
    let backups: Vec<_> = fs::read_dir(codex.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .contains(".rilldisk-backup-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read_to_string(backups[0].path()).unwrap(), CODEX);
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cursor).unwrap()).unwrap();
    assert_eq!(json["mcpServers"]["rilldisk"]["args"][0], "mcp");
    assert_eq!(json["mcpServers"]["rilldisk"]["args"][1], "--root");

    // Running it again is a no-op.
    let again = cli(
        &home,
        &bin,
        &["mcp", "setup", "--client", "cursor", "--yes"],
    );
    assert!(
        text(&again).contains("already configured"),
        "{}",
        text(&again)
    );

    let doctor = cli(&home, &bin, &["mcp", "doctor", "--format", "json"]);
    assert!(doctor.status.success(), "{}", text(&doctor));
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let clients = report["clients"].as_array().unwrap();
    let row = |key: &str| clients.iter().find(|c| c["client"] == key).unwrap().clone();
    assert_eq!(row("cursor")["status"]["state"], "configured");
    assert_eq!(row("cursor")["check"]["ok"], true);
    assert!(row("codex")["check"]["tools"].as_array().unwrap().len() >= 5);
    assert_eq!(row("zed")["status"]["state"], "not_installed");

    let table = cli(&home, &bin, &["mcp", "doctor"]);
    assert!(text(&table).contains("ok · "), "{}", text(&table));

    let removed = cli(&home, &bin, &["mcp", "setup", "--all", "--remove", "--yes"]);
    assert!(removed.status.success(), "{}", text(&removed));
    assert_eq!(fs::read_to_string(&codex).unwrap(), CODEX);
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cursor).unwrap()).unwrap();
    assert!(json["mcpServers"].get("rilldisk").is_none());
}

#[test]
fn doctor_fails_when_an_entry_points_at_a_missing_exe() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let gemini = home.join(".gemini").join("settings.json");
    fs::create_dir_all(gemini.parent().unwrap()).unwrap();
    let gone = temp.path().join("old").join("rilldisk.exe");
    fs::write(
        &gemini,
        serde_json::json!({"mcpServers": {"rilldisk": {"command": gone, "args": ["mcp"]}}})
            .to_string(),
    )
    .unwrap();
    let doctor = cli(&home, temp.path(), &["mcp", "doctor"]);
    assert_eq!(doctor.status.code(), Some(1), "{}", text(&doctor));
    let out = text(&doctor);
    assert!(out.contains("stale (missing exe)"), "{out}");
    assert!(out.contains("FAILED: executable not found"), "{out}");
    assert!(out.contains("--client gemini"), "{out}");
}

/// When `claude` is on PATH, Claude Code's own CLI makes the change.
#[test]
fn claude_code_uses_its_own_cli_when_present() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let calls = bin.join("calls.txt");
    if cfg!(windows) {
        fs::write(bin.join("claude.cmd"), "@echo %*>>\"%~dp0calls.txt\"\r\n").unwrap();
    } else {
        let script = bin.join("claude");
        fs::write(
            &script,
            "#!/bin/sh\necho \"$@\" >> \"$(dirname \"$0\")/calls.txt\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let dry = cli(
        &home,
        &bin,
        &["mcp", "setup", "--client", "claude-code", "--dry-run"],
    );
    assert!(dry.status.success(), "{}", text(&dry));
    assert!(text(&dry).contains("Runs: "), "{}", text(&dry));
    assert!(!calls.exists(), "a dry run never starts the client CLI");

    let run = cli(
        &home,
        &bin,
        &["mcp", "setup", "--client", "claude-code", "--yes"],
    );
    let recorded = fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        recorded.contains("mcp add --scope user rilldisk -- ") && recorded.contains(" mcp --root "),
        "{recorded}\n{}",
        text(&run)
    );
    // The fake CLI writes nothing, so the follow-up check notices.
    assert!(
        text(&run).contains("Entry not found after the change"),
        "{}",
        text(&run)
    );
}
