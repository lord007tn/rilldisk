// SPDX-License-Identifier: AGPL-3.0-only
//! Unix backend (Linux, macOS). `std::fs::read_dir` lists names with their type
//! (`getdents64` on Linux, `readdir` on macOS); files and directories then need one
//! metadata call each (`statx`/`fstatat` relative to the open directory on Linux,
//! `lstat` on macOS) for size, blocks, identity and time. Symlinks and special files
//! need nothing beyond the listing. Symlinks are never followed.
//!
//! A scan stays on the root's filesystem by default: directories on another device
//! and mount points listed by the kernel are recorded but not entered, and counted
//! in `skipped_mounts`. Virtual filesystems (`/proc`, `/sys`, `/dev`, `/run`, tmpfs,
//! overlay and snap images on Linux; `/dev`, `/Volumes` and `/System/Volumes` on
//! macOS) are skipped even when crossing filesystems is allowed.
use crate::{Backend, Batch, EntryKind, RawEntry, ScanControl, ScanOptions};
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, CString, OsStr},
    fs, io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

const BACKEND_FD: &str = if cfg!(any(target_os = "linux", target_os = "android")) {
    "linux-getdents"
} else {
    "unix-openat"
};
const BACKEND_LEGACY: &str = "unix-readdir";

/// Per-scan filesystem facts shared by every directory read.
#[derive(Debug, Clone, Default)]
pub(crate) struct VolumeInfo {
    /// Devices that belong to the scanned filesystem. On macOS, a root on the sealed
    /// system volume also owns the Data volume that firmlinks (`/Users`, `/Applications`)
    /// join into one tree.
    devices: Vec<u64>,
    cross_filesystems: bool,
    /// Mount points strictly inside the root, and whether each one is virtual
    /// (never entered). Catches bind mounts that share the root's device.
    mounts: HashMap<PathBuf, bool>,
    /// Parent directories of `mounts`, so most directories skip the path lookup.
    mount_parents: HashSet<PathBuf>,
    /// Set when the mount table could not be read: bind mounts of the same
    /// filesystem cannot be told apart and may be counted twice.
    warning: Option<String>,
    /// Use RillDisk 0.1's `std::fs::read_dir` walk.
    legacy: bool,
}

pub(crate) fn volume_info(
    root: &Path,
    options: &ScanOptions,
    backend: Backend,
) -> io::Result<VolumeInfo> {
    let device = fs::metadata(root)?.dev();
    #[allow(unused_mut)]
    let mut devices = vec![device];
    #[cfg(target_os = "macos")]
    if fs::metadata("/").is_ok_and(|m| m.dev() == device)
        && let Ok(data) = fs::metadata("/System/Volumes/Data")
        && data.dev() != device
    {
        devices.push(data.dev());
    }
    let mut info = VolumeInfo {
        devices,
        cross_filesystems: options.cross_filesystems,
        legacy: backend == Backend::Legacy,
        ..Default::default()
    };
    let mounts = match read_mount_table() {
        Ok(mounts) => mounts,
        Err(error) => {
            info.warning = Some(format!(
                "cannot read the mount table ({error}); bind mounts inside this folder may be counted twice"
            ));
            Vec::new()
        }
    };
    for mount in mounts {
        if mount.point != root && mount.point.starts_with(root) {
            if let Some(parent) = mount.point.parent() {
                info.mount_parents.insert(parent.to_path_buf());
            }
            let virtual_fs = is_virtual(&mount.fstype);
            info.mounts.insert(mount.point, virtual_fs);
        }
    }
    Ok(info)
}

impl VolumeInfo {
    /// A problem that makes the result less certain, reported as a scan issue.
    pub(crate) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub(crate) fn backend(&self) -> &'static str {
        if self.legacy {
            BACKEND_LEGACY
        } else {
            BACKEND_FD
        }
    }

    /// Whether the directory `parent/name` (on `device`) must not be entered.
    fn skip(&self, parent: &Path, name: &OsStr, device: u64) -> bool {
        if always_skipped(parent, name) {
            return true;
        }
        if !self.cross_filesystems && !self.devices.contains(&device) {
            return true;
        }
        self.mount_parents.contains(parent)
            && self
                .mounts
                .get(&parent.join(name))
                .is_some_and(|&virtual_fs| virtual_fs || !self.cross_filesystems)
    }
}

