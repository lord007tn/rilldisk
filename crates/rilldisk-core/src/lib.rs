// SPDX-License-Identifier: AGPL-3.0-only
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::{Serialize, Serializer};

mod platform;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Directory,
    File,
    Link,
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: usize,
    pub parent_id: Option<usize>,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Subtree bytes for directories; one file's length otherwise.
    pub logical_bytes: u64,
    /// Deduplicated by file ID. Attribution goes to the first encountered link.
    pub unique_logical_bytes: u64,
    /// None if the filesystem/backend cannot report allocated bytes.
    pub allocated_bytes: Option<u64>,
    pub hard_link_duplicate: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanIssue {
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub schema_version: u32,
    #[serde(serialize_with = "serialize_path")]
    pub root: PathBuf,
    pub backend: &'static str,
    pub started_unix_ms: u128,
    pub elapsed_ms: u128,
    pub files: u64,
    pub directories: u64,
    pub skipped_links: u64,
    pub hard_link_duplicates: u64,
    pub logical_bytes: u64,
    pub unique_logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub issue_count: u64,
    pub issues: Vec<ScanIssue>,
    pub cancelled: bool,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub summary: Summary,
    pub entries: Vec<Entry>,
}

fn serialize_path<S: Serializer>(
    path: &Path,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

#[derive(Debug, Default)]
pub struct ScanControl {
    pub cancel: AtomicBool,
    pub files: AtomicU64,
    pub directories: AtomicU64,
    remaining_entries: AtomicUsize,
}

impl ScanControl {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub(crate) fn record(&self, kind: EntryKind) -> bool {
        let mut remaining = self.remaining_entries.load(Ordering::Relaxed);
        loop {
            if remaining == 0 {
                return false;
            }
            match self.remaining_entries.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(value) => remaining = value,
            }
        }
        match kind {
            EntryKind::File => {
                self.files.fetch_add(1, Ordering::Relaxed);
            }
            EntryKind::Directory => {
                self.directories.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub threads: usize,
    /// A deliberate memory bound; reaching it yields a partial snapshot.
    pub max_entries: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            threads: std::thread::available_parallelism().map_or(4, |n| n.get().min(8)),
            max_entries: 5_000_000,
        }
    }
}

pub(crate) struct RawEntry {
    path: PathBuf,
    kind: EntryKind,
    logical: u64,
    allocated: Option<u64>,
    identity: Option<(u64, u64)>,
}

pub(crate) struct Batch {
    entries: Vec<RawEntry>,
    issues: Vec<ScanIssue>,
}

pub fn scan(root: &Path, options: ScanOptions, control: Arc<ScanControl>) -> Result<Snapshot> {
    if !(1..=64).contains(&options.threads) {
        bail!("threads must be between 1 and 64");
    }
    if options.max_entries == 0 {
        bail!("max_entries must be greater than zero");
    }
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot open {}", root.display()))?;
    if !root.is_dir() {
        bail!("scan root must be a directory");
    }
    let allocation_unit = platform::allocation_unit(&root)?;
    let start = Instant::now();
    let mut summary = Summary {
        schema_version: 1,
        root: root.clone(),
        backend: platform::BACKEND,
        started_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        elapsed_ms: 0,
        files: 0,
        directories: 1,
        skipped_links: 0,
        hard_link_duplicates: 0,
        logical_bytes: 0,
        unique_logical_bytes: 0,
        allocated_bytes: Some(0),
        issue_count: 0,
        issues: Vec::new(),
        cancelled: false,
        complete: true,
    };
    let mut entries = vec![Entry {
        id: 0,
        parent_id: None,
        path: root.clone(),
        kind: EntryKind::Directory,
        logical_bytes: 0,
        unique_logical_bytes: 0,
        allocated_bytes: Some(0),
        hard_link_duplicate: false,
    }];
    let mut frontier = vec![(0, root)];
    let mut seen = HashSet::new();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.threads)
        .build()?;
    control.directories.store(1, Ordering::Relaxed);
    control.files.store(0, Ordering::Relaxed);
    control
        .remaining_entries
        .store(options.max_entries - 1, Ordering::Relaxed);
    // All workers reserve from one budget before retaining an entry.
    'walk: while !frontier.is_empty() && !control.is_cancelled() {
        let mut next = Vec::new();
        for chunk in frontier.chunks(options.threads * 2) {
            if control.is_cancelled() {
                break 'walk;
            }
            let batches: Vec<_> = pool.install(|| {
                chunk
                    .par_iter()
                    .map(|(_, path)| platform::read_directory(path, &control, allocation_unit))
                    .collect()
            });
            for ((parent, _), batch) in chunk.iter().zip(batches) {
                for issue in batch.issues {
                    add_issue(&mut summary, issue);
                }
                for raw in batch.entries {
                    let duplicate = raw.kind == EntryKind::File
                        && raw.identity.is_some_and(|id| !seen.insert(id));
                    let unique = if duplicate { 0 } else { raw.logical };
                    let allocated = if duplicate { Some(0) } else { raw.allocated };
                    let id = entries.len();
                    match raw.kind {
                        EntryKind::Directory => {
                            summary.directories += 1;
                            next.push((id, raw.path.clone()));
                        }
                        EntryKind::File => {
                            summary.files += 1;
                            if duplicate {
                                summary.hard_link_duplicates += 1;
                            }
                        }
                        EntryKind::Link => summary.skipped_links += 1,
                        EntryKind::Other => {}
                    }
                    entries.push(Entry {
                        id,
                        parent_id: Some(*parent),
                        path: raw.path,
                        kind: raw.kind,
                        logical_bytes: raw.logical,
                        unique_logical_bytes: unique,
                        allocated_bytes: allocated,
                        hard_link_duplicate: duplicate,
                    });
                }
            }
        }
        if !next.is_empty() && control.remaining_entries.load(Ordering::Relaxed) == 0 {
            let path = summary.root.clone();
            add_issue(
                &mut summary,
                ScanIssue {
                    path,
                    message: "entry limit reached; unscanned directories remain".into(),
                },
            );
            break;
        }
        frontier = next;
    }
    // Parents always precede their children, so one reverse pass aggregates the tree.
    for id in (1..entries.len()).rev() {
        let parent = entries[id].parent_id.expect("only the root has no parent");
        let logical = entries[id].logical_bytes;
        let unique = entries[id].unique_logical_bytes;
        let allocated = entries[id].allocated_bytes;
        entries[parent].logical_bytes = entries[parent].logical_bytes.saturating_add(logical);
        entries[parent].unique_logical_bytes =
            entries[parent].unique_logical_bytes.saturating_add(unique);
        entries[parent].allocated_bytes = entries[parent]
            .allocated_bytes
            .zip(allocated)
            .map(|(a, b)| a.saturating_add(b));
    }
    summary.logical_bytes = entries[0].logical_bytes;
    summary.unique_logical_bytes = entries[0].unique_logical_bytes;
    summary.allocated_bytes = entries[0].allocated_bytes;
    summary.elapsed_ms = start.elapsed().as_millis();
    summary.cancelled = control.is_cancelled();
    summary.complete = !summary.cancelled && summary.issue_count == 0;
    control.files.store(summary.files, Ordering::Relaxed);
    control
        .directories
        .store(summary.directories, Ordering::Relaxed);
    Ok(Snapshot { summary, entries })
}

