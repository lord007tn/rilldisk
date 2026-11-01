// SPDX-License-Identifier: AGPL-3.0-only
use std::{
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Serialize, Serializer, ser::SerializeStruct};

mod platform;
mod reclaim;
mod stats;

pub use platform::{Volume, volumes};
pub use reclaim::{Candidate, Category, ReclaimReport, Safety, find_reclaimable};
pub use stats::{TypeStat, file_types, largest_files};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Directory,
    File,
    Link,
    Other,
}

/// Default bound on retained entries. Each costs roughly 200 bytes of memory.
pub const DEFAULT_MAX_ENTRIES: usize = 30_000_000;

/// One scanned name. Only the final component is stored; use [`Snapshot::path`] or
/// [`Snapshot::view`] for full paths. This keeps multi-million-entry scans compact.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: usize,
    pub parent_id: Option<usize>,
    /// Final path component; the root entry holds the full scan root.
    #[serde(serialize_with = "serialize_os")]
    pub name: Box<OsStr>,
    pub kind: EntryKind,
    /// Subtree bytes for directories; one file's length otherwise.
    pub logical_bytes: u64,
    /// Deduplicated by file ID. Attribution goes to the first encountered link.
    pub unique_logical_bytes: u64,
    /// None if the filesystem/backend cannot report allocated bytes.
    pub allocated_bytes: Option<u64>,
    /// Last write time in Unix milliseconds. Directories report the newest value in
    /// their subtree, so idle folders can be recognized without reading contents.
    pub modified_unix_ms: Option<u64>,
    /// Files in this subtree (1 for a file, including hard-link duplicates).
    pub file_count: u64,
    pub hard_link_duplicate: bool,
    /// The same file identity has another name in this scan (hard link). Deleting
    /// this name alone frees no space.
    pub multiple_links: bool,
    /// Directories: every child was read. False for folders that were unreadable,
    /// cancelled, cut off by the entry limit or never reached. Always true for files.
    pub fully_listed: bool,
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

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub summary: Summary,
    pub entries: Vec<Entry>,
    children: ChildIndex,
}

/// An entry with its full path, for JSON output and tool responses.
#[derive(Serialize)]
pub struct EntryView<'a> {
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    #[serde(flatten)]
    pub entry: &'a Entry,
}

struct Entries<'a>(&'a Snapshot);

impl Serialize for Entries<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq((0..self.0.entries.len()).map(|id| self.0.view(id)))
    }
}

impl Serialize for Snapshot {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Snapshot", 2)?;
        state.serialize_field("summary", &self.summary)?;
        state.serialize_field("entries", &Entries(self))?;
        state.end()
    }
}

/// Compressed child lists, each sorted by logical size (largest first).
#[derive(Debug, Clone, Default)]
struct ChildIndex {
    offsets: Vec<u32>,
    items: Vec<u32>,
}

impl ChildIndex {
    fn build(entries: &[Entry]) -> Self {
        let mut offsets = vec![0_u32; entries.len() + 1];
        for entry in entries {
            if let Some(parent) = entry.parent_id {
                offsets[parent + 1] += 1;
            }
        }
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }
        let mut cursor = offsets.clone();
        let mut items = vec![0_u32; entries.len().saturating_sub(1)];
        for entry in entries {
            if let Some(parent) = entry.parent_id {
                items[cursor[parent] as usize] = entry.id as u32;
                cursor[parent] += 1;
            }
        }
        for id in 0..entries.len() {
            let range = offsets[id] as usize..offsets[id + 1] as usize;
            items[range].sort_unstable_by(|&a, &b| {
                entries[b as usize]
                    .logical_bytes
                    .cmp(&entries[a as usize].logical_bytes)
                    .then(a.cmp(&b))
            });
        }
        Self { offsets, items }
    }
}

fn serialize_os<S: Serializer>(
    name: &OsStr,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(&name.to_string_lossy())
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
    /// Logical file bytes discovered so far (progress only; may include duplicates).
    pub bytes: AtomicU64,
    remaining_entries: AtomicUsize,
    limit_reached: AtomicBool,
    /// Set when the scan stops waiting (stall or cancel grace) so leftover workers quit.
    abandoned: AtomicBool,
}

