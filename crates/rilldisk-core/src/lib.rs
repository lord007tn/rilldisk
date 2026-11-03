// SPDX-License-Identifier: AGPL-3.0-only
use std::{
    collections::HashMap,
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::{Serialize, Serializer, ser::SerializeStruct};

pub mod cleanup;
pub mod git;
mod live;
mod platform;
mod reclaim;
mod stats;

pub use cleanup::{CleanupMode, CleanupPlan, CleanupReport};
pub use live::{LiveFolder, LiveView};
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

/// Why a path could not be read, for programs that act on issues (for example,
/// offering to rerun with administrator rights). `message` keeps the OS text.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    /// The OS refused access (Windows `ERROR_ACCESS_DENIED`,
    /// `ERROR_NETWORK_ACCESS_DENIED`; Unix `EACCES`/`EPERM`). Administrator rights
    /// usually help; on macOS, `EPERM` may need Full Disk Access instead.
    AccessDenied,
    /// The path disappeared between listing and reading.
    NotFound,
    /// Another process holds the path open or locked.
    InUse,
    /// The read made no progress before the stall timeout.
    Stalled,
    /// The entry limit stopped the scan.
    EntryLimit,
    Other,
}

impl IssueKind {
    /// Classify an I/O error. Raw OS codes are checked first because they are
    /// platform-specific (raw 1 is `EPERM` on Unix but `ERROR_INVALID_FUNCTION` on
    /// Windows), then the portable [`io::ErrorKind`].
    pub fn of(error: &io::Error) -> Self {
        if let Some(kind) = error.raw_os_error().and_then(Self::from_raw_os_error) {
            return kind;
        }
        match error.kind() {
            io::ErrorKind::PermissionDenied => Self::AccessDenied,
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::ResourceBusy => Self::InUse,
            _ => Self::Other,
        }
    }

