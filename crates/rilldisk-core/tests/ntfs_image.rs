// SPDX-License-Identifier: AGPL-3.0-only
//! The MFT parser on a real (synthetic) NTFS filesystem, without administrator
//! rights: `fixtures/ntfs-tiny.img.gz` is an 8 MiB volume made by
//! `scripts/make_ntfs_image.sh` (mkntfs + ntfs-3g). Expected values are computed
//! from what that script writes, not read back from any scanner.
#![cfg(windows)]
use rilldisk_core::{EntryKind, ScanControl, ScanOptions, scan_ntfs_image};
use std::{io::Read, sync::Arc};

fn image() -> tempfile::NamedTempFile {
    let packed = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/ntfs-tiny.img.gz"
    ))
    .unwrap();
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(&packed[..])
        .read_to_end(&mut raw)
        .unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), raw).unwrap();
    file
}

#[test]
fn reads_names_sizes_links_and_extension_records() {
    let image = image();
    let control = Arc::new(ScanControl::default());
    let snapshot = scan_ntfs_image(image.path(), ScanOptions::default(), control.clone()).unwrap();
    let s = &snapshot.summary;
    // Entries built in parallel still reach the live view through the collector.
    let live = control.live().expect("published at the end of the scan");
    assert_eq!(live.total_logical(), s.logical_bytes);
    for folder in &live.folders {
        assert_eq!(folder.name, snapshot.name(folder.id));
    }
    // Headless mode produces the same inventory without publishing a view.
    let control = Arc::new(ScanControl::without_live_view());
    let plain = scan_ntfs_image(image.path(), ScanOptions::default(), control.clone()).unwrap();
    assert!(control.live().is_none());
    assert_eq!(plain.entries.len(), snapshot.entries.len());
    assert_eq!(plain.summary.logical_bytes, s.logical_bytes);
    assert_eq!(plain.summary.allocated_bytes, s.allocated_bytes);
    assert!(s.complete, "{:?}", s.issues);
    assert_eq!(s.backend, "ntfs-mft");
    // small, big, its link, deep, 名前, sparse, 300 numbered, the 50-link target
    // and its 50 links, the 200-character name.
    assert_eq!(s.files, 1 + 1 + 1 + 1 + 1 + 1 + 300 + 1 + 50 + 1);
    // root, a, a/b, a/b/c, many, trail, ünïcødé-مرحبا, links
    assert_eq!(s.directories, 8);
    let numbered: u64 = (1..=300).sum();
    let logical = 17 + 2 * 100_000 + 5000 + 1234 + (10 << 20) + numbered + 51 * 9000 + 42;
    assert_eq!(s.logical_bytes, logical);
    assert_eq!(s.unique_logical_bytes, logical - 100_000 - 50 * 9000);
    // 4 KiB clusters: big 25, deep 2, 名前 1, the link target 3. Data that fits in
    // its MFT record and the sparse file's hole have no clusters.
    assert_eq!(s.allocated_bytes, Some((25 + 2 + 1 + 3) * 4096));
    // 51 names of one file: most of them live in MFT extension records.
    assert_eq!(s.hard_link_duplicates, 51);
    let find = |name: &str| {
        snapshot
            .entries
            .iter()
            .find(|e| snapshot.name(e.id) == name)
            .unwrap_or_else(|| panic!("{name} is listed"))
    };
    let links = find("links");
    assert_eq!((links.kind, links.file_count), (EntryKind::Directory, 50));
    assert!(links.fully_listed);
    assert_eq!(find("名前.txt").logical_bytes, 1234);
    assert_eq!(find("sparse.bin").allocated_bytes, Some(0));
    assert!(find("many-links-target.bin").multiple_links);
    let deep = find("deep.bin");
    assert!(snapshot.path(deep.id).ends_with(r"a\b\c\deep.bin"));
    assert_eq!(find(&format!("{}.txt", "L".repeat(200))).logical_bytes, 42);
}

#[test]
fn a_torn_git_pointer_cannot_turn_a_tracked_worktree_into_an_orphan() {
    let packed = include_bytes!("fixtures/worktrees.img.gz");
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(&packed[..])
        .read_to_end(&mut raw)
        .unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    let scan = |bytes: &[u8]| {
        std::fs::write(file.path(), bytes).unwrap();
        scan_ntfs_image(file.path(), ScanOptions::default(), Arc::default()).unwrap()
    };
    let report =
        |s: &rilldisk_core::Snapshot| rilldisk_core::ReclaimDiscovery::new(s).into_report(0);
    let intact = scan(&raw);
    assert!(intact.summary.complete);
    let before = report(&intact);
    assert!(
        before
            .candidates
            .iter()
            .any(|c| c.path.ends_with("tracked") && c.is_linked_worktree())
    );
    assert!(
        before
            .candidates
            .iter()
            .any(|c| c.path.ends_with("orphan") && c.label == "Orphaned agent worktree")
    );

    // The fixture uses 1 KiB FILE records and 512-byte fixup sectors.
    // Identify the pointer by its unique resident payload, independent of its record number.
    let marker = b"gitdir: synthetic-worktree-fixture";
    let (records, tail) = raw.as_chunks_mut::<1024>();
    assert!(tail.is_empty());
    let pointer = records
        .iter_mut()
        .find(|r| r.starts_with(b"FILE") && r.windows(marker.len()).any(|w| w == marker))
        .unwrap();
    pointer[510] ^= 1;
    let partial = scan(&raw);
    assert!(!partial.summary.complete);
    assert!(
        partial
            .summary
            .issues
            .iter()
            .any(|i| i.message.contains("1 MFT records"))
    );
    assert!(
        partial
            .entries
            .iter()
            .any(|e| partial.name(e.id) == "tracked" && !e.fully_listed)
    );
    assert!(
        !report(&partial)
            .candidates
            .iter()
            .any(|c| c.label == "Orphaned agent worktree")
    );
}

#[test]
fn image_scans_honor_the_entry_budget_and_cancellation() {
    let image = image();
    let budget = |max_entries| {
        scan_ntfs_image(
            image.path(),
            ScanOptions {
                max_entries,
                ..Default::default()
            },
            Arc::default(),
        )
    };
    // Parsing the table costs memory per record, so a budget far below the
    // table's size is refused up front rather than ignored.
    let refused = format!("{:#}", budget(20).unwrap_err());
    assert!(refused.contains("raise --max-entries"), "{refused}");
    let limited = budget(300).unwrap();
    assert!(!limited.summary.complete);
    assert_eq!(limited.entries.len(), 300);
    assert!(
        limited.summary.issues[0]
            .message
            .contains("entry limit of 300")
    );
    let control = Arc::new(ScanControl::default());
    control.cancel();
    let cancelled = scan_ntfs_image(image.path(), ScanOptions::default(), control).unwrap();
    assert!(cancelled.summary.cancelled && !cancelled.summary.complete);
    assert_eq!(cancelled.entries.len(), 1);
}