impl ScanControl {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// Workers stop on user cancellation or when the scan has stopped waiting.
    pub(crate) fn should_stop(&self) -> bool {
        self.is_cancelled() || self.abandoned.load(Ordering::Relaxed)
    }
    pub(crate) fn record(&self, kind: EntryKind, logical: u64) -> bool {
        let mut remaining = self.remaining_entries.load(Ordering::Relaxed);
        loop {
            if remaining == 0 {
                self.limit_reached.store(true, Ordering::Relaxed);
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
                self.bytes.fetch_add(logical, Ordering::Relaxed);
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
            // Enumeration is latency-bound; more workers than cores helps on SSDs.
            threads: std::thread::available_parallelism().map_or(8, |n| n.get().clamp(8, 32)),
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

struct Walk {
    control: Arc<ScanControl>,
    volume: platform::VolumeInfo,
    sender: mpsc::Sender<(usize, Batch)>,
    /// Tasks spawned but not yet sent back.
    pending: AtomicUsize,
    next_key: AtomicUsize,
    in_flight: std::sync::Mutex<HashMap<usize, PathBuf>>,
}

fn read_task(walk: Arc<Walk>, key: usize, path: PathBuf) {
    let control = &walk.control;
    let mut batch = if control.should_stop() {
        Batch::default()
    } else {
        platform::read_directory(&path, control, &walk.volume)
    };
    for raw in &mut batch.entries {
        // Once the entry budget has refused a record the result is partial anyway;
        // until then, children still get a chance (an empty folder costs nothing).
        if raw.kind != EntryKind::Directory
            || control.should_stop()
            || control.limit_reached.load(Ordering::Relaxed)
        {
            continue;
        }
        let child = walk.next_key.fetch_add(1, Ordering::Relaxed);
        let child_path = path.join(&*raw.name);
        raw.key = Some(child);
        walk.pending.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut map) = walk.in_flight.lock() {
            map.insert(child, child_path.clone());
        }
        let walk = walk.clone();
        rayon::spawn(move || read_task(walk, child, child_path));
    }
    let _ = walk.sender.send((key, batch));
    if let Ok(mut map) = walk.in_flight.lock() {
        map.remove(&key);
    }
    walk.pending.fetch_sub(1, Ordering::AcqRel);
}

/// File IDs are already well distributed; a multiply-xorshift is enough and much
/// cheaper than SipHash for tens of millions of inserts.
#[derive(Default, Clone, Copy)]
struct IdentityHasher;

impl std::hash::BuildHasher for IdentityHasher {
    type Hasher = IdentityHash;
    fn build_hasher(&self) -> IdentityHash {
        IdentityHash(0)
    }
}

struct IdentityHash(u64);

impl std::hash::Hasher for IdentityHash {
    fn finish(&self) -> u64 {
        let x = self.0.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^ (x >> 29)
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(b)).wrapping_mul(0x100_0000_01B3);
        }
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(29) ^ value).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

/// After cancellation, stop waiting on reads that have not returned by then.
const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// A scan that makes no progress for this long reports what is outstanding and stops.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct RawEntry {
    name: Box<OsStr>,
    kind: EntryKind,
    logical: u64,
    allocated: Option<u64>,
    modified: Option<u64>,
    identity: Option<(u64, u64)>,
    /// Task key of a subdirectory a worker already queued for reading.
    key: Option<usize>,
}

#[derive(Default)]
pub(crate) struct Batch {
    entries: Vec<RawEntry>,
    issues: Vec<ScanIssue>,
    /// The listing ran to its natural end.
    complete: bool,
}

pub fn scan(root: &Path, options: ScanOptions, control: Arc<ScanControl>) -> Result<Snapshot> {
    if !(1..=64).contains(&options.threads) {
        bail!("threads must be between 1 and 64");
    }
    if options.max_entries == 0 {
        bail!("max_entries must be greater than zero");
    }
    // Walk with the canonical (verbatim `\\?\`) path so names ending in a dot or a
    // space and reserved device names open exactly; report the familiar form.
    let verbatim = root
        .canonicalize()
        .with_context(|| format!("cannot open {}", root.display()))?;
    let root = simplify(verbatim.clone());
    if !verbatim.is_dir() {
        bail!("scan root must be a directory");
    }
    // Reparse points are never traversed, so one scan stays on the root's volume.
    let volume = platform::volume_info(&verbatim)?;
    let start = Instant::now();
    let root_modified = std::fs::symlink_metadata(&verbatim)
        .and_then(|m| m.modified())
        .ok()
        .and_then(unix_ms);
    let mut summary = Summary {
        schema_version: 2,
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
        name: root.clone().into_os_string().into_boxed_os_str(),
        kind: EntryKind::Directory,
        logical_bytes: 0,
        unique_logical_bytes: 0,
        allocated_bytes: Some(0),
        modified_unix_ms: root_modified,
        file_count: 0,
        hard_link_duplicate: false,
        multiple_links: false,
        fully_listed: false,
    }];
    let mut first_link: HashMap<(u64, u64), usize, IdentityHasher> = HashMap::default();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.threads)
        .thread_name(|i| format!("rilldisk-scan-{i}"))
        .build()?;
    control.directories.store(1, Ordering::Relaxed);
    control.files.store(0, Ordering::Relaxed);
    control.bytes.store(0, Ordering::Relaxed);
    control
        .remaining_entries
        .store(options.max_entries - 1, Ordering::Relaxed);
    control.limit_reached.store(false, Ordering::Relaxed);
    // Workers queue subfolders the moment they read them, so the pool never waits on
    // this thread or on a slow sibling. This thread only assigns IDs; a batch whose
    // parent has no ID yet waits, so parents always precede children. All workers
    // reserve from one shared budget before retaining an entry.
    let (sender, results) = mpsc::channel();
    let walk = Arc::new(Walk {
        control: control.clone(),
        volume,
        sender,
        pending: AtomicUsize::new(1),
        next_key: AtomicUsize::new(1),
        in_flight: std::sync::Mutex::new(HashMap::from([(0, verbatim.clone())])),
    });
    {
        let walk = walk.clone();
        pool.spawn(move || read_task(walk, 0, verbatim));
    }
    let mut ids: Vec<usize> = vec![0];
    let mut waiting: HashMap<usize, Batch> = HashMap::new();
    let mut ready: Vec<(usize, Batch)> = Vec::new();
    let mut last_result = Instant::now();
    let mut last_budget = control.remaining_entries.load(Ordering::Relaxed);
    let mut cancel_seen: Option<Instant> = None;
    let mut stop = false;
    loop {
        if walk.pending.load(Ordering::Acquire) == 0 {
            ready.extend(results.try_iter());
            if ready.is_empty() {
                break;
            }
        } else {
            // A worker sends before it decrements `pending`, so poll briefly rather
            // than blocking: the final result can arrive just before the count drops.
            match results.recv_timeout(Duration::from_millis(5)) {
                Ok(result) => {
                    ready.push(result);
                    last_result = Instant::now();
                }
                Err(_) => {
                    // Entries recorded inside a long listing also count as progress.
                    let budget = control.remaining_entries.load(Ordering::Relaxed);
                    if budget != last_budget {
                        last_budget = budget;
                        last_result = Instant::now();
                    }
                    if control.is_cancelled() {
                        // Give in-flight reads a moment, then stop waiting on hung ones.
                        stop =
                            cancel_seen.get_or_insert_with(Instant::now).elapsed() > CANCEL_GRACE;
                    } else if last_result.elapsed() >= STALL_TIMEOUT {
                        // Everything still outstanding stopped responding (e.g. a hung
                        // network or cloud provider). Report it and keep what was found.
                        ready.extend(results.try_iter());
                        let arrived: std::collections::HashSet<usize> =
                            ready.iter().map(|(key, _)| *key).collect();
                        let stalled: Vec<PathBuf> = walk
                            .in_flight
                            .lock()
                            .map(|map| {
                                map.iter()
                                    .filter(|(key, _)| !arrived.contains(key))
                                    .map(|(_, path)| path.clone())
                                    .collect()
                            })
                            .unwrap_or_default();
                        for path in stalled {
                            add_issue(
                                &mut summary,
                                ScanIssue {
                                    path,
                                    message: format!(
                                        "not read: no progress for {} s (unresponsive folder or provider)",
                                        STALL_TIMEOUT.as_secs()
                                    ),
                                },
                            );
                        }
                        stop = true;
                    }
                    if !stop {
                        continue;
                    }
                }
            }
        }
        while let Some((key, batch)) = ready.pop() {
            let Some(&parent) = ids.get(key).filter(|&&id| id != usize::MAX) else {
                waiting.insert(key, batch);
                continue;
            };
            for issue in batch.issues {
                add_issue(&mut summary, issue);
            }
            entries[parent].fully_listed = batch.complete;
            for raw in batch.entries {
                let id = entries.len();
                let mut duplicate = false;
                let mut multiple_links = false;
                if raw.kind == EntryKind::File
                    && let Some(identity) = raw.identity
                {
                    match first_link.entry(identity) {
                        std::collections::hash_map::Entry::Occupied(first) => {
                            duplicate = true;
                            multiple_links = true;
                            entries[*first.get()].multiple_links = true;
                        }
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert(id);
                        }
                    }
                }
                let unique = if duplicate { 0 } else { raw.logical };
                let allocated = if duplicate { Some(0) } else { raw.allocated };
                match raw.kind {
                    EntryKind::Directory => {
                        summary.directories += 1;
                        if let Some(child) = raw.key {
                            if ids.len() <= child {
                                ids.resize(child + 1, usize::MAX);
                            }
                            ids[child] = id;
                            if let Some(early) = waiting.remove(&child) {
                                ready.push((child, early));
                            }
                        }
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
                    parent_id: Some(parent),
                    name: raw.name,
                    kind: raw.kind,
                    logical_bytes: raw.logical,
                    unique_logical_bytes: unique,
                    allocated_bytes: allocated,
                    modified_unix_ms: raw.modified,
                    file_count: u64::from(raw.kind == EntryKind::File),
                    hard_link_duplicate: duplicate,
                    multiple_links,
                    fully_listed: raw.kind != EntryKind::Directory,
                });
            }
        }
        if stop {
            break;
        }
    }
    // Leftover workers (hung reads, cancelled tails) exit at their next check.
    control.abandoned.store(true, Ordering::Relaxed);
    drop(first_link);
    // Report the memory bound once rather than once per truncated directory.
    if control.limit_reached.load(Ordering::Relaxed) {
        let path = summary.root.clone();
        add_issue(
            &mut summary,
            ScanIssue {
                path,
                message: format!(
                    "entry limit of {} reached; some folders were not scanned (raise --max-entries)",
                    options.max_entries
                ),
            },
        );
    }
    // Parents always precede their children, so one reverse pass aggregates the tree.
    for id in (1..entries.len()).rev() {
        let parent = entries[id].parent_id.expect("only the root has no parent");
        let logical = entries[id].logical_bytes;
        let unique = entries[id].unique_logical_bytes;
        let allocated = entries[id].allocated_bytes;
        let modified = entries[id].modified_unix_ms;
        entries[parent].modified_unix_ms = entries[parent].modified_unix_ms.max(modified);
        entries[parent].file_count += entries[id].file_count;
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
    let children = ChildIndex::build(&entries);
    Ok(Snapshot {
        summary,
        entries,
        children,
    })
}