    /// The kind for a raw OS error code of the platform this was compiled for.
    pub fn from_raw_os_error(code: i32) -> Option<Self> {
        if cfg!(windows) {
            match code {
                // ERROR_ACCESS_DENIED, ERROR_NETWORK_ACCESS_DENIED,
                // ERROR_PRIVILEGE_NOT_HELD
                5 | 65 | 1314 => Some(Self::AccessDenied),
                // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
                2 | 3 => Some(Self::NotFound),
                // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
                32 | 33 => Some(Self::InUse),
                _ => None,
            }
        } else {
            match code {
                // EPERM (macOS privacy protection), EACCES
                1 | 13 => Some(Self::AccessDenied),
                // ENOENT, ENOTDIR (replaced by a file since it was listed)
                2 | 20 => Some(Self::NotFound),
                // EBUSY, ETXTBSY
                16 | 26 => Some(Self::InUse),
                _ => None,
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanIssue {
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub kind: IssueKind,
    pub message: String,
    /// The raw OS error code, when the issue came from the OS. On macOS, `1`
    /// (`EPERM`) on a folder usually means privacy protection (Full Disk Access may
    /// lift it), or the sandbox of the app that started this process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_error: Option<i32>,
}

impl ScanIssue {
    pub fn new(path: impl Into<PathBuf>, kind: IssueKind, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind,
            message: message.into(),
            os_error: None,
        }
    }

    /// An issue for an I/O error, classified by [`IssueKind::of`].
    pub fn from_error(path: impl Into<PathBuf>, error: &io::Error) -> Self {
        Self {
            path: path.into(),
            kind: IssueKind::of(error),
            message: error.to_string(),
            os_error: error.raw_os_error(),
        }
    }
}

/// The process's privilege state, for deciding whether to offer a rerun with more
/// rights. Querying never changes anything.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
pub struct Elevation {
    /// Windows: the token is elevated (Run as administrator). Unix: effective UID 0.
    pub is_elevated: bool,
    /// The OS offers a way to restart with more rights: Windows UAC (consent or
    /// credentials); Unix `sudo` or `pkexec` on `PATH`. False when already elevated.
    pub can_elevate: bool,
    /// Windows: `SeBackupPrivilege` is enabled, so directories protected by their
    /// ACLs can be listed through backup semantics. Always false on Unix.
    pub backup_privilege_enabled: bool,
}

/// Current privilege state of this process. On Unix this looks for `sudo` and
/// `pkexec` on `PATH`; [`is_elevated`] alone touches no file.
pub fn elevation() -> Elevation {
    platform::elevation()
}

/// Whether this process is elevated (administrator token, or effective UID 0).
/// Cheap and file-free, so a scan can record it before it starts.
pub fn is_elevated() -> bool {
    platform::is_elevated()
}

/// Whether `program` is an executable file in a `PATH` directory that this user
/// may run (Unix `access(X_OK)`; Windows: a file with a `PATHEXT` extension), for
/// offering `sudo` or `pkexec`.
pub fn executable_on_path(program: &str) -> bool {
    platform::executable_on_path(program)
}

/// When the process is elevated on Windows, enable `SeBackupPrivilege` on its token
/// so directory listings (which always open with `FILE_FLAG_BACKUP_SEMANTICS`)
/// bypass read ACLs. The privilege only widens read access through that flag:
/// nothing is written and no ACL or owner is changed. Does nothing when not
/// elevated, and on Unix, where root already lists everything except macOS
/// privacy-protected folders. Returns whether the privilege is enabled afterwards.
/// The CLI scan commands and the desktop call this; the MCP server never does.
pub fn enable_backup_privilege() -> bool {
    platform::enable_backup_privilege()
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
    /// Directories recorded but not entered because they belong to another
    /// filesystem (a mount point) or to a virtual one such as `/proc`. Their sizes
    /// are not included. Always zero on Windows, where mount points are reparse
    /// points and count as `skipped_links`.
    pub skipped_mounts: u64,
    pub hard_link_duplicates: u64,
    pub logical_bytes: u64,
    pub unique_logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub issue_count: u64,
    /// Issues of kind `access_denied`, counted over all issues (not only the
    /// listed ones). Each is a folder that could not be listed or whose entries
    /// could not be examined. Rerunning elevated usually reads these folders.
    pub access_denied_count: u64,
    /// macOS only: the part of `access_denied_count` refused with `EPERM`, which
    /// privacy protection returns even to root. Full Disk Access usually lifts it;
    /// it does not lift the sandbox of a sandboxed parent app, which also returns
    /// `EPERM`. Counted over all issues; always 0 on other platforms.
    pub privacy_denied_count: u64,
    pub issues: Vec<ScanIssue>,
    pub cancelled: bool,
    pub complete: bool,
    /// The scan stopped at its entry limit (`ScanOptions::max_entries`): some
    /// folders were not read. Set even when the issue list is full.
    pub entry_limit_reached: bool,
    /// The scanning process was elevated (administrator or root).
    pub elevated: bool,
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
        // Sort each folder's children in parallel; the slices are disjoint.
        let mut lists: Vec<&mut [u32]> = Vec::new();
        let mut rest = &mut items[..];
        for id in 0..entries.len() {
            let len = (offsets[id + 1] - offsets[id]) as usize;
            let (list, tail) = std::mem::take(&mut rest).split_at_mut(len);
            rest = tail;
            if len > 1 {
                lists.push(list);
            }
        }
        lists.par_iter_mut().for_each(|list| {
            list.sort_unstable_by(|&a, &b| {
                entries[b as usize]
                    .logical_bytes
                    .cmp(&entries[a as usize].logical_bytes)
                    .then(a.cmp(&b))
            });
        });
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
    /// Latest picture of the scan in progress; see [`ScanControl::live`].
    live: std::sync::Mutex<Option<Arc<LiveView>>>,
    live_focus: AtomicUsize,
    backend_status: std::sync::Mutex<String>,
    /// NTFS table scans: records read so far and in total (0 for other scans).
    mft_read: AtomicU64,
    mft_total: AtomicU64,
    /// The table is read; folders are being built from it.
    mft_building: AtomicBool,
}

/// Where an NTFS table (MFT) scan is, for progress displays. The table is read in
/// one pass before any folder exists, so this is the only progress until then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MftProgress {
    pub records_read: u64,
    pub records_total: u64,
    /// Reading is done and folders are being built.
    pub building: bool,
}

impl ScanControl {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// Current backend selection, including an Auto fallback reason.
    pub fn backend_status(&self) -> String {
        self.backend_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }
    fn set_backend_status(&self, status: String) {
        if let Ok(mut slot) = self.backend_status.lock() {
            *slot = status;
        }
    }
    /// Request a directory from the current live view; root is zero.
    pub fn focus_live(&self, id: usize) {
        self.live_focus.store(id, Ordering::Relaxed);
    }
    /// Requested focus, including a navigation click not yet published.
    pub fn live_focus(&self) -> usize {
        self.live_focus.load(Ordering::Relaxed)
    }
    /// Partial totals below the requested directory, refreshed during scanning.
    /// Progress of an NTFS table scan, if this is one.
    pub fn mft_progress(&self) -> Option<MftProgress> {
        let total = self.mft_total.load(Ordering::Relaxed);
        (total > 0).then(|| MftProgress {
            records_read: self.mft_read.load(Ordering::Relaxed).min(total),
            records_total: total,
            building: self.mft_building.load(Ordering::Relaxed),
        })
    }

    pub fn live(&self) -> Option<Arc<LiveView>> {
        self.live.lock().ok().and_then(|slot| slot.clone())
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

/// How a scan enumerates the tree. Every backend reports the same accounting
/// (logical, unique and allocated bytes, hard links, links not followed,
/// partial results); [`Summary::backend`] names the one that ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// Windows: read the NTFS Master File Table when the root is a whole NTFS
    /// volume and the process may read the volume (administrator); otherwise, and
    /// whenever that fails, the `Directory` walk. Linux and macOS use the
    /// descriptor-relative `Directory` walk to reject symlink replacements.
    #[default]
    Auto,
    /// Windows only: read the NTFS Master File Table directly. Needs administrator
    /// rights and an NTFS volume, and reads the whole table even for a subfolder.
    Mft,
    /// Walk directories with the fastest listing calls this OS offers.
    Directory,
    /// The directory walk of RillDisk 0.1: full-path opens with
    /// `GetFileInformationByHandleEx` on Windows (kept as a reference for
    /// benchmarks and comparisons), `std::fs::read_dir` on Linux and macOS
    /// without the descriptor-relative opens that `Auto` uses.
    Legacy,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub threads: usize,
    /// A deliberate memory bound; reaching it yields a partial snapshot.
    pub max_entries: usize,
    /// Unix: also enter other mounted filesystems below the root (like `du` without
    /// `-x`). Virtual filesystems are skipped either way. Ignored on Windows.
    pub cross_filesystems: bool,
    pub backend: Backend,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            // Enumeration is latency-bound; more workers than cores helps on SSDs.
            threads: std::thread::available_parallelism().map_or(8, |n| n.get().clamp(8, 32)),
            max_entries: DEFAULT_MAX_ENTRIES,
            cross_filesystems: false,
            backend: Backend::Auto,
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
}

fn read_task(walk: Arc<Walk>, key: usize, target: platform::Target) {
    let control = &walk.control;
    let mut listing = if control.should_stop() {
        platform::Listing::default()
    } else {
        platform::read_directory(target, control, &walk.volume)
    };
    for (index, child) in listing.children.drain(..) {
        // Once the entry budget has refused a record the result is partial anyway;
        // until then, children still get a chance (an empty folder costs nothing).
        if control.should_stop() || control.limit_reached.load(Ordering::Relaxed) {
            break;
        }
        let child_key = walk.next_key.fetch_add(1, Ordering::Relaxed);
        listing.batch.entries[index].key = Some(child_key);
        walk.pending.fetch_add(1, Ordering::AcqRel);
        let walk = walk.clone();
        rayon::spawn(move || read_task(walk, child_key, child));
    }
    let _ = walk.sender.send((key, listing.batch));
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
    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }
}

/// First entry seen for each file identity, to find hard-link duplicates.
///
/// NTFS file IDs are a 48-bit MFT record number plus a 16-bit sequence number,
/// and record numbers are dense, so once a scan has seen many files a flat table
/// indexed by record number is far smaller than a hash map (8 bytes per record
/// instead of ~40 per file) and needs no hashing. Small scans keep the map: the
/// table is sized by the highest record number on the volume, not by the scan.
/// Identities that do not look like NTFS IDs (ReFS 128-bit IDs, Unix
/// `(device, inode)` pairs, recorded only for files with several links) always
/// use the map.
#[derive(Default)]
struct LinkTable {
    /// Record number -> (first entry id + 1, sequence number); 0 is empty.
    dense: Vec<(u32, u16)>,
    dense_active: bool,
    max_record: u64,
    map: HashMap<(u64, u64), usize, IdentityHasher>,
}

/// Record numbers above this always go to the map.
const DENSE_RECORDS: u64 = 1 << 27;
const RECORD_MASK: u64 = 0xFFFF_FFFF_FFFF;

impl LinkTable {
    fn dense_key(identity: (u64, u64), id: usize) -> Option<(usize, u16)> {
        let record = identity.1 & RECORD_MASK;
        (identity.0 == 0 && record < DENSE_RECORDS && id < u32::MAX as usize)
            .then_some((record as usize, (identity.1 >> 48) as u16))
    }

    /// The first entry with this identity, or None after recording `id` as first.
    fn first(&mut self, identity: (u64, u64), id: usize) -> Option<usize> {
        let dense = Self::dense_key(identity, id);
        if let Some((record, sequence)) = dense {
            self.max_record = self.max_record.max(record as u64);
            if self.dense_active {
                if self.dense.len() <= record {
                    let len = (record + 1).next_power_of_two().min(DENSE_RECORDS as usize);
                    self.dense.resize(len, (0, 0));
                }
                let slot = &mut self.dense[record];
                if slot.0 == 0 {
                    *slot = (id as u32 + 1, sequence);
                    return None;
                }
                if slot.1 == sequence {
                    return Some(slot.0 as usize - 1);
                }
                // The record was reused while the scan ran (a batch listed
                // before the deletion can still arrive later): the slot keeps
                // the first file, any other sequence number uses the map.
            }
        }
        let first = match self.map.entry(identity) {
            std::collections::hash_map::Entry::Occupied(first) => Some(*first.get()),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(id);
                None
            }
        };
        // Switch once the table would be smaller than the map (~40 B per entry).
        if dense.is_some()
            && !self.dense_active
            && self.map.len() > (1 << 20).max(self.max_record as usize / 5)
        {
            self.densify();
        }
        first
    }

    fn densify(&mut self) {
        let table = &mut self.dense;
        *table = vec![(0, 0); self.max_record as usize + 1];
        self.map
            .retain(|&identity, &mut id| match Self::dense_key(identity, id) {
                // One file per record moves to the table; a second sequence
                // number of a reused record stays in the map.
                Some((record, sequence)) if table[record].0 == 0 => {
                    table[record] = (id as u32 + 1, sequence);
                    false
                }
                Some(_) => true,
                None => true,
            });
        self.map.shrink_to_fit();
        self.dense_active = true;
    }
}

#[cfg(test)]
mod link_table_tests {
    use super::LinkTable;