fn add_issue(summary: &mut Summary, issue: ScanIssue) {
    summary.issue_count += 1;
    if summary.issues.len() < 100 {
        summary.issues.push(issue);
    }
}

impl Snapshot {
    /// Largest entries, optionally restricted to immediate children or matching paths.
    /// The page size is capped so MCP clients cannot accidentally request the whole disk.
    pub fn query(
        &self,
        parent: Option<usize>,
        query: Option<&str>,
        files_only: bool,
        offset: usize,
        limit: usize,
    ) -> (usize, Vec<&Entry>) {
        let query = query.map(str::to_lowercase);
        let mut matches: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| {
                entry.id != 0
                    && parent.is_none_or(|id| entry.parent_id == Some(id))
                    && (!files_only || entry.kind == EntryKind::File)
                    && query
                        .as_ref()
                        .is_none_or(|q| entry.path.to_string_lossy().to_lowercase().contains(q))
            })
            .collect();
        let total = matches.len();
        let end = offset.saturating_add(limit.min(500)).min(total);
        if offset >= end {
            return (total, Vec::new());
        }
        let compare =
            |a: &&Entry, b: &&Entry| b.logical_bytes.cmp(&a.logical_bytes).then(a.id.cmp(&b.id));
        if end < total {
            matches.select_nth_unstable_by(end, compare);
            matches.truncate(end);
        }
        matches.sort_unstable_by(compare);
        (
            total,
            matches
                .into_iter()
                .skip(offset)
                .take(limit.min(500))
                .collect(),
        )
    }
}