/// Drop the `\\?\` prefix from canonical drive and UNC paths so reports show
/// familiar paths. The standard library re-adds it for long paths when needed.
pub fn simplify(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

fn join_lower(parent: &str, name: &OsStr) -> String {
    let name = name.to_string_lossy().to_lowercase();
    if parent.ends_with(std::path::MAIN_SEPARATOR) {
        format!("{parent}{name}")
    } else {
        format!("{parent}{}{name}", std::path::MAIN_SEPARATOR)
    }
}

pub(crate) fn unix_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

fn add_issue(summary: &mut Summary, mut issue: ScanIssue) {
    issue.path = simplify(issue.path);
    summary.issue_count += 1;
    if summary.issues.len() < 100 {
        summary.issues.push(issue);
    }
}

impl Snapshot {
    /// Immediate children of `id`, largest logical size first. Unknown IDs have none.
    pub fn children(&self, id: usize) -> &[u32] {
        match (
            self.children.offsets.get(id),
            self.children.offsets.get(id + 1),
        ) {
            (Some(&start), Some(&end)) => &self.children.items[start as usize..end as usize],
            _ => &[],
        }
    }

    /// Final path component, or the full root path for entry 0.
    pub fn name(&self, id: usize) -> std::borrow::Cow<'_, str> {
        self.entries[id].name.to_string_lossy()
    }

    /// Full path, rebuilt from the parent chain.
    pub fn path(&self, id: usize) -> PathBuf {
        let mut chain = Vec::new();
        let mut cursor = Some(id);
        while let Some(current) = cursor {
            chain.push(&*self.entries[current].name);
            cursor = self.entries[current].parent_id;
        }
        let mut path = PathBuf::from(chain.pop().expect("an entry has at least itself"));
        for name in chain.into_iter().rev() {
            path.push(name);
        }
        path
    }

    pub fn view(&self, id: usize) -> EntryView<'_> {
        EntryView {
            path: self.path(id),
            entry: &self.entries[id],
        }
    }

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
        // Lowercase directory paths, built once in parent-first order for path search.
        let dir_paths: Vec<String> = match &query {
            Some(_) => {
                let mut paths = vec![String::new(); self.entries.len()];
                paths[0] = self.name(0).to_lowercase();
                for entry in self.entries.iter().skip(1) {
                    if entry.kind == EntryKind::Directory {
                        let parent = &paths[entry.parent_id.unwrap_or(0)];
                        paths[entry.id] = join_lower(parent, &entry.name);
                    }
                }
                paths
            }
            None => Vec::new(),
        };
        let candidates: Box<dyn Iterator<Item = &Entry>> = match parent {
            Some(id) => Box::new(self.children(id).iter().map(|&c| &self.entries[c as usize])),
            None => Box::new(self.entries.iter().skip(1)),
        };
        let mut matches: Vec<_> = candidates
            .filter(|entry| {
                (!files_only || entry.kind == EntryKind::File)
                    && query.as_ref().is_none_or(|q| {
                        if entry.kind == EntryKind::Directory {
                            dir_paths[entry.id].contains(q.as_str())
                        } else {
                            let parent = &dir_paths[entry.parent_id.unwrap_or(0)];
                            parent.contains(q.as_str())
                                || join_lower(parent, &entry.name).contains(q.as_str())
                        }
                    })
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
