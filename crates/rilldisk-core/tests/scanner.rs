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
        .find(|e| snapshot.path(e.id).ends_with("nested"))
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
    // The memory bound is reported once, not once per truncated directory.
    assert_eq!(snapshot.summary.issue_count, 1);
    assert!(
        snapshot.summary.issues[0]
            .message
            .contains("entry limit of 5")
    );
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

#[cfg(windows)]
#[test]
fn resident_ntfs_data_has_no_separate_allocated_clusters() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::ERROR_HANDLE_EOF,
        System::{IO::DeviceIoControl, Ioctl::FSCTL_GET_RETRIEVAL_POINTERS},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("resident.txt");
    fs::write(&path, [1_u8; 17]).unwrap();
    let file = fs::File::open(&path).unwrap();
    let start_vcn = 0_i64;
    let mut output = [0_u64; 128];
    let mut returned = 0;
    // SAFETY: all buffers and the handle live through this synchronous metadata query.
    let success = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_GET_RETRIEVAL_POINTERS,
            (&start_vcn as *const i64).cast(),
            8,
            output.as_mut_ptr().cast(),
            1024,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    // Independent NTFS extent query: ERROR_HANDLE_EOF means this resident file has no extents.
    assert_eq!(success, 0);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(ERROR_HANDLE_EOF as i32)
    );
    let snapshot = scan(root.path(), ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete);
    assert_eq!(snapshot.summary.logical_bytes, 17);
    assert_eq!(snapshot.summary.allocated_bytes, Some(0));
}

#[test]
fn exports_non_unicode_filenames_as_lossy_text_without_losing_the_inventory() {
    let root = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let name = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[98, 97, 100, 45, 0xD800])
    };
    #[cfg(unix)]
    let name = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![98, 97, 100, 45, 255])
    };
    fs::write(root.path().join(name), b"data").unwrap();
    let snapshot = scan(root.path(), ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete);
    assert_eq!(snapshot.summary.files, 1);
    let json = serde_json::to_value(&snapshot).unwrap();
    assert!(
        json["entries"][1]["path"]
            .as_str()
            .unwrap()
            .ends_with("bad-\u{fffd}")
    );
    assert_eq!(json["summary"]["logical_bytes"], 4);
}

#[test]
fn parallel_directories_share_one_record_budget() {
    let root = tempfile::tempdir().unwrap();
    for directory in 0..32 {
        let path = root.path().join(format!("dir-{directory}"));
        fs::create_dir(&path).unwrap();
        for file in 0..50 {
            fs::write(path.join(format!("file-{file}")), b"data").unwrap();
        }
    }
    let control = Arc::new(ScanControl::default());
    let snapshot = scan(
        root.path(),
        ScanOptions {
            threads: 8,
            max_entries: 100,
            ..Default::default()
        },
        control.clone(),
    )
    .unwrap();
    assert!(!snapshot.summary.complete);
    assert_eq!(snapshot.entries.len(), 100);
    assert_eq!(snapshot.summary.directories, 33);
    assert_eq!(snapshot.summary.files, 67);
    assert_eq!(control.files.load(std::sync::atomic::Ordering::Relaxed), 67);
}

#[test]
fn repeated_small_scans_finish_promptly() {
    // Guards the completion handshake between workers and the coordinator: a lost
    // wake-up would make a tiny scan wait for the stall timeout.
    let root = tempfile::tempdir().unwrap();
    for d in 0..8 {
        fs::create_dir_all(root.path().join(format!("d{d}/e"))).unwrap();
        fs::write(root.path().join(format!("d{d}/e/f.txt")), b"x").unwrap();
    }
    for _ in 0..200 {
        let start = std::time::Instant::now();
        let snapshot = scan(root.path(), ScanOptions::default(), Arc::default()).unwrap();
        assert_eq!(snapshot.summary.files, 8);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }
}

#[test]
fn an_exactly_spent_entry_budget_is_still_complete() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("empty")).unwrap();
    let snapshot = scan(
        root.path(),
        ScanOptions {
            max_entries: 2,
            ..Default::default()
        },
        Arc::default(),
    )
    .unwrap();
    assert!(snapshot.summary.complete, "{:?}", snapshot.summary.issues);
    assert_eq!(snapshot.entries.len(), 2);
}

