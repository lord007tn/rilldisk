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
