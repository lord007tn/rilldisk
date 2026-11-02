// SPDX-License-Identifier: AGPL-3.0-only
//! Config merges against fixture files in temporary home folders. Nothing here
//! reads or writes this machine's real client configs: every `Env` is isolated.
use rilldisk_mcp_setup::{Change, ClientId, Env, Server, Status, apply, detect, detect_one, plan};
use std::{fs, path::Path, path::PathBuf};

fn fake_exe(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::create_dir_all(dir).unwrap();
    fs::write(&path, b"").unwrap();
    path
}

struct Fixture {
    _temp: tempfile::TempDir,
    env: Env,
    server: Server,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let env = Env::isolated(&home);
    let exe = fake_exe(&temp.path().join("bin"), "rilldisk.exe");
    let server = Server::rilldisk(exe, std::slice::from_ref(&home));
    Fixture {
        _temp: temp,
        env,
        server,
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Connect, check the result, connect again (no change), then remove.
fn round_trip(client: ClientId, original: &str, must_keep: &[&str]) -> (String, String) {
    let f = fixture();
    let path = client.config_path(&f.env);
    write(&path, original);
    assert_eq!(
        detect_one(&f.env, client, &f.server).status,
        Status::NotConfigured
    );

    let preview = plan(&f.env, client, &f.server, Change::Add).unwrap();
    assert!(preview.changes_anything());
    assert!(preview.diff.contains("+"), "{}", preview.diff);
    // A preview writes nothing.
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let applied = apply(&preview).unwrap();
    assert!(applied.changed);
    let backup = applied.backup.expect("backup of the existing file");
    assert_eq!(fs::read_to_string(&backup).unwrap(), original);
    assert_eq!(backup.parent(), path.parent());
    let added = fs::read_to_string(&path).unwrap();
    for keep in must_keep {
        assert!(added.contains(keep), "lost {keep:?}:\n{added}");
    }
    let detection = detect_one(&f.env, client, &f.server);
    assert_eq!(detection.status, Status::Configured, "{added}");
    let launch = detection.launch.unwrap();
    assert_eq!(launch.command, f.server.command.display().to_string());
    assert_eq!(launch.args, f.server.args);

    // Idempotent: a second connect changes nothing and makes no backup.
    let again = plan(&f.env, client, &f.server, Change::Add).unwrap();
    assert!(!again.changes_anything(), "{}", again.diff);
    assert!(again.diff.is_empty());
    assert!(!apply(&again).unwrap().changed);

    let removal = plan(&f.env, client, &f.server, Change::Remove).unwrap();
    apply(&removal).unwrap();
    let removed = fs::read_to_string(&path).unwrap();
    assert_eq!(
        detect_one(&f.env, client, &f.server).status,
        Status::NotConfigured
    );
    for keep in must_keep {
        assert!(removed.contains(keep), "lost {keep:?}:\n{removed}");
    }
    // No temporary files are left behind.
    let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    (added, removed)
}

const JSONC: &str = r#"{
    // Personal settings: keep this comment.
    "editor.fontSize": 14,
    "mcpServers": {
        "github": {
            "command": "gh-mcp",
            "args": ["--read-only"] // trailing comment
        }
    },
    "theme": "dark",
}
"#;

#[test]
fn claude_desktop_keeps_comments_other_servers_and_restores_on_remove() {
    let (added, removed) = round_trip(
        ClientId::ClaudeDesktop,
        JSONC,
        &[
            "// Personal settings: keep this comment.",
            "// trailing comment",
            "\"github\"",
            "\"theme\": \"dark\",",
        ],
    );
    // Indentation of the file (four spaces) is reused.
    assert!(
        added.contains("\n        \"rilldisk\": {\n            \"command\""),
        "{added}"
    );
    assert_eq!(removed, JSONC, "remove restores the original text");
}

#[test]
fn every_json_client_round_trips() {
    for (client, key) in [
        (ClientId::ClaudeCode, "mcpServers"),
        (ClientId::Cursor, "mcpServers"),
        (ClientId::Windsurf, "mcpServers"),
        (ClientId::Gemini, "mcpServers"),
        (ClientId::Cline, "mcpServers"),
        (ClientId::VsCode, "servers"),
        (ClientId::Zed, "context_servers"),
        (ClientId::OpenCode, "mcp"),
    ] {
        let original = format!(
            "{{\n  \"other\": {{\"keep\": true}},\n  \"{key}\": {{\n    \"existing\": {{\"command\": \"x\"}}\n  }}\n}}\n"
        );
        let (_, removed) = round_trip(client, &original, &["\"keep\": true", "\"existing\""]);
        assert_eq!(removed, original, "{client:?}");
    }
}

#[test]
fn entries_use_each_clients_documented_shape() {
    let f = fixture();
    let exe = f.server.command.display().to_string();
    let shape = |client: ClientId| -> serde_json::Value {
        let p = plan(&f.env, client, &f.server, Change::Add);
        // Not installed yet: create its folder so it counts as installed.
        let p = p.unwrap_or_else(|_| {
            fs::create_dir_all(client.config_path(&f.env).parent().unwrap()).unwrap();
            fs::create_dir_all(f.env.home.join(".claude")).unwrap();
            plan(&f.env, client, &f.server, Change::Add).unwrap()
        });
        let value: serde_json::Value = serde_json::from_str(&p.after).unwrap();
        value
    };
    let args = serde_json::json!(f.server.args);
    let vscode = shape(ClientId::VsCode);
    assert_eq!(vscode["servers"]["rilldisk"]["type"], "stdio");
    assert_eq!(vscode["servers"]["rilldisk"]["command"], exe.as_str());
    let claude = shape(ClientId::ClaudeCode);
    assert_eq!(
        claude["mcpServers"]["rilldisk"],
        serde_json::json!({"type": "stdio", "command": exe, "args": args, "env": {}})
    );
    let desktop = shape(ClientId::ClaudeDesktop);
    assert_eq!(
        desktop["mcpServers"]["rilldisk"],
        serde_json::json!({"command": exe, "args": args})
    );
    let zed = shape(ClientId::Zed);
    assert_eq!(
        zed["context_servers"]["rilldisk"]["env"],
        serde_json::json!({})
    );
    let opencode = shape(ClientId::OpenCode);
    let mut command = vec![exe.clone()];
    command.extend(f.server.args.iter().cloned());
    assert_eq!(
        opencode["mcp"]["rilldisk"],
        serde_json::json!({"type": "local", "command": command, "enabled": true})
    );
    // A new file is plain, valid JSON ending in a newline.
    assert!(desktop.is_object());
}

const CODEX: &str = r#"# Codex settings: keep this comment.
model = "gpt-5.5"
approval_policy = "on-request"

[mcp_servers.context7]
command = "npx"
args = ["-y", "@upstash/context7-mcp"]

# Profiles below.
[profiles.fast]
model = "gpt-5.5-mini"
"#;

#[test]
fn codex_toml_keeps_formatting_comments_and_other_tables() {
    let (added, removed) = round_trip(
        ClientId::Codex,
        CODEX,
        &[
            "# Codex settings: keep this comment.",
            "[mcp_servers.context7]",
            "# Profiles below.",
            "[profiles.fast]",
        ],
    );
    assert!(
        added.contains("[mcp_servers.rilldisk]\ncommand = "),
        "{added}"
    );
    assert_eq!(removed, CODEX);
}

#[test]
fn codex_reconnect_keeps_user_options() {
    let f = fixture();
    let path = ClientId::Codex.config_path(&f.env);
    write(
        &path,
        "[mcp_servers.rilldisk]\ncommand = \"C:/old/rilldisk.exe\" # old copy\nargs = [\"mcp\"]\nstartup_timeout_sec = 30\n",
    );
    let status = detect_one(&f.env, ClientId::Codex, &f.server).status;
    assert!(
        matches!(status, Status::Stale { missing: true, .. }),
        "{status:?}"
    );
    apply(&plan(&f.env, ClientId::Codex, &f.server, Change::Add).unwrap()).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("startup_timeout_sec = 30"), "{text}");
    assert!(text.contains("# old copy"), "{text}");
    assert_eq!(
        detect_one(&f.env, ClientId::Codex, &f.server).status,
        Status::Configured
    );
}

