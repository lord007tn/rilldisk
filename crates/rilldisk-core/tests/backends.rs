// SPDX-License-Identifier: AGPL-3.0-only
//! Every backend must produce the same accounting for the same tree.
use rilldisk_core::{Backend, EntryKind, ScanControl, ScanOptions, Snapshot, scan};
use std::{collections::BTreeMap, fs, path::Path, sync::Arc};

fn scan_with(root: &Path, backend: Backend) -> Snapshot {
    scan(
        root,
        ScanOptions {
            backend,
            ..Default::default()
        },
        Arc::default(),
    )
    .unwrap()
}

/// Totals that must not depend on the backend.
fn totals(s: &Snapshot) -> [u64; 9] {
    let m = &s.summary;
    [
        m.files,
        m.directories,
        m.logical_bytes,
        m.unique_logical_bytes,
        m.allocated_bytes.unwrap_or(u64::MAX),
        m.hard_link_duplicates,
        m.skipped_links,
        m.skipped_mounts,
        m.issue_count,
    ]
}

/// Path -> (kind, logical bytes, file count). Which hard link counts as the
/// first one depends on walk order, so unique bytes are compared as totals only.
fn inventory(s: &Snapshot) -> BTreeMap<String, (EntryKind, u64, u64)> {
    s.entries
        .iter()
        .map(|e| {
            (
                s.path(e.id).to_string_lossy().into_owned(),
                (e.kind, e.logical_bytes, e.file_count),
            )
        })
        .collect()
}

fn tree() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let p = root.path();
    for d in 0..40 {
        let dir = p.join(format!("pkg-{d}")).join("lib").join("deep");
        fs::create_dir_all(&dir).unwrap();
        for f in 0..(d % 7) {
            fs::write(dir.join(format!("f{f}.js")), vec![b'x'; d * 97 + f]).unwrap();
        }
        fs::write(
            p.join(format!("pkg-{d}")).join("index.js"),
            vec![1; 5000 + d],
        )
        .unwrap();
    }
    // More names than one 64 KiB listing buffer holds.
    let wide = p.join("wide");
    fs::create_dir(&wide).unwrap();
    for i in 0..1500 {
        fs::write(
            wide.join(format!("{i:04}-{}.txt", "n".repeat(40))),
            vec![0; i % 50],
        )
        .unwrap();
    }
    fs::write(p.join("مرحبا-世界.txt"), [7_u8; 4321]).unwrap();
    fs::hard_link(p.join("pkg-3/index.js"), p.join("pkg-4/linked.js")).unwrap();
    fs::hard_link(p.join("pkg-3/index.js"), p.join("wide/linked-again.js")).unwrap();
    fs::create_dir(p.join("empty")).unwrap();
    let target = p.join("pkg-1");
    let link = p.join("link-to-pkg");
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    root
}

#[test]
fn directory_and_legacy_walks_agree() {
    let root = tree();
    let new = scan_with(root.path(), Backend::Directory);
    let old = scan_with(root.path(), Backend::Legacy);
    assert!(new.summary.complete, "{:?}", new.summary.issues);
    assert_ne!(new.summary.backend, old.summary.backend);
    assert_eq!(totals(&new), totals(&old));
    assert_eq!(inventory(&new), inventory(&old));
    assert_eq!(
        new.summary.files,
        40 + 1500 + 1 + 2 + (0..40).map(|d| d % 7).sum::<u64>()
    );
    assert_eq!(new.summary.hard_link_duplicates, 2);
    assert_eq!(new.summary.skipped_links, 1);
    // `Auto` never reads the MFT for a folder, so it is the directory walk here.
    assert_eq!(
        scan_with(root.path(), Backend::Auto).summary.backend,
        new.summary.backend
    );
}

#[test]
fn directory_walk_reports_partial_scans_like_legacy() {
    let root = tree();
    for backend in [Backend::Directory, Backend::Legacy] {
        let limited = scan(
            root.path(),
            ScanOptions {
                backend,
                max_entries: 50,
                ..Default::default()
            },
            Arc::default(),
        )
        .unwrap();
        assert!(!limited.summary.complete);
        assert_eq!(limited.entries.len(), 50);
        assert_eq!(limited.summary.issue_count, 1);
        let control = Arc::new(ScanControl::default());
        control.cancel();
        let cancelled = scan(
            root.path(),
            ScanOptions {
                backend,
                ..Default::default()
            },
            control,
        )
        .unwrap();
        assert!(cancelled.summary.cancelled && !cancelled.summary.complete);
        assert!(!cancelled.entries[0].fully_listed);
    }
}

