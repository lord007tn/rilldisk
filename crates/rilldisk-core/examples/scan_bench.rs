// SPDX-License-Identifier: AGPL-3.0-only
//! One scan, measured: run by `scripts/benchmark.py` once per (backend, run) in a
//! fresh process, so peak memory belongs to that scan alone.
//!
//! ```text
//! scan_bench PATH [--backend auto|mft|directory|legacy] [--threads N] [--max-entries N]
//! scan_bench --ntfs-image FILE [--threads N]     (Windows: the MFT parser on an image)
//! ```
//!
//! Prints one JSON object: the backend that ran, wall time around `scan()`,
//! files per second, peak memory, and every total the backends must agree on.
//! Only counts and timings are printed, never names or paths below the root.
use rilldisk_core::{Backend, ScanControl, ScanOptions, Snapshot};
use std::{path::PathBuf, sync::Arc, time::Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut options = ScanOptions::default();
    let mut path = None;
    let mut image = None;
    let mut backup = false;
    let mut list = 0_usize;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--backend" => {
                options.backend = match args.next().as_deref() {
                    Some("auto") => Backend::Auto,
                    Some("mft") => Backend::Mft,
                    Some("directory" | "dir") => Backend::Directory,
                    Some("legacy") => Backend::Legacy,
                    other => panic!("unknown backend {other:?}"),
                }
            }
            "--threads" => options.threads = args.next().and_then(|v| v.parse().ok()).unwrap(),
            "--max-entries" => {
                options.max_entries = args.next().and_then(|v| v.parse().ok()).unwrap()
            }
            "--ntfs-image" => image = args.next().map(PathBuf::from),
            // Elevated Windows: enable SeBackupPrivilege (as the CLI does when
            // elevated), so walks read folders whose ACL denies this user.
            "--backup-privilege" => backup = rilldisk_core::enable_backup_privilege(),
            // Debugging synthetic fixtures only: print entries this many levels deep.
            "--list-depth" => list = args.next().and_then(|v| v.parse().ok()).unwrap(),
            other => path = Some(PathBuf::from(other)),
        }
    }
    let control = Arc::new(ScanControl::default());
    let start = Instant::now();
    let snapshot: Snapshot = match image {
        #[cfg(windows)]
        Some(image) => rilldisk_core::scan_ntfs_image(&image, options.clone(), control),
        #[cfg(not(windows))]
        Some(_) => panic!("--ntfs-image needs Windows"),
        None => rilldisk_core::scan(
            &path.expect("usage: scan_bench PATH [--backend B] [--threads N]"),
            options.clone(),
            control,
        ),
    }
    .expect("scan failed");
    let wall = start.elapsed().as_secs_f64() * 1000.0;
    if list > 0 {
        let mut depth = vec![0_usize; snapshot.entries.len()];
        for e in &snapshot.entries[1..] {
            depth[e.id] = depth[e.parent_id.unwrap()] + 1;
            if depth[e.id] <= list {
                eprintln!(
                    "{:?} {} {} {:?}",
                    e.kind,
                    e.logical_bytes,
                    snapshot.path(e.id).display(),
                    e.allocated_bytes
                );
            }
        }
    }
    let s = &snapshot.summary;
    let entries = snapshot.entries.len();
    let (peak_working_set, peak_commit) = peak_memory();
    println!(
        concat!(
            "{{\"backend\":\"{}\",\"requested\":\"{:?}\",\"threads\":{},\"wall_ms\":{:.1},\"scanner_ms\":{},",
            "\"files_per_s\":{:.0},\"entries\":{},\"files\":{},\"directories\":{},\"logical_bytes\":{},",
            "\"unique_logical_bytes\":{},\"allocated_bytes\":{},\"hard_link_duplicates\":{},",
            "\"skipped_links\":{},\"skipped_mounts\":{},\"issue_count\":{},\"complete\":{},",
            "\"peak_rss_bytes\":{},\"peak_commit_bytes\":{},\"backup_privilege\":{}}}"
        ),
        s.backend,
        options.backend,
        options.threads,
        wall,
        s.elapsed_ms,
        s.files as f64 / wall * 1000.0,
        entries,
        s.files,
        s.directories,
        s.logical_bytes,
        s.unique_logical_bytes,
        s.allocated_bytes
            .map_or("null".to_string(), |b| b.to_string()),
        s.hard_link_duplicates,
        s.skipped_links,
        s.skipped_mounts,
        s.issue_count,
        s.complete,
        peak_working_set,
        peak_commit,
        backup,
    );
    // Freeing tens of millions of names is not part of the scan.
    std::mem::forget(snapshot);
}

/// (peak resident bytes, peak committed bytes) of this process.
#[cfg(windows)]
fn peak_memory() -> (u64, u64) {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };
    // SAFETY: zeroed counters are valid; the call fills them for this process.
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: the pseudo-handle is always valid; counters is writable.
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (
        counters.PeakWorkingSetSize as u64,
        counters.PeakPagefileUsage as u64,
    )
}

#[cfg(unix)]
fn peak_memory() -> (u64, u64) {
    // SAFETY: zeroed rusage is valid; getrusage fills it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: usage is writable.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    // Linux reports KiB, macOS bytes.
    let rss = usage.ru_maxrss as u64 * if cfg!(target_os = "macos") { 1 } else { 1024 };
    (rss, 0)
}