    #[test]
    fn finds_first_links_before_and_after_switching_to_the_dense_table() {
        let mut table = LinkTable::default();
        let ntfs = |record: u64, sequence: u64| (0, (sequence << 48) | record);
        assert_eq!(table.first(ntfs(40, 1), 0), None);
        assert_eq!(table.first(ntfs(40, 1), 5), Some(0));
        // A reused record (new sequence number) is a different file.
        assert_eq!(table.first(ntfs(40, 2), 6), None);
        assert_eq!(table.first((7, 40), 7), None, "not an NTFS ID: the map");
        for i in 0..(1_u64 << 20) + 2 {
            table.first(ntfs(1000 + i, 1), 10 + i as usize);
        }
        assert!(table.dense_active);
        assert!(table.map.len() <= 2);
        // Both files of the reused record are still known, whichever went where.
        assert_eq!(table.first(ntfs(40, 1), 99), Some(0));
        assert_eq!(table.first(ntfs(40, 2), 99), Some(6));
        assert_eq!(table.first(ntfs(1000, 1), 99), Some(10));
        // Reuse after the switch (record 2000 holds sequence 1 in the table): the
        // new file, a late name of the old one, then the new one again.
        assert_eq!(table.first(ntfs(2000, 2), 200), None);
        assert_eq!(table.first(ntfs(2000, 1), 201), Some(1010));
        assert_eq!(table.first(ntfs(2000, 2), 202), Some(200));
        assert_eq!(table.first((7, 40), 99), Some(7));
        assert_eq!(table.first(ntfs(1 << 26, 1), 3), None, "grows on demand");
        assert_eq!(table.first(ntfs(1 << 26, 1), 4), Some(3));
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
    /// A directory the backend decided not to enter (another or virtual filesystem).
    skipped: bool,
    /// Task key of a subdirectory a worker already queued for reading.
    key: Option<usize>,
}

/// One directory's listing, as a worker sends it to the collector.
#[derive(Default)]
pub(crate) struct Batch {
    entries: Vec<RawEntry>,
    /// Errors for the directory itself (`None`) or for one named child.
    errors: Vec<(Option<Box<OsStr>>, std::io::Error)>,
    /// The listing ran to its natural end.
    complete: bool,
}

/// Builds the entry list. Every retained entry passes through [`Collector::push`],
/// parents before children, whatever the backend.
pub(crate) struct Collector {
    entries: Vec<Entry>,
    summary: Summary,
    links: LinkTable,
    /// Running totals for [`ScanControl::live`], fed by every retained entry.
    live: live::LiveTracker,
}

impl Collector {
    /// Retain one entry below `parent`, resolving hard links, and return its id.
    pub(crate) fn push(&mut self, parent: usize, raw: RawEntry) -> usize {
        let id = self.entries.len();
        let mut duplicate = false;
        if raw.kind == EntryKind::File
            && let Some(identity) = raw.identity
            && let Some(first) = self.links.first(identity, id)
        {
            duplicate = true;
            self.entries[first].multiple_links = true;
        }
        self.accept(
            Entry {
                id,
                parent_id: Some(parent),
                name: raw.name,
                kind: raw.kind,
                logical_bytes: raw.logical,
                unique_logical_bytes: if duplicate { 0 } else { raw.logical },
                allocated_bytes: if duplicate { Some(0) } else { raw.allocated },
                modified_unix_ms: raw.modified,
                file_count: u64::from(raw.kind == EntryKind::File),
                hard_link_duplicate: duplicate,
                multiple_links: duplicate,
                fully_listed: raw.kind != EntryKind::Directory,
            },
            raw.skipped,
        );
        id
    }