/// Fixed locations that are never real data on this OS, whatever their mount state.
fn always_skipped(parent: &Path, name: &OsStr) -> bool {
    let name = name.as_bytes();
    let parent = parent.as_os_str().as_bytes();
    if cfg!(any(target_os = "linux", target_os = "android")) {
        parent == b"/" && matches!(name, b"proc" | b"sys" | b"dev" | b"run")
    } else if cfg!(target_os = "macos") {
        // `/System/Volumes/Data` is the same data that `/Users` and other firmlinks show;
        // `/Volumes` holds other disks (and a symlink back to `/`).
        (parent == b"/" && matches!(name, b"dev" | b"Volumes"))
            || (parent == b"/System" && name == b"Volumes")
    } else {
        parent == b"/" && name == b"dev"
    }
}

/// Filesystems that hold no disk data of their own, or only duplicate it.
fn is_virtual(fstype: &str) -> bool {
    matches!(
        fstype,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "devfs"
            | "tmpfs"
            | "ramfs"
            | "cgroup"
            | "cgroup2"
            | "securityfs"
            | "debugfs"
            | "tracefs"
            | "configfs"
            | "fusectl"
            | "mqueue"
            | "hugetlbfs"
            | "pstore"
            | "bpf"
            | "binfmt_misc"
            | "autofs"
            | "efivarfs"
            | "rpc_pipefs"
            | "nsfs"
            | "selinuxfs"
            | "overlay"
            | "squashfs"
            | "fuse.snapfuse"
            | "nullfs"
    )
}

/// How a worker opens one folder: by full path (legacy `std::fs::read_dir`), or
/// by name relative to its parent's open descriptor (`None` for the scan root).
/// The path is kept for the mount checks.
pub(crate) enum Target {
    Path(PathBuf),
    Relative {
        parent: Option<Arc<DirFd>>,
        name: CString,
        path: PathBuf,
    },
}

pub(crate) fn root_target(root: &Path, volume: &VolumeInfo) -> Target {
    if volume.legacy {
        return Target::Path(root.to_path_buf());
    }
    match CString::new(root.as_os_str().as_bytes()) {
        Ok(name) => Target::Relative {
            parent: None,
            name,
            path: root.to_path_buf(),
        },
        Err(_) => Target::Path(root.to_path_buf()),
    }
}

/// One folder's entries, plus the subfolders to read next (index into
/// `batch.entries` and how to open it).
#[derive(Default)]
pub(crate) struct Listing {
    pub batch: Batch,
    pub children: Vec<(usize, Target)>,
}

pub(crate) fn read_directory(
    target: Target,
    control: &ScanControl,
    volume: &VolumeInfo,
) -> Listing {
    let mut listing = Listing::default();
    let result = match target {
        Target::Path(path) => enumerate(&path, control, volume, &mut listing),
        Target::Relative { parent, name, path } => {
            fd_enumerate(parent, &name, &path, control, volume, &mut listing)
        }
    };
    match result {
        Ok(complete) => listing.batch.complete = complete,
        Err(error) => listing.batch.errors.push((None, error)),
    }
    listing
}

/// RillDisk 0.1's walk: `std::fs::read_dir` on the full path, then `lstat`-like
/// metadata per file and folder.
fn enumerate(
    path: &Path,
    control: &ScanControl,
    volume: &VolumeInfo,
    listing: &mut Listing,
) -> io::Result<bool> {
    for item in fs::read_dir(path)? {
        if control.should_stop() {
            return Ok(false);
        }
        let item = item?;
        let name = item.file_name();
        // The listing's type (d_type) is enough for links and special files; an
        // unknown d_type makes std fall back to one lstat here.
        let listed = match item.file_type() {
            Ok(value) => value,
            Err(error) => {
                listing
                    .batch
                    .errors
                    .push((Some(name.into_boxed_os_str()), error));
                continue;
            }
        };
        let mut entry = blank(name.into_boxed_os_str());
        if listed.is_symlink() {
            entry.kind = EntryKind::Link;
        } else if listed.is_file() || listed.is_dir() {
            // Never follows symlinks (`AT_SYMLINK_NOFOLLOW` / `lstat`).
            let metadata = match item.metadata() {
                Ok(value) => value,
                Err(error) => {
                    listing.batch.errors.push((Some(entry.name), error));
                    continue;
                }
            };
            let facts = Facts {
                mode: metadata.mode(),
                size: metadata.len(),
                blocks: metadata.blocks(),
                nlink: metadata.nlink(),
                dev: metadata.dev(),
                ino: metadata.ino(),
                mtime: metadata.mtime(),
                mtime_nsec: metadata.mtime_nsec(),
            };
            apply(&mut entry, &facts, path, volume);
        }
        if !control.record(entry.kind, entry.logical) {
            return Ok(false);
        }
        let enter = entry.kind == EntryKind::Directory && !entry.skipped;
        let child = enter.then(|| path.join(&*entry.name));
        listing.batch.entries.push(entry);
        if let Some(child) = child {
            let index = listing.batch.entries.len() - 1;
            listing.children.push((index, Target::Path(child)));
        }
    }
    Ok(true)
}

