// SPDX-License-Identifier: AGPL-3.0-only
use rilldisk_core::{EntryKind, ScanControl, ScanOptions, scan};
use std::{fs, sync::Arc};

#[test]
fn accounts_for_nested_files_hard_links_and_unicode() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(root.path().join("nested/مرحبا.txt"), [42_u8; 8192]).unwrap();
    fs::write(root.path().join("small.txt"), [1_u8; 17]).unwrap();
    fs::write(root.path().join(".hidden"), [2_u8; 3]).unwrap();
    fs::hard_link(
        root.path().join("nested/مرحبا.txt"),
        root.path().join("alias.bin"),
    )
    .unwrap();
    let snapshot = scan(root.path(), ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete, "{:?}", snapshot.summary.issues);
    assert_eq!(snapshot.summary.files, 4);
    assert_eq!(snapshot.summary.directories, 2);
    assert_eq!(snapshot.summary.logical_bytes, 16404);
    assert_eq!(snapshot.summary.unique_logical_bytes, 8212);
    assert_eq!(snapshot.summary.hard_link_duplicates, 1);
    assert!(snapshot.summary.allocated_bytes.is_some());
    assert_eq!(snapshot.query(None, Some("مرحبا"), true, 0, 50).0, 1);
    let (total, page) = snapshot.query(Some(0), None, false, 0, 1);
    assert_eq!(total, 4);
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].logical_bytes, 8192);
    assert!(snapshot.query(None, None, true, 100, 50).1.is_empty());
    let nested = snapshot
        .entries
        .iter()
        .find(|e| e.path.ends_with("nested"))
        .unwrap();
    assert_eq!(nested.kind, EntryKind::Directory);
    assert_eq!(nested.logical_bytes, 8192);
    assert_eq!(snapshot.query(Some(nested.id), None, false, 0, 50).0, 1);
}

#[test]
fn entry_limit_and_cancellation_are_explicitly_partial() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..20 {
        fs::write(root.path().join(format!("{i}.txt")), [1_u8; 10]).unwrap();
    }
    let snapshot = scan(
        root.path(),
        ScanOptions {
            max_entries: 5,
            ..Default::default()
        },
        Arc::default(),
    )
    .unwrap();
    assert!(!snapshot.summary.complete);
    assert!(snapshot.summary.issue_count > 0);
    assert_eq!(snapshot.entries.len(), 5);
    let control = Arc::new(ScanControl::default());
    control.cancel();
    let snapshot = scan(root.path(), ScanOptions::default(), control).unwrap();
    assert!(snapshot.summary.cancelled);
    assert!(!snapshot.summary.complete);
}

#[test]
fn rejects_missing_or_file_roots_and_invalid_options() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), b"data").unwrap();
    assert!(
        scan(
            &root.path().join("missing"),
            ScanOptions::default(),
            Arc::default()
        )
        .is_err()
    );
    assert!(
        scan(
            &root.path().join("file"),
            ScanOptions::default(),
            Arc::default()
        )
        .is_err()
    );
    assert!(
        scan(
            root.path(),
            ScanOptions {
                threads: 0,
                ..Default::default()
            },
            Arc::default()
        )
        .is_err()
    );
}

#[test]
fn does_not_follow_directory_links_or_junctions() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("must-not-be-scanned"), [1_u8; 100]).unwrap();
    let link = root.path().join("outside");
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    let snapshot = scan(root.path(), ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete, "{:?}", snapshot.summary.issues);
    assert_eq!(snapshot.summary.skipped_links, 1);
    assert_eq!(snapshot.summary.files, 0);
    assert_eq!(snapshot.summary.logical_bytes, 0);
}

#[test]
fn enumerates_multiple_native_buffers_and_paginates_in_size_order() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..1500 {
        fs::write(root.path().join(format!("file-{i:04}.bin")), vec![0_u8; i]).unwrap();
    }
    let snapshot = scan(root.path(), ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete, "{:?}", snapshot.summary.issues);
    assert_eq!(snapshot.summary.files, 1500);
    assert_eq!(snapshot.summary.logical_bytes, 1_124_250);
    let (total, page) = snapshot.query(None, None, true, 499, 2);
    assert_eq!(total, 1500);
    assert_eq!(page[0].logical_bytes, 1000);
    assert_eq!(page[1].logical_bytes, 999);
}
