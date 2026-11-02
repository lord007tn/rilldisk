// SPDX-License-Identifier: AGPL-3.0-only
use std::{fs, process::Command};

#[test]
fn cli_json_jsonl_and_partial_exit_codes() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("sample.txt"), b"hello world").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rilldisk"))
        .arg("scan")
        .arg(root.path())
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["summary"]["logical_bytes"], 11);
    assert_eq!(result["summary"]["complete"], true);
    let output = Command::new(env!("CARGO_BIN_EXE_rilldisk"))
        .arg("scan")
        .arg(root.path())
        .args(["--format", "jsonl", "--max-entries", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let lines = String::from_utf8(output.stdout).unwrap();
    let first: serde_json::Value = serde_json::from_str(lines.lines().next().unwrap()).unwrap();
    assert_eq!(first["type"], "summary");
    assert_eq!(first["data"]["complete"], false);
    let table = Command::new(env!("CARGO_BIN_EXE_rilldisk"))
        .arg("scan")
        .arg(root.path())
        .args(["--max-entries", "1"])
        .output()
        .unwrap();
    assert_eq!(table.status.code(), Some(2));
    assert!(
        String::from_utf8(table.stdout)
            .unwrap()
            .contains("PARTIAL:")
    );
}

#[test]
fn cli_never_overwrites_an_existing_report() {
    let root = tempfile::tempdir().unwrap();
    let output_path = root.path().join("existing.json");
    fs::write(&output_path, "preserve me").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rilldisk"))
        .arg("scan")
        .arg(root.path())
        .arg("--output")
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(output_path).unwrap(), "preserve me");
}

#[test]
fn broken_pipe_preserves_partial_scan_exit_status() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file.txt"), b"data").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rilldisk"))
        .arg("scan")
        .arg(root.path())
        .args(["--format", "json", "--max-entries", "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    assert_eq!(child.wait().unwrap().code(), Some(2));
}

fn cleanup_fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let write = |path: &str, bytes: usize| {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1_u8; bytes]).unwrap();
    };
    write("app/Cargo.toml", 10);
    write("app/target/debug/app.bin", 40_000);
    write("web/package.json", 10);
    write("web/node_modules/a/index.js", 20_000);
    write("wt/feature/.git", 40);
    write("wt/feature/notes.md", 500);
    root
}

fn reclaim_delete(root: &std::path::Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rilldisk"))
        .arg("reclaim")
        .arg(root)
        .args(["--min-size", "0", "--delete"])
        .args(extra)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn reclaim_delete_refuses_without_confirmation_when_not_interactive() {
    let root = cleanup_fixture();
    let output = reclaim_delete(root.path(), &["--permanent"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("without confirmation"), "{stderr}");
    assert!(root.path().join("app/target/debug/app.bin").exists());
    assert!(root.path().join("web/node_modules/a/index.js").exists());
    // --permanent and --yes only make sense with --delete.
    let output = Command::new(env!("CARGO_BIN_EXE_rilldisk"))
        .arg("reclaim")
        .arg(root.path())
        .args(["--permanent", "--yes"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(root.path().join("app/target").exists());
}

#[test]
fn reclaim_delete_removes_safe_candidates_and_keeps_review_items() {
    let root = cleanup_fixture();
    let output = reclaim_delete(root.path(), &["--permanent", "--yes", "--format", "json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["mode"], "permanent");
    assert_eq!(report["removed"], 2);
    assert_eq!(report["failed"], 0);
    assert!(report["freed_bytes"].as_u64().unwrap() >= 60_000);
    assert!(
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["status"] == "removed")
    );
    assert!(!root.path().join("app/target").exists());
    assert!(!root.path().join("web/node_modules").exists());
    assert!(root.path().join("app/Cargo.toml").exists());
    // The linked worktree is "review first" and was not selected.
    assert!(root.path().join("wt/feature/notes.md").exists());

    // Named explicitly, it is removed and the prune follow-up is reported.
    let output = reclaim_delete(root.path(), &["--safety", "review", "--permanent", "--yes"]);
    assert_eq!(output.status.code(), Some(0));
    let table = String::from_utf8(output.stdout).unwrap();
    assert!(table.contains("Deleted 1 folders"), "{table}");
    assert!(table.contains("git worktree prune"), "{table}");
    assert!(!root.path().join("wt/feature").exists());
}