fn blank(name: Box<OsStr>) -> RawEntry {
    RawEntry {
        name,
        kind: EntryKind::Other,
        logical: 0,
        allocated: Some(0),
        modified: None,
        identity: None,
        skipped: false,
        key: None,
    }
}

/// The `stat` fields the accounting uses.
struct Facts {
    mode: u32,
    size: u64,
    blocks: u64,
    nlink: u64,
    dev: u64,
    ino: u64,
    mtime: i64,
    mtime_nsec: i64,
}

/// Fill an entry from its metadata. The entry may have been replaced since it
/// was listed; the metadata wins.
// `mode_t` is u32 on Linux but u16 on macOS, so the casts are needed there.
#[allow(clippy::unnecessary_cast)]
fn apply(entry: &mut RawEntry, facts: &Facts, parent: &Path, volume: &VolumeInfo) {
    let format = facts.mode & libc::S_IFMT as u32;
    entry.modified = unix_ms(facts.mtime, facts.mtime_nsec);
    if format == libc::S_IFREG as u32 {
        entry.kind = EntryKind::File;
        entry.logical = facts.size;
        // st_blocks is in 512-byte units on Linux and macOS.
        entry.allocated = Some(facts.blocks.saturating_mul(512));
        // A file with one link cannot be a duplicate; skipping it keeps the
        // hard-link table small.
        entry.identity = (facts.nlink > 1).then_some((facts.dev, facts.ino));
    } else if format == libc::S_IFDIR as u32 {
        entry.kind = EntryKind::Directory;
        entry.skipped = volume.skip(parent, &entry.name, facts.dev);
    } else if format == libc::S_IFLNK as u32 {
        entry.kind = EntryKind::Link;
    } else {
        entry.kind = EntryKind::Other;
    }
}

/// `lstat` of `name` inside the open directory `dir`.
fn stat_at(dir: &DirFd, name: &CStr) -> io::Result<Facts> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: name is terminated and stat is writable; the descriptor is open.
    if unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat succeeded and initialized the structure.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Ok(Facts {
        mode: stat.st_mode as u32,
        size: stat.st_size.max(0) as u64,
        blocks: stat.st_blocks.max(0) as u64,
        nlink: stat.st_nlink as u64,
        dev: stat.st_dev as u64,
        ino: stat.st_ino as u64,
        mtime: stat.st_mtime as i64,
        mtime_nsec: stat.st_mtime_nsec as i64,
    })
}

/// Directory descriptors open right now: the folders being listed plus the
/// parents that queued children keep open for their relative opens.
static OPEN_DIRS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// An open directory, counted in `OPEN_DIRS`.
pub(crate) struct DirFd(OwnedFd);