    /// Append entries a backend built in parallel (ids already assigned in
    /// order, parents first, hard links resolved).
    #[cfg(windows)]
    pub(crate) fn append(&mut self, entries: Vec<Entry>) {
        self.entries.reserve(entries.len());
        for entry in entries {
            self.accept(entry, false);
        }
    }

    /// The single place where every retained entry, of every backend, is
    /// counted and stored, with its parent id, parents before children.
    /// `skipped`: a folder that was deliberately not entered (a mount point).
    fn accept(&mut self, entry: Entry, skipped: bool) {
        debug_assert_eq!(entry.id, self.entries.len());
        debug_assert!(entry.parent_id.is_some_and(|p| p < entry.id));
        let summary = &mut self.summary;
        match entry.kind {
            EntryKind::Directory => {
                summary.directories += 1;
                // Never listed, so it stays `fully_listed: false`.
                summary.skipped_mounts += u64::from(skipped);
            }
            EntryKind::File => {
                summary.files += 1;
                summary.hard_link_duplicates += u64::from(entry.hard_link_duplicate);
            }
            EntryKind::Link => summary.skipped_links += 1,
            EntryKind::Other => {}
        }
        let parent = entry.parent_id.expect("only the root has no parent");
        self.live.add(
            entry.id,
            parent,
            entry.kind,
            &entry.name,
            entry.logical_bytes,
            entry.allocated_bytes,
        );
        self.entries.push(entry);
    }

    #[cfg(windows)]
    pub(crate) fn mark_multiple_links(&mut self, id: usize) {
        self.entries[id].multiple_links = true;
    }

    #[cfg(windows)]
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub(crate) fn set_fully_listed(&mut self, id: usize, complete: bool) {
        self.entries[id].fully_listed = complete;
    }

    pub(crate) fn issue(&mut self, issue: ScanIssue) {
        add_issue(&mut self.summary, issue);
    }