#[test]
fn json_reconnect_keeps_user_env_and_extra_keys() {
    let f = fixture();
    let path = ClientId::Gemini.config_path(&f.env);
    write(
        &path,
        r#"{"mcpServers": {"rilldisk": {"command": "/gone/rilldisk", "args": ["mcp"], "env": {"RUST_LOG": "info"}, "trust": false}}}"#,
    );
    apply(&plan(&f.env, ClientId::Gemini, &f.server, Change::Add).unwrap()).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let entry = &value["mcpServers"]["rilldisk"];
    assert_eq!(entry["env"]["RUST_LOG"], "info");
    assert_eq!(entry["trust"], false);
    assert_eq!(entry["command"], f.server.command.display().to_string());
}

#[test]
fn stale_paths_are_detected() {
    let f = fixture();
    let other = fake_exe(&f.env.home.join("old"), "rilldisk.exe");
    let path = ClientId::Cursor.config_path(&f.env);
    let entry = |command: &Path| {
        serde_json::json!({"mcpServers": {"rilldisk": {"command": command, "args": ["mcp"]}}})
            .to_string()
    };
    write(&path, &entry(&other));
    assert_eq!(
        detect_one(&f.env, ClientId::Cursor, &f.server).status,
        Status::Stale {
            command: other.display().to_string(),
            missing: false
        }
    );
    let gone = f.env.home.join("gone").join("rilldisk.exe");
    write(&path, &entry(&gone));
    assert!(matches!(
        detect_one(&f.env, ClientId::Cursor, &f.server).status,
        Status::Stale { missing: true, .. }
    ));
    write(&path, &entry(&f.server.command));
    assert_eq!(
        detect_one(&f.env, ClientId::Cursor, &f.server).status,
        Status::Configured
    );
}