impl Drop for DirFd {
    fn drop(&mut self) {
        OPEN_DIRS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl AsRawFd for DirFd {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

/// How many directory descriptors queued children may keep open: half the
/// soft descriptor limit (256 by default on macOS). Past it, children open by
/// full path instead, so a deep tree cannot exhaust descriptors.
fn descriptor_budget() -> usize {
    static BUDGET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BUDGET.get_or_init(|| {
        // SAFETY: zeroed rlimit is valid; getrlimit fills it.
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        // SAFETY: limit is writable.
        let soft = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
            limit.rlim_cur as u64
        } else {
            256
        };
        (soft.min(1 << 20) as usize / 2).max(32)
    })
}

/// Open a folder for listing relative to its parent, never following a symlink.
fn open_at(parent: Option<&DirFd>, name: &CStr) -> io::Result<DirFd> {
    // SAFETY: name is terminated; the parent descriptor (if any) is open.
    let fd = unsafe {
        libc::openat(
            parent.map_or(libc::AT_FDCWD, |p| p.as_raw_fd()),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ELOOP) {
            return Err(io::Error::other(
                "directory became a symbolic link; skipped",
            ));
        }
        return Err(error);
    }
    OPEN_DIRS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: openat returned a new descriptor that nothing else owns.
    Ok(DirFd(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// Names of one open directory, with their listed type (`DT_*`).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn for_each_name(
    dir: &DirFd,
    control: &ScanControl,
    mut visit: impl FnMut(&CStr, u8) -> bool,
) -> io::Result<bool> {
    thread_local! {
        /// One `getdents64` buffer per worker, reused for every folder.
        static BUFFER: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    BUFFER.with(|cell| {
        let mut buffer = cell.borrow_mut();
        if buffer.is_empty() {
            *buffer = vec![0; LIST_BUFFER / 8];
        }
        while !control.should_stop() {
            // SAFETY: the buffer is writable for its length; the descriptor is an
            // open directory.
            let read = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    dir.as_raw_fd(),
                    buffer.as_mut_ptr(),
                    buffer.len() * 8,
                )
            };
            if read < 0 {
                return Err(io::Error::last_os_error());
            }
            if read == 0 {
                return Ok(true);
            }
            // SAFETY: the kernel initialized `read` bytes.
            let bytes =
                unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), read as usize) };
            // struct linux_dirent64 { u64 d_ino; i64 d_off; u16 d_reclen; u8 d_type; char d_name[]; }
            let mut at = 0;
            while at + 19 < bytes.len() {
                let length = usize::from(u16::from_ne_bytes([bytes[at + 16], bytes[at + 17]]));
                if length < 20 || at + length > bytes.len() {
                    return Err(io::Error::other("invalid directory record length"));
                }
                let name = CStr::from_bytes_until_nul(&bytes[at + 19..at + length])
                    .map_err(|_| io::Error::other("unterminated directory record"))?;
                let bytes_of = name.to_bytes();
                if bytes_of != b"." && bytes_of != b".." && !visit(name, bytes[at + 18]) {
                    return Ok(false);
                }
                at += length;
            }
        }
        Ok(false)
    })
}

/// Names of one open directory, with their listed type (`DT_*`).
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn for_each_name(
    dir: &DirFd,
    control: &ScanControl,
    mut visit: impl FnMut(&CStr, u8) -> bool,
) -> io::Result<bool> {
    // fdopendir takes ownership of the descriptor it is given, so give it a copy.
    // SAFETY: dup of an open descriptor.
    let copy = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: copy is an open directory descriptor owned by nobody else.
    let stream = unsafe { libc::fdopendir(copy) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so copy is still ours to close.
        unsafe { libc::close(copy) };
        return Err(error);
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: the stream came from fdopendir and is closed once.
            unsafe { libc::closedir(self.0) };
        }
    }
    let stream = Stream(stream);
    loop {
        if control.should_stop() {
            return Ok(false);
        }
        // readdir signals errors only through errno.
        // SAFETY: errno is thread-local.
        #[cfg(target_vendor = "apple")]
        unsafe {
            *libc::__error() = 0
        };
        // SAFETY: the stream is open; the entry is valid until the next readdir.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(0) | None => Ok(true),
                _ => Err(error),
            };
        }
        // SAFETY: readdir returned a valid entry with a terminated name.
        let (name, kind) = unsafe { (CStr::from_ptr((*entry).d_name.as_ptr()), (*entry).d_type) };
        let bytes = name.to_bytes();
        if bytes != b"." && bytes != b".." && !visit(name, kind) {
            return Ok(false);
        }
    }
}

/// Bytes per `getdents64` call.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
const LIST_BUFFER: usize = 64 << 10;