#[cfg(unix)]
#[test]
fn unreadable_folders_are_reported_by_every_walk() {
    use std::os::unix::fs::PermissionsExt;
    let root = tree();
    let locked = root.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("inside"), b"x").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = fs::read_dir(&locked).is_err();
    let new = scan_with(root.path(), Backend::Directory);
    let old = scan_with(root.path(), Backend::Legacy);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(totals(&new), totals(&old));
    assert_eq!(new.summary.issue_count, u64::from(unreadable));
    if unreadable {
        assert!(new.summary.issues[0].path.ends_with("locked"));
    }
}

#[cfg(windows)]
#[test]
fn unreadable_folders_are_reported_by_every_walk() {
    let root = tree();
    let locked = root.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("inside"), b"x").unwrap();
    let user = std::env::var("USERNAME").unwrap();
    let deny = |args: &[&str]| {
        std::process::Command::new("icacls")
            .arg(&locked)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(
        deny(&["/deny", &format!("{user}:(OI)(CI)(RD)")])
            .status
            .success()
    );
    let new = scan_with(root.path(), Backend::Directory);
    let old = scan_with(root.path(), Backend::Legacy);
    deny(&["/remove:d", &user]);
    assert_eq!(totals(&new), totals(&old));
    // An elevated process with backup rights may still read it.
    if new.summary.issue_count == 1 {
        assert!(new.summary.issues[0].path.ends_with("locked"));
        assert!(
            !new.summary.issues[0]
                .path
                .to_string_lossy()
                .starts_with(r"\\?\")
        );
    }
}

/// Windows, elevated: make NTFS write everything it holds in memory, so the
/// on-disk MFT includes the files just created. Returns false when the volume
/// cannot be opened for that (not elevated).
#[cfg(windows)]
fn flush_volume(path: &Path) -> bool {
    let letter = path
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .chars()
        .nth(4)
        .unwrap();
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!(r"\\.\{letter}:"))
        .and_then(|volume| volume.sync_all())
        .is_ok()
}

#[test]
fn the_mft_backend_is_explicit_about_where_it_works() {
    let root = tree();
    #[cfg(windows)]
    let elevated = flush_volume(root.path());
    let result = scan(
        root.path(),
        ScanOptions {
            backend: Backend::Mft,
            ..Default::default()
        },
        Arc::default(),
    );
    #[cfg(not(windows))]
    {
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("Windows only"), "{error}");
    }
    // Without administrator rights the volume cannot be opened: a clear error,
    // never a silent fallback when the MFT was asked for explicitly.
    #[cfg(windows)]
    match result {
        Err(error) if !elevated => {
            assert!(format!("{error:#}").contains("administrator"), "{error:#}")
        }
        result => mft_matches_the_walk(root.path(), result.unwrap()),
    }
}

/// Elevated (CI runners are): the MFT result for a folder must match the walk.
#[cfg(windows)]
fn mft_matches_the_walk(root: &Path, mft: Snapshot) {
    assert_eq!(mft.summary.backend, "ntfs-mft");
    let walk = scan_with(root, Backend::Directory);
    // The MFT is read from disk; anything NTFS had not written yet would be
    // missing. A mismatch prints both inventories' differences.
    let (a, b) = (inventory(&mft), inventory(&walk));
    let only_mft: Vec<_> = a
        .iter()
        .filter(|(k, v)| b.get(*k) != Some(v))
        .take(10)
        .collect();
    let only_walk: Vec<_> = b
        .iter()
        .filter(|(k, v)| a.get(*k) != Some(v))
        .take(10)
        .collect();
    assert!(
        only_mft.is_empty() && only_walk.is_empty(),
        "{only_mft:?} vs {only_walk:?}"
    );
    assert_eq!(totals(&mft), totals(&walk));
}