#[test]
fn not_installed_clients_are_left_alone() {
    let f = fixture();
    for detection in detect(&f.env, &f.server) {
        assert_eq!(
            detection.status,
            Status::NotInstalled,
            "{:?}",
            detection.client
        );
    }
    assert!(plan(&f.env, ClientId::Cursor, &f.server, Change::Add).is_err());
    assert!(!ClientId::Cursor.config_path(&f.env).exists());
}

#[test]
fn installed_client_without_config_gets_a_new_file_and_no_backup() {
    let f = fixture();
    fs::create_dir_all(f.env.home.join(".cursor")).unwrap();
    let preview = plan(&f.env, ClientId::Cursor, &f.server, Change::Add).unwrap();
    assert!(preview.before.is_none());
    let applied = apply(&preview).unwrap();
    assert!(applied.changed && applied.backup.is_none());
    let text = fs::read_to_string(ClientId::Cursor.config_path(&f.env)).unwrap();
    assert!(text.ends_with('\n'), "{text}");
    assert_eq!(
        detect_one(&f.env, ClientId::Cursor, &f.server).status,
        Status::Configured
    );
}

#[test]
fn unreadable_configs_are_reported_and_never_edited() {
    let f = fixture();
    let path = ClientId::Windsurf.config_path(&f.env);
    write(&path, "{ not json");
    assert!(matches!(
        detect_one(&f.env, ClientId::Windsurf, &f.server).status,
        Status::Invalid { .. }
    ));
    assert!(plan(&f.env, ClientId::Windsurf, &f.server, Change::Add).is_err());
    write(&path, "[1, 2]");
    assert!(plan(&f.env, ClientId::Windsurf, &f.server, Change::Add).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "[1, 2]");
}

#[test]
fn apply_refuses_when_the_file_changed_after_the_preview() {
    let f = fixture();
    let path = ClientId::Gemini.config_path(&f.env);
    write(&path, "{}\n");
    let preview = plan(&f.env, ClientId::Gemini, &f.server, Change::Add).unwrap();
    write(&path, "{\"theme\": \"light\"}\n");
    assert!(apply(&preview).is_err());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{\"theme\": \"light\"}\n"
    );
}

#[test]
fn claude_config_dir_and_codex_home_are_honoured() {
    let mut f = fixture();
    let claude = f.env.home.join("claude-alt");
    let codex = f.env.home.join("codex-alt");
    f.env.claude_config_dir = Some(claude.clone());
    f.env.codex_home = Some(codex.clone());
    assert_eq!(
        ClientId::ClaudeCode.config_path(&f.env),
        claude.join(".claude.json")
    );
    assert_eq!(
        ClientId::Codex.config_path(&f.env),
        codex.join("config.toml")
    );
}

#[test]
fn windows_line_endings_survive_add_and_remove() {
    let original = JSONC.replace('\n', "\r\n");
    let (added, removed) = round_trip(ClientId::Cursor, &original, &["\"github\""]);
    assert_eq!(
        added.matches('\n').count(),
        added.matches("\r\n").count(),
        "{added:?}"
    );
    assert_eq!(removed, original);
    let codex = CODEX.replace('\n', "\r\n");
    let (added, removed) = round_trip(ClientId::Codex, &codex, &["[profiles.fast]"]);
    assert_eq!(
        added.matches('\n').count(),
        added.matches("\r\n").count(),
        "{added:?}"
    );
    assert_eq!(removed, codex);
}