/// The descriptor-relative walk: `openat` from the parent's descriptor, list with
/// `getdents64` (Linux) or `readdir`, and `fstatat` each file and folder relative
/// to the open directory. No path is built per entry.
fn fd_enumerate(
    parent: Option<Arc<DirFd>>,
    name: &CStr,
    path: &Path,
    control: &ScanControl,
    volume: &VolumeInfo,
    listing: &mut Listing,
) -> io::Result<bool> {
    let dir = Arc::new(open_at(parent.as_deref(), name)?);
    // The parent stays open only until its last child has opened.
    drop(parent);
    let mut result = Ok(());
    let complete = for_each_name(&dir, control, |name, listed| {
        let os = OsStr::from_bytes(name.to_bytes());
        let mut entry = blank(os.into());
        match listed {
            libc::DT_LNK => entry.kind = EntryKind::Link,
            libc::DT_REG | libc::DT_DIR | libc::DT_UNKNOWN => match stat_at(&dir, name) {
                Ok(facts) => apply(&mut entry, &facts, path, volume),
                Err(error) => {
                    listing.batch.errors.push((Some(entry.name), error));
                    return true;
                }
            },
            _ => {}
        }
        if !control.record(entry.kind, entry.logical) {
            return false;
        }
        let enter = entry.kind == EntryKind::Directory && !entry.skipped;
        listing.batch.entries.push(entry);
        if enter {
            let index = listing.batch.entries.len() - 1;
            let child = path.join(os);
            // Under descriptor pressure the child opens by full path and does
            // not keep this folder open.
            let relative =
                OPEN_DIRS.load(std::sync::atomic::Ordering::Relaxed) < descriptor_budget();
            let target = if relative {
                CString::new(name.to_bytes()).map(|name| Target::Relative {
                    parent: Some(dir.clone()),
                    name,
                    path: child,
                })
            } else {
                CString::new(child.as_os_str().as_bytes()).map(|name| Target::Relative {
                    parent: None,
                    name,
                    path: child,
                })
            };
            match target {
                Ok(target) => listing.children.push((index, target)),
                Err(error) => result = Err(io::Error::other(error)),
            }
        }
        true
    })?;
    result?;
    Ok(complete)
}

/// The NTFS Master File Table backend exists on Windows only.
pub(crate) const BACKEND_MFT: &str = "ntfs-mft";

pub(crate) enum NoMft {}

pub(crate) fn mft_source(_root: &Path, _explicit: bool) -> io::Result<NoMft> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the MFT backend reads NTFS volumes on Windows only",
    ))
}

pub(crate) fn scan_mft(
    source: NoMft,
    _root: &Path,
    _control: &ScanControl,
    _collector: &mut crate::Collector,
) -> io::Result<()> {
    match source {}
}

fn unix_ms(seconds: i64, nanoseconds: i64) -> Option<u64> {
    (seconds >= 0).then(|| seconds as u64 * 1000 + (nanoseconds.max(0) as u64) / 1_000_000)
}

/// One row of the kernel mount table. Some fields are only filled on one OS.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
struct Mount {
    point: PathBuf,
    fstype: String,
    source: String,
    /// Linux `major:minor`; identical for bind mounts of the same filesystem.
    device: String,
    /// Linux super-block options (for example `aname=drvfs` on WSL drive mounts).
    options: String,
    /// macOS: capacity, free bytes and `f_flags`, read with the table.
    statfs: Option<(u64, u64, u32)>,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn mount_table() -> Vec<Mount> {
    read_mount_table().unwrap_or_default()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_mount_table() -> io::Result<Vec<Mount>> {
    fs::read("/proc/self/mountinfo").map(|bytes| parse_mountinfo(&bytes))
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn read_mount_table() -> io::Result<Vec<Mount>> {
    Ok(mount_table())
}

/// Parse `/proc/self/mountinfo` (see proc(5)): fields 3-5 are `major:minor`, the
/// mounted subtree and the mount point; after the `-` separator come the filesystem
/// type, source and super-block options. Paths use octal escapes such as `\040`.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "android", test)),
    allow(dead_code)
)]
fn parse_mountinfo(bytes: &[u8]) -> Vec<Mount> {
    let mut mounts = Vec::new();
    for line in bytes.split(|&b| b == b'\n') {
        let Some(split) = line.windows(3).position(|w| w == b" - ") else {
            continue;
        };
        let left: Vec<&[u8]> = line[..split].split(|&b| b == b' ').collect();
        let mut right = line[split + 3..].split(|&b| b == b' ');
        if left.len() < 5 {
            continue;
        }
        let text = |field: &[u8]| String::from_utf8_lossy(&unescape(field)).into_owned();
        let (Some(fstype), Some(source)) = (right.next(), right.next()) else {
            continue;
        };
        mounts.push(Mount {
            point: PathBuf::from(OsStr::from_bytes(&unescape(left[4]))),
            fstype: text(fstype),
            source: text(source),
            device: text(left[2]),
            options: right.next().map(text).unwrap_or_default(),
            statfs: None,
        });
    }
    mounts
}

/// Decode the kernel's `\ooo` octal escapes (space, tab, newline, backslash).
fn unescape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\'
            && let Some(code) = field
                .get(i + 1..i + 4)
                .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)))
        {
            let value = code
                .iter()
                .fold(0_u32, |acc, d| acc * 8 + u32::from(d - b'0'));
            if let Ok(byte) = u8::try_from(value) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(field[i]);
        i += 1;
    }
    out
}