    /// Report one folder's errors: for the folder itself (`None`) or a named
    /// child. A listable folder without search permission refuses `lstat` on
    /// every entry; that is one protected folder, so it is reported once, on the
    /// folder.
    fn errors(&mut self, id: usize, errors: &[(Option<Box<OsStr>>, std::io::Error)]) {
        let mut denied = false;
        for (name, error) in errors {
            let mut path = entry_path(&self.entries, id);
            match name {
                Some(_) if IssueKind::of(error) == IssueKind::AccessDenied => {
                    if !std::mem::replace(&mut denied, true) {
                        let mut issue = ScanIssue::from_error(path, error);
                        issue.message = format!("entries could not be examined: {error}");
                        self.issue(issue);
                    }
                }
                Some(name) => {
                    path.push(&**name);
                    self.issue(ScanIssue::from_error(path, error));
                }
                None => self.issue(ScanIssue::from_error(path, error)),
            }
        }
    }
}

/// Check options and resolve the root: the canonical (verbatim `\\?\` on Windows)
/// path to walk, so names ending in a dot or a space and reserved device names
/// open exactly, and the familiar form to report.
fn prepare(root: &Path, options: &ScanOptions) -> Result<(PathBuf, PathBuf)> {
    if !(1..=64).contains(&options.threads) {
        bail!("threads must be between 1 and 64");
    }
    if options.max_entries == 0 {
        bail!("max_entries must be greater than zero");
    }
    // Walk with the canonical (verbatim `\\?\`) path so names ending in a dot or a
    // space and reserved device names open exactly; report the familiar form.
    let verbatim = root.canonicalize().map_err(|error| {
        let hint = root_denied_hint(&error);
        anyhow::Error::new(error).context(format!("cannot open {}{hint}", root.display()))
    })?;
    let root = simplify(verbatim.clone());
    if !verbatim.is_dir() {
        bail!("scan root must be a directory");
    }
    Ok((verbatim, root))
}

impl Collector {
    /// A collector holding only the root entry.
    fn new(root: PathBuf, root_modified: Option<u64>) -> Result<Self> {
        let summary = Summary {
            schema_version: 2,
            root: root.clone(),
            backend: "",
            started_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            elapsed_ms: 0,
            files: 0,
            directories: 1,
            skipped_links: 0,
            skipped_mounts: 0,
            hard_link_duplicates: 0,
            logical_bytes: 0,
            unique_logical_bytes: 0,
            allocated_bytes: Some(0),
            issue_count: 0,
            access_denied_count: 0,
            privacy_denied_count: 0,
            issues: Vec::new(),
            cancelled: false,
            complete: true,
            entry_limit_reached: false,
            elevated: is_elevated(),
        };
        let entry = Entry {
            id: 0,
            parent_id: None,
            name: root.into_os_string().into_boxed_os_str(),
            kind: EntryKind::Directory,
            logical_bytes: 0,
            unique_logical_bytes: 0,
            allocated_bytes: Some(0),
            modified_unix_ms: root_modified,
            file_count: 0,
            hard_link_duplicate: false,
            multiple_links: false,
            fully_listed: false,
        };
        Ok(Self {
            entries: vec![entry],
            summary,
            links: LinkTable::default(),
            live: live::LiveTracker::new(),
        })
    }
}

/// Zero the progress counters and refill the entry budget (the root is entry 1).
fn reset_progress(control: &ScanControl, options: &ScanOptions) {
    control.directories.store(1, Ordering::Relaxed);
    control.files.store(0, Ordering::Relaxed);
    control.bytes.store(0, Ordering::Relaxed);
    control
        .remaining_entries
        .store(options.max_entries - 1, Ordering::Relaxed);
    control.limit_reached.store(false, Ordering::Relaxed);
}

fn thread_pool(options: &ScanOptions) -> Result<rayon::ThreadPool> {
    Ok(rayon::ThreadPoolBuilder::new()
        .num_threads(options.threads)
        .thread_name(|i| format!("rilldisk-scan-{i}"))
        .build()?)
}

pub fn scan(root: &Path, options: ScanOptions, control: Arc<ScanControl>) -> Result<Snapshot> {
    let (verbatim, root) = prepare(root, &options)?;
    let start = Instant::now();
    let root_modified = std::fs::symlink_metadata(&verbatim)
        .and_then(|m| m.modified())
        .ok()
        .and_then(unix_ms);
    control.focus_live(0);
    control.set_backend_status("Selecting scan backend".into());
    let fresh = || Collector::new(root.clone(), root_modified);
    let reset = || reset_progress(&control, &options);
    reset();
    if let Ok(mut slot) = control.live.lock() {
        *slot = None;
    }
    let pool = thread_pool(&options)?;
    let mut collector = fresh()?;
    let mut walk_backend = Backend::Directory;
    match options.backend {
        Backend::Mft => {
            let source = platform::mft_source(&verbatim, true)
                .context("the MFT backend needs an NTFS volume and administrator rights")?;
            control.set_backend_status("NTFS MFT scan".into());
            collector.summary.backend = platform::BACKEND_MFT;
            pool.install(|| platform::scan_mft(source, &verbatim, &control, &mut collector))
                .context("cannot read the NTFS Master File Table")?;
            return Ok(pool.install(|| finish(collector, &options, &control, start)));
        }
        Backend::Auto => {
            match platform::mft_source(&verbatim, false) {
                Ok(source) => {
                    control.set_backend_status("NTFS MFT scan".into());
                    collector.summary.backend = platform::BACKEND_MFT;
                    let result = pool.install(|| {
                        platform::scan_mft(source, &verbatim, &control, &mut collector)
                    });
                    // A failed MFT attempt resets entries and progress before falling back.
                    match result {
                        Ok(()) => {
                            return Ok(
                                pool.install(|| finish(collector, &options, &control, start))
                            );
                        }
                        Err(error) if !control.is_cancelled() => {
                            control.set_backend_status(format!(
                                "Directory scan - MFT unavailable: {error}"
                            ));
                            collector = fresh()?;
                            reset();
                        }
                        Err(_) => {
                            collector.summary.backend = platform::BACKEND_MFT;
                            return Ok(finish(collector, &options, &control, start));
                        }
                    }
                }
                Err(error) => {
                    control.set_backend_status(format!("Directory scan - MFT unavailable: {error}"))
                }
            }
            // Preserve no-follow directory opens on Unix as well as Windows.
            if cfg!(unix) {
                control.set_backend_status("Directory scan".into());
            }
        }
        Backend::Directory => control.set_backend_status("Directory scan".into()),
        Backend::Legacy => {
            control.set_backend_status("Legacy directory scan".into());
            walk_backend = Backend::Legacy;
        }
    }
    // Links are never traversed; each backend also keeps the scan on the root's
    // filesystem (Windows: reparse points; Unix: device and mount checks).
    let volume = platform::volume_info(&verbatim, &options, walk_backend)?;
    collector.summary.backend = volume.backend();
    if let Some(warning) = volume.warning() {
        let path = collector.summary.root.clone();
        collector.issue(ScanIssue::new(path, IssueKind::Other, warning));
    }
    walk(&pool, verbatim, volume, &control, &mut collector);
    Ok(pool.install(|| finish(collector, &options, &control, start)))
}

/// Scan an NTFS volume image (a file holding a raw NTFS filesystem) with the
/// MFT backend, as if it were mounted. Read-only; for tests and benchmarks of the
/// MFT parser without administrator rights.
#[doc(hidden)]
#[cfg(windows)]
pub fn scan_ntfs_image(
    image: &Path,
    options: ScanOptions,
    control: Arc<ScanControl>,
) -> Result<Snapshot> {
    let start = Instant::now();
    let file =
        std::fs::File::open(image).with_context(|| format!("cannot open {}", image.display()))?;
    reset_progress(&control, &options);
    let mut collector = Collector::new(image.to_path_buf(), None)?;
    collector.summary.backend = platform::BACKEND_MFT;
    let pool = thread_pool(&options)?;
    pool.install(|| platform::scan_ntfs_image(file, &control, &mut collector))
        .context("cannot read the image's Master File Table")?;
    Ok(pool.install(|| finish(collector, &options, &control, start)))
}

/// Parallel directory walk. Workers queue subfolders the moment they read them,
/// so the pool never waits on this thread or on a slow sibling. This thread only
/// assigns IDs; a batch whose parent has no ID yet waits, so parents always
/// precede children. All workers reserve from one shared budget before
/// retaining an entry.
fn walk(
    pool: &rayon::ThreadPool,
    verbatim: PathBuf,
    volume: platform::VolumeInfo,
    control: &Arc<ScanControl>,
    collector: &mut Collector,
) {
    let (sender, results) = mpsc::channel();
    let walk = Arc::new(Walk {
        control: control.clone(),
        sender,
        pending: AtomicUsize::new(1),
        next_key: AtomicUsize::new(1),
        volume,
    });
    let target = platform::root_target(&verbatim, &walk.volume);
    {
        let walk = walk.clone();
        pool.spawn(move || read_task(walk, 0, target));
    }
    // Task key -> entry id (usize::MAX until the parent's batch is collected).
    let mut ids: Vec<usize> = vec![0];
    // Keys queued for reading whose batch has not been collected yet.
    let mut outstanding = std::collections::HashSet::with_hasher(IdentityHasher);
    outstanding.insert(0_usize);
    let mut waiting: HashMap<usize, Batch> = HashMap::new();
    let mut ready: Vec<(usize, Batch)> = Vec::new();
    let mut last_result = Instant::now();
    let mut last_budget = control.remaining_entries.load(Ordering::Relaxed);
    let mut cancel_seen: Option<Instant> = None;
    let mut stalled = false;
    loop {
        let mut stop = false;
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
                    ready.extend(results.try_iter());
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
                        stalled = true;
                        stop = true;
                    }
                    if !stop {
                        collector
                            .live
                            .publish(&control.live, false, control.live_focus());
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
            outstanding.remove(&key);
            collector.errors(parent, &batch.errors);
            collector.set_fully_listed(parent, batch.complete);
            for raw in batch.entries {
                let child = raw.key;
                let id = collector.push(parent, raw);
                if let Some(child) = child {
                    if ids.len() <= child {
                        ids.resize(child + 1, usize::MAX);
                    }
                    ids[child] = id;
                    outstanding.insert(child);
                    if let Some(early) = waiting.remove(&child) {
                        ready.push((child, early));
                    }
                }
            }
        }
        collector.live.publish(
            &control.live,
            false,
            control.live_focus.load(Ordering::Relaxed),
        );
        if stop {
            break;
        }
    }
    if stalled {
        let mut keys: Vec<usize> = outstanding.into_iter().collect();
        keys.sort_unstable();
        for key in keys {
            if let Some(&id) = ids.get(key).filter(|&&id| id != usize::MAX) {
                let path = entry_path(&collector.entries, id);
                collector.issue(ScanIssue::new(
                    path,
                    IssueKind::Stalled,
                    format!(
                        "not read: no progress for {} s (unresponsive folder or provider)",
                        STALL_TIMEOUT.as_secs()
                    ),
                ));
            }
        }
    }
    // Leftover workers (hung reads, cancelled tails) exit at their next check.
    control.abandoned.store(true, Ordering::Relaxed);
}

/// Totals, rollups and the child index, shared by every backend.
fn finish(
    collector: Collector,
    options: &ScanOptions,
    control: &ScanControl,
    start: Instant,
) -> Snapshot {
    let Collector {
        mut entries,
        mut summary,
        links,
        mut live,
    } = collector;
    live.publish(
        &control.live,
        true,
        control.live_focus.load(Ordering::Relaxed),
    );
    let mut phase = Instant::now();
    control.abandoned.store(true, Ordering::Relaxed);
    drop(links);
    // Report the memory bound once rather than once per truncated directory.
    if control.limit_reached.load(Ordering::Relaxed) {
        summary.entry_limit_reached = true;
        let path = summary.root.clone();
        add_issue(
            &mut summary,
            ScanIssue::new(
                path,
                IssueKind::EntryLimit,
                format!(
                    "entry limit of {} reached; some folders were not scanned (raise --max-entries)",
                    options.max_entries
                ),
            ),
        );
    }
    // Parents always precede their children, so one reverse pass aggregates the tree.
    for id in (1..entries.len()).rev() {
        let parent = entries[id].parent_id.expect("only the root has no parent");
        let logical = entries[id].logical_bytes;
        let unique = entries[id].unique_logical_bytes;
        let allocated = entries[id].allocated_bytes;
        let modified = entries[id].modified_unix_ms;
        let files = entries[id].file_count;
        let target = &mut entries[parent];
        target.modified_unix_ms = target.modified_unix_ms.max(modified);
        target.file_count += files;
        target.logical_bytes = target.logical_bytes.saturating_add(logical);
        target.unique_logical_bytes = target.unique_logical_bytes.saturating_add(unique);
        target.allocated_bytes = target
            .allocated_bytes
            .zip(allocated)
            .map(|(a, b)| a.saturating_add(b));
    }
    trace("rollup", &mut phase);
    summary.logical_bytes = entries[0].logical_bytes;
    summary.unique_logical_bytes = entries[0].unique_logical_bytes;
    summary.allocated_bytes = entries[0].allocated_bytes;
    summary.cancelled = control.is_cancelled();
    summary.complete = !summary.cancelled && summary.issue_count == 0;
    control.files.store(summary.files, Ordering::Relaxed);
    control
        .directories
        .store(summary.directories, Ordering::Relaxed);
    let children = ChildIndex::build(&entries);
    trace("child index", &mut phase);
    summary.elapsed_ms = start.elapsed().as_millis();
    Snapshot {
        summary,
        entries,
        children,
    }
}

/// Profiling aid: with `RILLDISK_TRACE_PHASES=1`, print how long each scan phase
/// took to stderr (local only; nothing is recorded or sent anywhere).
pub(crate) fn tracing() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RILLDISK_TRACE_PHASES").is_some())
}

