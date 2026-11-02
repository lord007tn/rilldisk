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
use crate::{Batch, EntryKind, IssueKind, RawEntry, ScanControl, ScanIssue, ScanOptions};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs, io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

pub(crate) const BACKEND: &str = "unix-readdir";

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
}

pub(crate) fn volume_info(root: &Path, options: &ScanOptions) -> io::Result<VolumeInfo> {
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

pub(crate) fn read_directory(path: &Path, control: &ScanControl, volume: &VolumeInfo) -> Batch {
    let mut batch = Batch::default();
    match enumerate(path, control, volume, &mut batch) {
        Ok(complete) => batch.complete = complete,
        Err(error) => batch.issues.push(ScanIssue::from_error(path, &error)),
    }
    batch
}

fn enumerate(
    path: &Path,
    control: &ScanControl,
    volume: &VolumeInfo,
    batch: &mut Batch,
) -> io::Result<bool> {
    // A listable folder without search permission refuses `lstat` on every entry.
    // That is one protected folder, so it is reported once, on the folder.
    let mut denied = false;
    let mut entry_error = |batch: &mut Batch, name: &OsStr, error: io::Error| {
        if IssueKind::of(&error) != IssueKind::AccessDenied {
            batch
                .issues
                .push(ScanIssue::from_error(path.join(name), &error));
        } else if !std::mem::replace(&mut denied, true) {
            let mut issue = ScanIssue::from_error(path, &error);
            issue.message = format!("entries could not be examined: {error}");
            batch.issues.push(issue);
        }
    };
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
                entry_error(batch, &name, error);
                continue;
            }
        };
        let mut entry = RawEntry {
            name: name.into_boxed_os_str(),
            kind: EntryKind::Other,
            logical: 0,
            allocated: Some(0),
            modified: None,
            identity: None,
            skipped: false,
            key: None,
        };
        if listed.is_symlink() {
            entry.kind = EntryKind::Link;
        } else if listed.is_file() || listed.is_dir() {
            // Never follows symlinks (`AT_SYMLINK_NOFOLLOW` / `lstat`).
            let metadata = match item.metadata() {
                Ok(value) => value,
                Err(error) => {
                    entry_error(batch, &entry.name, error);
                    continue;
                }
            };
            // The entry may have been replaced since it was listed; trust the metadata.
            let actual = metadata.file_type();
            entry.modified = unix_ms(metadata.mtime(), metadata.mtime_nsec());
            if actual.is_file() {
                entry.kind = EntryKind::File;
                entry.logical = metadata.len();
                // st_blocks is in 512-byte units on Linux and macOS.
                entry.allocated = Some(metadata.blocks().saturating_mul(512));
                // A file with one link cannot be a duplicate; skipping it keeps the
                // hard-link table small.
                entry.identity = (metadata.nlink() > 1).then(|| (metadata.dev(), metadata.ino()));
            } else if actual.is_dir() {
                entry.kind = EntryKind::Directory;
                entry.skipped = volume.skip(path, &entry.name, metadata.dev());
            } else if actual.is_symlink() {
                entry.kind = EntryKind::Link;
            }
        }
        if !control.record(entry.kind, entry.logical) {
            return Ok(false);
        }
        batch.entries.push(entry);
    }
    Ok(true)
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

/// Root (effective UID 0) is elevated.
pub(crate) fn is_elevated() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// `sudo` or `pkexec` on `PATH` is a way to get to root.
pub(crate) fn elevation() -> crate::Elevation {
    let is_elevated = is_elevated();
    crate::Elevation {
        is_elevated,
        can_elevate: !is_elevated && (executable_on_path("sudo") || executable_on_path("pkexec")),
        backup_privilege_enabled: false,
    }
}

/// Unix has no backup privilege: root already lists every folder except macOS
/// privacy-protected ones, which need Full Disk Access instead.
pub(crate) fn enable_backup_privilege() -> bool {
    false
}

/// Whether a regular file named `program` in a `PATH` directory is executable by
/// this user (real UID and groups, as `access(X_OK)` checks).
pub(crate) fn executable_on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            fs::metadata(&candidate).is_ok_and(|m| m.is_file())
                && std::ffi::CString::new(candidate.as_os_str().as_bytes()).is_ok_and(|c| {
                    // SAFETY: `c` is a NUL-terminated path that outlives the call.
                    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
                })
        })
    })
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