#[cfg(target_os = "macos")]
fn mount_table() -> Vec<Mount> {
    read_mount_table().unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn read_mount_table() -> io::Result<Vec<Mount>> {
    use std::ffi::CStr;
    // getfsstat fills a caller-owned buffer, unlike getmntinfo's shared static one,
    // so concurrent scans and drive listings cannot race.
    // SAFETY: a null buffer only asks for the number of mounted filesystems.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    let capacity = count as usize + 8;
    // SAFETY: statfs is plain old data; zeroed values are valid.
    let mut buffer: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; capacity];
    let bytes = (capacity * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    // SAFETY: the buffer is writable for `bytes` bytes for this synchronous call.
    let filled = unsafe { libc::getfsstat(buffer.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if filled < 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(filled as usize);
    let text = |chars: &[libc::c_char]| {
        // SAFETY: the kernel terminates each fixed-size name; the slice bounds the read.
        let bytes: &[u8] = unsafe { &*(chars as *const [libc::c_char] as *const [u8]) };
        CStr::from_bytes_until_nul(bytes)
            .map(|c| c.to_bytes().to_vec())
            .unwrap_or_default()
    };
    Ok(buffer
        .iter()
        .map(|s| {
            let unit = u64::from(s.f_bsize);
            Mount {
                point: PathBuf::from(OsStr::from_bytes(&text(&s.f_mntonname))),
                fstype: String::from_utf8_lossy(&text(&s.f_fstypename)).into_owned(),
                source: String::from_utf8_lossy(&text(&s.f_mntfromname)).into_owned(),
                device: String::new(),
                options: String::new(),
                statfs: Some((
                    s.f_blocks.saturating_mul(unit),
                    s.f_bfree.saturating_mul(unit),
                    s.f_flags,
                )),
            }
        })
        .collect())
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn mount_table() -> Vec<Mount> {
    Vec::new()
}

/// Filesystems that store files on a local or removable disk. Network shares are
/// excluded, matching the Windows listing of fixed and removable drives.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
fn is_disk_filesystem(mount: &Mount) -> bool {
    match mount.fstype.as_str() {
        "ext2" | "ext3" | "ext4" | "xfs" | "btrfs" | "zfs" | "f2fs" | "jfs" | "reiserfs"
        | "bcachefs" | "nilfs2" | "vfat" | "msdos" | "exfat" | "ntfs" | "ntfs3" | "fuseblk"
        | "hfs" | "hfsplus" | "apfs" | "drvfs" => true,
        // WSL mounts Windows drives over 9p with `aname=drvfs`; other 9p mounts are
        // WSL internals or network shares.
        "9p" => mount.options.split([',', ';']).any(|o| o == "aname=drvfs"),
        _ => false,
    }
}

/// Mounted local disks with capacity. Read-only metadata.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn volumes() -> Vec<crate::Volume> {
    let labels = disk_labels();
    linux_volumes(mount_table(), |mount| {
        let point = std::ffi::CString::new(mount.point.as_os_str().as_bytes()).ok()?;
        // SAFETY: statvfs is plain old data; zeroed values are valid.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: point is terminated and stat is writable for this synchronous call.
        if unsafe { libc::statvfs(point.as_ptr(), &mut stat) } != 0 {
            return None;
        }
        let unit = stat.f_frsize as u64;
        Some((
            (stat.f_blocks as u64).saturating_mul(unit),
            (stat.f_bfree as u64).saturating_mul(unit),
        ))
    })
    .into_iter()
    .map(|(mount, total, free)| {
        let source =
            fs::canonicalize(&mount.source).unwrap_or_else(|_| PathBuf::from(&mount.source));
        let label = labels.get(&source).cloned().unwrap_or_else(|| {
            if mount.fstype == "9p" {
                mount.source.clone()
            } else {
                String::new()
            }
        });
        crate::Volume {
            removable: removable(&mount),
            root: mount.point,
            label,
            filesystem: mount.fstype,
            total_bytes: total,
            free_bytes: free,
        }
    })
    .collect()
}

/// Select listable mounts: disk filesystems, one row per device (bind mounts and
/// repeated mounts share `major:minor`; the shortest mount point wins), skipping
/// boot partitions and anything without capacity.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "android", test)),
    allow(dead_code)
)]
fn linux_volumes(
    mut mounts: Vec<Mount>,
    capacity: impl Fn(&Mount) -> Option<(u64, u64)>,
) -> Vec<(Mount, u64, u64)> {
    mounts.retain(|m| is_disk_filesystem(m) && !m.point.starts_with("/boot"));
    mounts.sort_by_key(|m| (m.point.as_os_str().len(), m.point.clone()));
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for mount in mounts {
        if !mount.device.is_empty() && seen.contains(&mount.device) {
            continue;
        }
        if let Some((total, free)) = capacity(&mount).filter(|(total, _)| *total > 0) {
            seen.insert(mount.device.clone());
            result.push((mount, total, free));
        }
    }
    result.sort_by(|a, b| a.0.point.cmp(&b.0.point));
    result
}

/// `/dev/disk/by-label` links name each labelled block device.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn disk_labels() -> HashMap<PathBuf, String> {
    let mut labels = HashMap::new();
    let Ok(listing) = fs::read_dir("/dev/disk/by-label") else {
        return labels;
    };
    for item in listing.flatten() {
        if let Ok(device) = fs::canonicalize(item.path()) {
            let name = item.file_name();
            // udev escapes spaces and other unsafe bytes as `\x20`.
            let label = String::from_utf8_lossy(&unescape_hex(name.as_bytes())).into_owned();
            labels.insert(device, label);
        }
    }
    labels
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "android", test)),
    allow(dead_code)
)]
fn unescape_hex(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'\\'
            && text.get(i + 1) == Some(&b'x')
            && let Some(byte) = text
                .get(i + 2..i + 4)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 4;
            continue;
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// The block device's sysfs `removable` flag (on the disk, not the partition), or
/// a desktop automount location.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn removable(mount: &Mount) -> bool {
    if mount.point.starts_with("/media") || mount.point.starts_with("/run/media") {
        return true;
    }
    let Ok(device) = fs::canonicalize(format!("/sys/dev/block/{}", mount.device)) else {
        return false;
    };
    [device.join("removable"), device.join("../removable")]
        .iter()
        .any(|flag| fs::read(flag).is_ok_and(|v| v.first() == Some(&b'1')))
}