#[cfg(windows)]
#[test]
fn names_ending_in_a_dot_or_space_are_read_exactly() {
    let root = tempfile::tempdir().unwrap();
    let verbatim = root.path().canonicalize().unwrap();
    for name in ["trail.", "space "] {
        let dir = verbatim.join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("inside.bin"), [1_u8; 5000]).unwrap();
    }
    let snapshot = scan(root.path(), ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete, "{:?}", snapshot.summary.issues);
    assert_eq!(snapshot.summary.files, 2);
    assert_eq!(snapshot.summary.logical_bytes, 10_000);
    assert!(!snapshot.summary.root.to_string_lossy().starts_with(r"\\?\"));
}

#[cfg(unix)]
#[test]
fn unix_reports_block_allocation_times_and_unreadable_folders() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data.bin");
    fs::write(&data, vec![1_u8; 10_000]).unwrap();
    // A sparse file: a large length with almost nothing allocated.
    let sparse = root.path().join("sparse.bin");
    fs::File::create(&sparse).unwrap().set_len(4 << 20).unwrap();
    let locked = root.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("inside.txt"), b"x").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // Root (some containers) can still list it; the assertions adapt.
    let unreadable = fs::read_dir(&locked).is_err();
    let snapshot = scan(root.path(), ScanOptions::default(), Arc::default());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let snapshot = snapshot.unwrap();
    let blocks = |path: &std::path::Path| fs::metadata(path).unwrap().blocks() * 512;
    let find = |name: &str| {
        snapshot
            .entries
            .iter()
            .find(|e| snapshot.name(e.id) == name)
            .unwrap()
    };
    let dense = find("data.bin");
    assert_eq!(dense.logical_bytes, 10_000);
    assert_eq!(dense.allocated_bytes, Some(blocks(&data)));
    let modified = fs::metadata(&data).unwrap().modified().unwrap();
    let expected = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert_eq!(dense.modified_unix_ms, Some(expected));
    let hole = find("sparse.bin");
    assert_eq!(hole.logical_bytes, 4 << 20);
    assert_eq!(hole.allocated_bytes, Some(blocks(&sparse)));
    assert!(hole.allocated_bytes.unwrap() < hole.logical_bytes);
    assert_eq!(
        snapshot.summary.unique_logical_bytes,
        10_000 + (4 << 20) + u64::from(!unreadable)
    );
    let folder = find("locked");
    if unreadable {
        assert!(!snapshot.summary.complete);
        assert_eq!(snapshot.summary.issue_count, 1);
        assert!(snapshot.summary.issues[0].path.ends_with("locked"));
        assert!(!folder.fully_listed);
    } else {
        assert!(snapshot.summary.complete);
    }
    assert_eq!(snapshot.summary.skipped_mounts, 0);
}

/// Scanning `/` must record, but never enter, virtual filesystems or other disks.
/// The entry limit keeps this to the top of the tree.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn root_scans_skip_virtual_and_other_filesystems() {
    let expected: &[&str] = if cfg!(target_os = "linux") {
        &["proc", "sys", "dev"]
    } else {
        &["dev", "Volumes"]
    };
    for cross_filesystems in [false, true] {
        let snapshot = scan(
            std::path::Path::new("/"),
            ScanOptions {
                max_entries: 300,
                cross_filesystems,
                ..Default::default()
            },
            Arc::default(),
        )
        .unwrap();
        assert!(snapshot.summary.skipped_mounts >= expected.len() as u64);
        for name in expected {
            let &id = snapshot
                .children(0)
                .iter()
                .find(|&&c| snapshot.name(c as usize) == *name)
                .unwrap_or_else(|| panic!("/{name} is listed"));
            let entry = &snapshot.entries[id as usize];
            assert_eq!(entry.kind, EntryKind::Directory);
            assert!(!entry.fully_listed, "/{name} is not entered");
            assert!(snapshot.children(id as usize).is_empty(), "/{name}");
        }
    }
}