pub(crate) fn trace(phase: &str, since: &mut Instant) {
    if tracing() {
        eprintln!(
            "phase {phase}: {:.1} ms",
            since.elapsed().as_secs_f64() * 1000.0
        );
    }
    *since = Instant::now();
}

/// Full path of `id`, rebuilt from the parent chain.
fn entry_path(entries: &[Entry], id: usize) -> PathBuf {
    let mut chain = Vec::new();
    let mut cursor = Some(id);
    while let Some(current) = cursor {
        chain.push(&*entries[current].name);
        cursor = entries[current].parent_id;
    }
    let mut path = PathBuf::from(chain.pop().expect("an entry has at least itself"));
    for name in chain.into_iter().rev() {
        path.push(name);
    }
    path
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

/// Guidance for a scan root that could not be opened at all, so there is no
/// summary to count the denial in: the same routes the issue guidance names.
fn root_denied_hint(error: &io::Error) -> &'static str {
    if IssueKind::of(error) != IssueKind::AccessDenied {
        ""
    } else if cfg!(target_os = "macos") && error.raw_os_error() == Some(1) {
        " (access denied: macOS privacy protection, which Full Disk Access may lift, or the sandbox of the app that started RillDisk)"
    } else {
        " (access denied: this folder or a parent cannot be opened by this user; rerunning with administrator rights may read it)"
    }
}