/// Mounted local disks with capacity: `/` and browsable volumes under `/Volumes`.
#[cfg(target_os = "macos")]
pub fn volumes() -> Vec<crate::Volume> {
    // XNU <sys/mount.h>; not exported by the libc crate.
    const MNT_REMOVABLE: u32 = 0x0000_0200;
    // Finder names the startup disk with a `/Volumes/<name>` symlink back to `/`.
    let startup = fs::read_dir("/Volumes")
        .into_iter()
        .flatten()
        .flatten()
        .find(|item| fs::read_link(item.path()).is_ok_and(|t| t == Path::new("/")))
        .map(|item| item.file_name().to_string_lossy().into_owned());
    let mut result: Vec<crate::Volume> = mount_table()
        .into_iter()
        .filter_map(|mount| {
            let (total, free, flags) = mount.statfs?;
            let root = mount.point == Path::new("/");
            let browsable = (flags & libc::MNT_DONTBROWSE as u32) == 0
                && (flags & libc::MNT_LOCAL as u32) != 0
                && !matches!(mount.fstype.as_str(), "devfs" | "autofs" | "nullfs");
            (browsable && total > 0 && (root || mount.point.starts_with("/Volumes/"))).then(|| {
                crate::Volume {
                    label: if root {
                        startup.clone().unwrap_or_default()
                    } else {
                        mount
                            .point
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    },
                    root: mount.point,
                    filesystem: mount.fstype,
                    total_bytes: total,
                    free_bytes: free,
                    removable: flags & MNT_REMOVABLE != 0,
                }
            })
        })
        .collect();
    result.sort_by(|a, b| a.root.cmp(&b.root));
    result
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
pub fn volumes() -> Vec<crate::Volume> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &[u8] = br"22 1 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw
23 22 0:21 / /proc rw,nosuid - proc proc rw
24 22 0:5 / /dev rw - devtmpfs udev rw
25 22 0:24 / /run rw - tmpfs tmpfs rw
26 22 8:2 /srv/data /mnt/bind\040point rw - ext4 /dev/sda2 rw
27 22 8:17 / /media/me/USB\040KEY rw - vfat /dev/sdb1 rw
28 22 7:3 / /snap/core/1 ro - squashfs /dev/loop3 ro
29 22 8:1 / /boot/efi rw - vfat /dev/sda1 rw
30 22 0:40 / /mnt/c rw - 9p C:\134 rw,aname=drvfs;path=C:\;uid=1000
31 22 0:41 / /usr/lib/wsl/drivers ro - 9p drivers ro,aname=drivers;fmask=222
32 22 0:42 / /home rw - btrfs /dev/sda3 rw,subvol=/home
33 22 0:43 / /mnt/nas rw - nfs4 nas:/export rw
";

    #[test]
    fn parses_mountinfo_with_escapes() {
        let mounts = parse_mountinfo(MOUNTINFO);
        assert_eq!(mounts.len(), 12);
        assert_eq!(mounts[4].point, Path::new("/mnt/bind point"));
        assert_eq!(mounts[4].device, "8:2");
        assert_eq!(mounts[5].point, Path::new("/media/me/USB KEY"));
        assert_eq!(mounts[8].source, r"C:\");
        assert_eq!(mounts[8].options, r"rw,aname=drvfs;path=C:\;uid=1000");
        assert!(parse_mountinfo(b"garbage\n\n1 2 3\n").is_empty());
    }

    #[test]
    fn lists_disk_mounts_once_per_device() {
        let volumes = linux_volumes(parse_mountinfo(MOUNTINFO), |_| Some((100, 40)));
        let points: Vec<_> = volumes.iter().map(|(m, ..)| m.point.clone()).collect();
        // The bind mount repeats `/`'s device; pseudo, snap, boot and network mounts drop.
        assert_eq!(
            points,
            ["/", "/home", "/media/me/USB KEY", "/mnt/c"].map(PathBuf::from)
        );
        assert!(linux_volumes(parse_mountinfo(MOUNTINFO), |_| Some((0, 0))).is_empty());
    }

    #[test]
    fn a_device_is_listed_through_any_mount_that_reports_capacity() {
        // `/` cannot be measured, so its device appears through the bind mount.
        let volumes = linux_volumes(parse_mountinfo(MOUNTINFO), |m| {
            (m.point != Path::new("/")).then_some((100, 40))
        });
        let points: Vec<_> = volumes.iter().map(|(m, ..)| m.point.clone()).collect();
        assert!(
            points.contains(&PathBuf::from("/mnt/bind point")),
            "{points:?}"
        );
    }

    #[test]
    fn skips_virtual_and_foreign_filesystems() {
        let mut info = VolumeInfo {
            devices: vec![1],
            ..Default::default()
        };
        for mount in parse_mountinfo(MOUNTINFO).into_iter().skip(1) {
            if let Some(parent) = mount.point.parent() {
                info.mount_parents.insert(parent.to_path_buf());
            }
            info.mounts.insert(mount.point, is_virtual(&mount.fstype));
        }
        fn skip(info: &VolumeInfo, parent: &str, name: &str, device: u64) -> bool {
            info.skip(Path::new(parent), OsStr::new(name), device)
        }
        assert!(!skip(&info, "/", "usr", 1));
        assert!(skip(&info, "/", "home", 2), "another device");
        assert!(
            skip(&info, "/mnt", "bind point", 1),
            "bind mount on the same device"
        );
        info.cross_filesystems = true;
        assert!(!skip(&info, "/", "home", 2));
        assert!(!skip(&info, "/mnt", "bind point", 1));
        assert!(
            skip(&info, "/snap/core", "1", 2),
            "snap images stay skipped"
        );
        assert!(
            !skip(&info, "/srv", "proc", 1),
            "only the top-level pseudo folders"
        );
        if cfg!(target_os = "linux") {
            for name in ["proc", "sys", "dev", "run"] {
                assert!(skip(&info, "/", name, 1), "{name}");
            }
        }
        if cfg!(target_os = "macos") {
            assert!(skip(&info, "/", "Volumes", 1));
            assert!(skip(&info, "/System", "Volumes", 1));
            assert!(!skip(&info, "/", "Users", 1));
        }
    }

    #[test]
    fn decodes_kernel_and_udev_escapes() {
        assert_eq!(unescape(br"a\040b\134c\0"), b"a b\\c\\0");
        assert_eq!(unescape_hex(br"My\x20Disk\x2"), b"My Disk\\x2");
        assert_eq!(unix_ms(-5, 0), None);
        assert_eq!(unix_ms(2, 345_678_901), Some(2345));
    }
}
