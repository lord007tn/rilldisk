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
    let snapshot = scan_ntfs_image(image.path(), ScanOptions::default(), Arc::default()).unwrap();
    let s = &snapshot.summary;
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