/// `(issues, access denied, privacy denied)` that `issue` adds to the counts.
fn issue_counts(issue: &ScanIssue) -> (u64, u64, u64) {
    let denied = issue.kind == IssueKind::AccessDenied;
    let privacy = cfg!(target_os = "macos") && denied && issue.os_error == Some(1);
    (1, u64::from(denied), u64::from(privacy))
}

fn add_issue(summary: &mut Summary, mut issue: ScanIssue) {
    issue.path = simplify(issue.path);
    // Counted before the cap below, so they hold even when the issue is not listed.
    let (issues, denied, privacy) = issue_counts(&issue);
    summary.issue_count += issues;
    summary.access_denied_count += denied;
    summary.privacy_denied_count += privacy;
    if summary.issues.len() < 100 {
        summary.issues.push(issue);
    }
}

impl Snapshot {
    /// A copy without the subtrees rooted at `removed` (for example after a
    /// cleanup), with their sizes and counts subtracted from every ancestor so
    /// views stay correct without a rescan. Entry IDs are renumbered; the root
    /// (ID 0) is never removed. Hard-link flags of remaining entries are kept, so
    /// a file whose other name was removed still counts as shared until the next
    /// scan: figures err on the side of reporting less space.
    ///
    /// Listed issues inside a removed subtree are dropped and taken out of
    /// `issue_count`, `access_denied_count` and `privacy_denied_count`, so the
    /// denial guidance does not outlive the folders. Issues past the listing cap
    /// carry no path here and stay counted until the next scan: the counts err on
    /// the side of offering a rerun.
    pub fn without(&self, removed: &[usize]) -> Snapshot {
        let count = self.entries.len();
        let mut gone = vec![false; count];
        for &id in removed {
            if id != 0 && id < count {
                gone[id] = true;
            }
        }
        let tops: Vec<usize> = (1..count)
            .filter(|&id| gone[id] && !self.entries[id].parent_id.is_some_and(|p| gone[p]))
            .collect();
        // Parents precede children, so one forward pass marks whole subtrees.
        for id in 1..count {
            if let Some(parent) = self.entries[id].parent_id
                && gone[parent]
            {
                gone[id] = true;
            }
        }
        let mut entries = self.entries.clone();
        for &top in &tops {
            let removed = self.entries[top].clone();
            let mut cursor = removed.parent_id;
            while let Some(parent) = cursor {
                let entry = &mut entries[parent];
                entry.logical_bytes = entry.logical_bytes.saturating_sub(removed.logical_bytes);
                entry.unique_logical_bytes = entry
                    .unique_logical_bytes
                    .saturating_sub(removed.unique_logical_bytes);
                entry.allocated_bytes = entry
                    .allocated_bytes
                    .zip(removed.allocated_bytes)
                    .map(|(a, b)| a.saturating_sub(b));
                entry.file_count = entry.file_count.saturating_sub(removed.file_count);
                cursor = entry.parent_id;
            }
        }
        let mut summary = self.summary.clone();
        let removed_paths: Vec<PathBuf> = tops.iter().map(|&top| self.path(top)).collect();
        let listed = std::mem::take(&mut summary.issues);
        for issue in listed {
            if removed_paths
                .iter()
                .any(|path| issue.path.starts_with(path))
            {
                let (issues, denied, privacy) = issue_counts(&issue);
                summary.issue_count = summary.issue_count.saturating_sub(issues);
                summary.access_denied_count = summary.access_denied_count.saturating_sub(denied);
                summary.privacy_denied_count = summary.privacy_denied_count.saturating_sub(privacy);
            } else {
                summary.issues.push(issue);
            }
        }
        let mut new_id = vec![usize::MAX; count];
        let mut kept = Vec::with_capacity(count);
        for (id, mut entry) in entries.into_iter().enumerate() {
            if gone[id] {
                match entry.kind {
                    EntryKind::File => {
                        summary.files = summary.files.saturating_sub(1);
                        if entry.hard_link_duplicate {
                            summary.hard_link_duplicates =
                                summary.hard_link_duplicates.saturating_sub(1);
                        }
                    }
                    EntryKind::Directory => {
                        summary.directories = summary.directories.saturating_sub(1);
                    }
                    EntryKind::Link => {
                        summary.skipped_links = summary.skipped_links.saturating_sub(1)
                    }
                    EntryKind::Other => {}
                }
                continue;
            }
            new_id[id] = kept.len();
            entry.id = kept.len();
            entry.parent_id = entry.parent_id.map(|p| new_id[p]);
            kept.push(entry);
        }
        summary.logical_bytes = kept[0].logical_bytes;
        summary.unique_logical_bytes = kept[0].unique_logical_bytes;
        summary.allocated_bytes = kept[0].allocated_bytes;
        let children = ChildIndex::build(&kept);
        Snapshot {
            summary,
            entries: kept,
            children,
        }
    }

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
        entry_path(&self.entries, id)
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

#[cfg(test)]
mod mft_progress_tests {
    use super::{MftProgress, ScanControl};
    use std::sync::atomic::Ordering;

    #[test]
    fn only_table_scans_report_table_progress() {
        let control = ScanControl::default();
        assert_eq!(control.mft_progress(), None);
        control.mft_total.store(100, Ordering::Relaxed);
        control.mft_read.store(150, Ordering::Relaxed);
        assert_eq!(
            control.mft_progress(),
            Some(MftProgress {
                records_read: 100,
                records_total: 100,
                building: false
            })
        );
        control.mft_building.store(true, Ordering::Relaxed);
        assert!(control.mft_progress().unwrap().building);
    }
}
