// SPDX-License-Identifier: AGPL-3.0-only
//! Windows backends.
//!
//! * `nt-directory` (default walk): each folder is opened with `NtCreateFile`
//!   relative to its parent's open handle, so no path is rebuilt or re-parsed
//!   from the drive root, and listed with `NtQueryDirectoryFile`
//!   (`FileIdExtdDirectoryInformation`, 128-bit file IDs for ReFS; falls back to
//!   `FileIdBothDirectoryInformation` where unsupported) into a reused 64 KiB
//!   buffer. Reparse points are reported but never opened as folders.
//! * `win32-batched-directory` (`Backend::Legacy`): RillDisk 0.1's walk, which
//!   opens every folder by full path with `CreateFileW`.
//! * `ntfs-mft`: reads the NTFS Master File Table; see [`mft`].
use crate::{Backend, Batch, Collector, EntryKind, RawEntry, ScanControl, ScanIssue, ScanOptions};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    sync::Arc,
};
use windows_sys::{
    Wdk::{
        Foundation::{NtClose, OBJECT_ATTRIBUTES},
        Storage::FileSystem::{
            FILE_DIRECTORY_FILE, FILE_ID_EXTD_DIR_INFORMATION, FILE_INFORMATION_CLASS, FILE_OPEN,
            FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
            FileIdBothDirectoryInformation, FileIdExtdDirectoryInformation, NtCreateFile,
            NtQueryDirectoryFile,
        },
    },
    Win32::{
        Foundation::{
            ERROR_NO_MORE_FILES, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError,
            UNICODE_STRING,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FileAttributeTagInfo, FileIdBothDirectoryInfo,
            GetDiskFreeSpaceExW, GetDiskFreeSpaceW, GetDriveTypeW, GetFileInformationByHandle,
            GetFileInformationByHandleEx, GetLogicalDrives, GetVolumeInformationW,
            GetVolumePathNameW, SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, LUID},
    Security::{
        AdjustTokenPrivileges, GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
        PRIVILEGE_SET, PrivilegeCheck, SE_BACKUP_NAME, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenElevation,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};

pub(crate) mod mft;

use rayon::prelude::*;

pub(crate) const BACKEND_MFT: &str = "ntfs-mft";
const BACKEND_NT: &str = "nt-directory";
const BACKEND_LEGACY: &str = "win32-batched-directory";

const STATUS_NO_MORE_FILES: NTSTATUS = 0x8000_0006_u32 as i32;
const STATUS_INVALID_INFO_CLASS: NTSTATUS = 0xC000_0003_u32 as i32;
const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000D_u32 as i32;
const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010_u32 as i32;
const STATUS_NOT_SUPPORTED: NTSTATUS = 0xC000_00BB_u32 as i32;

/// Bytes per listing call: plenty for hundreds of names, and measured no slower
/// than 256 KiB or 1 MiB (see docs/PERFORMANCE.md).
const LIST_BUFFER: usize = 64 << 10;

/// Where the directory records of one information class keep their name and ID.
/// All classes used here share the leading fields (times, sizes, attributes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    class: FILE_INFORMATION_CLASS,
    name: usize,
    /// Where a reparse point's tag is (IdBoth reuses `EaSize` for it).
    tag: usize,
    id: usize,
    id_len: usize,
    min: usize,
}

const ID_BOTH: Layout = Layout {
    class: FileIdBothDirectoryInformation,
    name: offset_of!(FILE_ID_BOTH_DIR_INFO, FileName),
    tag: offset_of!(FILE_ID_BOTH_DIR_INFO, EaSize),
    id: offset_of!(FILE_ID_BOTH_DIR_INFO, FileId),
    id_len: 8,
    // A record ends after its name: the last one need not be padded to the
    // structure's full size.
    min: offset_of!(FILE_ID_BOTH_DIR_INFO, FileName),
};

const ID_EXTD: Layout = Layout {
    class: FileIdExtdDirectoryInformation,
    name: offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileName),
    tag: offset_of!(FILE_ID_EXTD_DIR_INFORMATION, ReparsePointTag),
    id: offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileId),
    id_len: 16,
    min: offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileName),
};

// The shared leading fields must sit at the same offsets in both classes.
const _: () = {
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, LastWriteTime) == 24);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFORMATION, LastWriteTime) == 24);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, EndOfFile) == 40);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFORMATION, EndOfFile) == 40);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, AllocationSize) == 48);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFORMATION, AllocationSize) == 48);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileAttributes) == 56);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileAttributes) == 56);
    assert!(offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength) == 60);
    assert!(offset_of!(FILE_ID_EXTD_DIR_INFORMATION, FileNameLength) == 60);
};

/// Per-scan volume facts shared by every directory read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VolumeInfo {
    pub allocation_unit: Option<u64>,
    legacy: bool,
    layout: Layout,
}

impl VolumeInfo {
    /// Windows has no scan-wide caveat to report.
    pub(crate) fn warning(&self) -> Option<&str> {
        None
    }
    pub(crate) fn backend(&self) -> &'static str {
        if self.legacy {
            BACKEND_LEGACY
        } else {
            BACKEND_NT
        }
    }
}

/// An open directory handle, closed on drop.
pub(crate) struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from NtCreateFile and is closed exactly once.
        unsafe { NtClose(self.0) };
    }
}

// SAFETY: kernel handles may be used and closed from any thread; children only
// pass this one as the root of a relative open.
unsafe impl Send for Handle {}
// SAFETY: as above; no call mutates the handle value.
unsafe impl Sync for Handle {}

/// What a worker opens: a full path (legacy) or a name relative to the parent's
/// open handle (`None` for the scan root, whose name is then an NT path).
pub(crate) enum Target {
    Path(PathBuf),
    Relative(Option<Arc<Handle>>, Vec<u16>),
}

/// The scan root as a first `Target`.
pub(crate) fn root_target(verbatim: &Path, volume: &VolumeInfo) -> Target {
    if volume.legacy {
        Target::Path(verbatim.to_path_buf())
    } else {
        Target::Relative(None, nt_path(verbatim))
    }
}

/// `\\?\C:\x` (a verbatim Win32 path) as the NT path `\??\C:\x`.
fn nt_path(verbatim: &Path) -> Vec<u16> {
    let wide: Vec<u16> = verbatim.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    let rest = wide.strip_prefix(&prefix[..]).unwrap_or(&wide);
    r"\??\".encode_utf16().chain(rest.iter().copied()).collect()
}

fn nt_error(status: NTSTATUS) -> io::Error {
    // SAFETY: a pure status-code translation.
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}

/// Open a directory for listing, never following a reparse point.
fn nt_open(parent: Option<&Handle>, name: &[u16]) -> io::Result<Handle> {
    let bytes = u16::try_from(name.len() * 2).map_err(|_| io::Error::other("name too long"))?;
    let string = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.map_or(std::ptr::null_mut(), |h| h.0),
        ObjectName: &string,
        // Win32 opens are case-insensitive too; case-sensitive folders still match
        // exactly, and these names come straight from the parent's listing.
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: zeroed IO_STATUS_BLOCK is a valid initial value.
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer refers to a live local for this synchronous call.
    // Backup intent matches the Win32 `FILE_FLAG_BACKUP_SEMANTICS` open used before.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            // FILE_READ_ATTRIBUTES: to recheck a folder whose listing has no ".".
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_FOR_BACKUP_INTENT
                | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    Ok(Handle(handle))
}

/// Fill `buffer` with the next records; Ok(0) at the end of the listing.
fn nt_list(
    handle: &Handle,
    buffer: &mut [u64],
    class: FILE_INFORMATION_CLASS,
    single: bool,
) -> Result<usize, NTSTATUS> {
    // SAFETY: zeroed IO_STATUS_BLOCK is a valid initial value.
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is writable for its length; the handle is synchronous, so
    // the call completes before returning.
    let status = unsafe {
        NtQueryDirectoryFile(
            handle.0,
            std::ptr::null_mut(),
            None,
            std::ptr::null(),
            &mut status_block,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            class,
            single,
            std::ptr::null(),
            single,
        )
    };
    match status {
        STATUS_NO_MORE_FILES => Ok(0),
        status if status < 0 => Err(status),
        _ => Ok(status_block.Information.min(buffer.len() * 8)),
    }
}

/// Reparse points (junctions, symlinks, volume mount points) are never traversed,
/// so a scan stays on the root's volume without any option.
pub(crate) fn volume_info(
    root: &Path,
    _options: &ScanOptions,
    backend: Backend,
) -> io::Result<VolumeInfo> {
    let legacy = backend == Backend::Legacy;
    let mut layout = ID_EXTD;
    if legacy {
        layout = ID_BOTH;
    } else if let Ok(handle) = nt_open(None, &nt_path(root)) {
        // FAT, exFAT and some network redirectors do not offer the extended class.
        let mut probe = vec![0_u64; 512];
        if let Err(
            STATUS_INVALID_INFO_CLASS
            | STATUS_INVALID_PARAMETER
            | STATUS_INVALID_DEVICE_REQUEST
            | STATUS_NOT_SUPPORTED,
        ) = nt_list(&handle, &mut probe, ID_EXTD.class, true)
        {
            layout = ID_BOTH;
        }
    }
    Ok(VolumeInfo {
        allocation_unit: allocation_unit(root)?,
        legacy,
        layout,
    })
}

fn allocation_unit(root: &Path) -> io::Result<Option<u64>> {
    let root: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut volume_path = vec![0_u16; 32768];
    // SAFETY: root is terminated and the output buffer is writable for its stated length.
    if unsafe {
        GetVolumePathNameW(
            root.as_ptr(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let (mut sectors, mut bytes, mut free, mut total) = (0, 0, 0, 0);
    // SAFETY: the volume path is terminated by Win32; all outputs are valid writable integers.
    if unsafe {
        GetDiskFreeSpaceW(
            volume_path.as_ptr(),
            &mut sectors,
            &mut bytes,
            &mut free,
            &mut total,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(u64::from(sectors) * u64::from(bytes)))
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
        Target::Path(path) => legacy_enumerate(&path, control, volume, &mut listing),
        Target::Relative(parent, name) => {
            nt_enumerate(parent, &name, control, volume, &mut listing)
        }
    };
    if let Err(error) = result {
        listing.batch.errors.push((None, error));
    }
    listing
}

thread_local! {
    /// One listing buffer per worker thread, reused for every folder (a fresh
    /// zeroed 64 KiB per folder cost about 10% of a warm walk).
    static BUFFER: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn with_buffer<R>(f: impl FnOnce(&mut [u64]) -> R) -> R {
    BUFFER.with(|cell| {
        let mut buffer = cell.borrow_mut();
        if buffer.len() != LIST_BUFFER / 8 {
            *buffer = vec![0; LIST_BUFFER / 8];
        }
        f(&mut buffer)
    })
}

fn nt_enumerate(
    parent: Option<Arc<Handle>>,
    name: &[u16],
    control: &ScanControl,
    volume: &VolumeInfo,
    listing: &mut Listing,
) -> io::Result<()> {
    let handle = Arc::new(nt_open(parent.as_deref(), name)?);
    // The scan root was resolved (and is not a link) before the walk started.
    let root = parent.is_none();
    // The parent stays open only until its last child has opened.
    drop(parent);
    let mut first = true;
    with_buffer(|buffer| {
        while !control.should_stop() {
            let filled = nt_list(&handle, buffer, volume.layout.class, false).map_err(nt_error)?;
            if filled == 0 {
                listing.batch.complete = true;
                break;
            }
            // SAFETY: the kernel initialized the first `filled` bytes.
            let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), filled) };
            // A folder's own "." record (always first when present) carries its
            // attributes, so a folder that became a reparse point after it was
            // listed shows up there. Without one, ask the handle.
            if std::mem::take(&mut first) && !root && !starts_with_dot(bytes, volume.layout) {
                check_not_reparse(&handle)?;
            }
            if !add_records(bytes, volume, control, listing, |listing, index, wide| {
                listing
                    .children
                    .push((index, Target::Relative(Some(handle.clone()), wide.to_vec())));
            })? {
                return Ok(());
            }
        }
        Ok(())
    })
}

fn starts_with_dot(bytes: &[u8], layout: Layout) -> bool {
    bytes.len() >= layout.name + 2
        && bytes[60..64] == 2_u32.to_le_bytes()
        && bytes[layout.name..layout.name + 2] == [46, 0]
}

fn check_not_reparse(handle: &Handle) -> io::Result<()> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO {
        FileAttributes: 0,
        ReparseTag: 0,
    };
    // SAFETY: info is the documented structure for this class; the handle is open
    // with FILE_READ_ATTRIBUTES.
    if unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && !mft::disguised(info.ReparseTag) {
        return Err(io::Error::other(
            "directory became a reparse point; skipped",
        ));
    }
    Ok(())
}

/// Parse one buffer of records into the listing. Returns false when the entry
/// budget ran out (the folder stays incomplete).
fn add_records(
    bytes: &[u8],
    volume: &VolumeInfo,
    control: &ScanControl,
    listing: &mut Listing,
    mut child: impl FnMut(&mut Listing, usize, &[u16]),
) -> io::Result<bool> {
    let mut offset = 0;
    loop {
        if control.should_stop() {
            return Ok(false);
        }
        let (record, next) = parse_record(bytes, offset, volume.layout)?;
        match record {
            Record::Dot { reparse: true } => {
                return Err(io::Error::other(
                    "directory became a reparse point; skipped",
                ));
            }
            Record::Dot { reparse: false } => {}
            Record::Entry(mut entry, wide) => {
                if !control.record(entry.kind, entry.logical) {
                    // Reported once by the scan; this directory is simply incomplete.
                    return Ok(false);
                }
                // NTFS resident data uses MFT space, not a separately allocated data
                // cluster. Directory records can report its byte-rounded resident
                // length as AllocationSize.
                if entry.kind == EntryKind::File
                    && volume
                        .allocation_unit
                        .zip(entry.allocated)
                        .is_some_and(|(unit, allocated)| allocated < unit)
                {
                    entry.allocated = Some(0);
                }
                let is_dir = entry.kind == EntryKind::Directory;
                listing.batch.entries.push(entry);
                if is_dir {
                    let index = listing.batch.entries.len() - 1;
                    child(listing, index, &wide);
                }
            }
        }
        if next == 0 {
            return Ok(true);
        }
        offset = offset
            .checked_add(next)
            .ok_or_else(|| io::Error::other("invalid directory offset"))?;
    }
}

/// RillDisk 0.1's walk: open by full path, check the attributes, then list with
/// `GetFileInformationByHandleEx` into a fresh 64 KiB buffer.
fn legacy_enumerate(
    path: &Path,
    control: &ScanControl,
    volume: &VolumeInfo,
    listing: &mut Listing,
) -> io::Result<()> {
    let directory = OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    // Recheck the opened handle: a directory may have become a junction since discovery.
    let attributes: FILE_ATTRIBUTE_TAG_INFO = info(&directory, FileAttributeTagInfo)?;
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && !mft::disguised(attributes.ReparseTag)
    {
        return Err(io::Error::other(
            "directory became a reparse point; skipped",
        ));
    }
    let mut storage = vec![0_u64; LIST_BUFFER / 8];
    while !control.should_stop() {
        // SAFETY: storage is writable, aligned, and lives throughout this synchronous call.
        let success = unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                FileIdBothDirectoryInfo,
                storage.as_mut_ptr().cast(),
                (storage.len() * 8) as u32,
            )
        };
        if success == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                listing.batch.complete = true;
                break;
            }
            return Err(error);
        }
        // SAFETY: viewing initialized storage as bytes is valid; parsing validates all offsets.
        let bytes =
            unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), storage.len() * 8) };
        if !add_records(bytes, volume, control, listing, |listing, index, _| {
            let child = path.join(&*listing.batch.entries[index].name);
            listing.children.push((index, Target::Path(child)));
        })? {
            return Ok(());
        }
    }
    Ok(())
}

fn info<T>(file: &File, class: i32) -> io::Result<T> {
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    // SAFETY: callers pair each Win32 class with its exact documented fixed-size structure.
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            value.as_mut_ptr().cast(),
            size_of::<T>() as u32,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call initializes the requested structure.
    Ok(unsafe { value.assume_init() })
}

enum Record {
    /// `.` (with the folder's own attributes) or `..`.
    Dot { reparse: bool },
    /// An entry, and its UTF-16 name (needed to open a subfolder).
    Entry(RawEntry, Vec<u16>),
}

fn parse_record(bytes: &[u8], offset: usize, layout: Layout) -> io::Result<(Record, usize)> {
    let rest = bytes
        .get(offset..)
        .filter(|rest| rest.len() >= layout.min)
        .ok_or_else(|| io::Error::other("truncated directory record"))?;
    let u32_at = |at: usize| u32::from_le_bytes(rest[at..at + 4].try_into().expect("in bounds"));
    let i64_at = |at: usize| i64::from_le_bytes(rest[at..at + 8].try_into().expect("in bounds"));
    let next = u32_at(0) as usize;
    let length = u32_at(60) as usize;
    let attributes = u32_at(56);
    // A reparse point is a link that is not followed, unless its filter driver
    // presents it as an ordinary file or folder (WOF, dedup, cloud files, ProjFS):
    // the same rule as the MFT backend, whether or not this listing disguises it.
    let link =
        attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && !mft::disguised(u32_at(layout.tag));
    if length == 0
        || !length.is_multiple_of(2)
        || layout.name + length > rest.len()
        || (next != 0 && (next < layout.name + length || next > rest.len()))
    {
        return Err(io::Error::other("invalid directory record length"));
    }
    let wide: Vec<u16> = rest[layout.name..layout.name + length]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    if wide == [46] || wide == [46, 46] {
        return Ok((
            Record::Dot {
                reparse: wide.len() == 1 && link,
            },
            next,
        ));
    }
    let kind = if link {
        EntryKind::Link
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    let is_file = kind == EntryKind::File;
    let id = &rest[layout.id..layout.id + layout.id_len];
    let low = u64::from_le_bytes(id[..8].try_into().expect("8 bytes"));
    let high = id
        .get(8..16)
        .map_or(0, |h| u64::from_le_bytes(h.try_into().expect("8 bytes")));
    let entry = RawEntry {
        name: OsString::from_wide(&wide).into_boxed_os_str(),
        kind,
        logical: if is_file { i64_at(40).max(0) as u64 } else { 0 },
        allocated: Some(if is_file { i64_at(48).max(0) as u64 } else { 0 }),
        modified: filetime_to_unix_ms(i64_at(24)),
        identity: (low != 0 || high != 0).then_some((high, low)),
        skipped: false,
        key: None,
    };
    Ok((Record::Entry(entry, wide), next))
}

/// FILETIME counts 100 ns intervals since 1601-01-01.
fn filetime_to_unix_ms(filetime: i64) -> Option<u64> {
    const UNIX_EPOCH_FILETIME: i64 = 116_444_736_000_000_000;
    (filetime >= UNIX_EPOCH_FILETIME).then(|| ((filetime - UNIX_EPOCH_FILETIME) / 10_000) as u64)
}

/// Open the volume behind `root` for the MFT backend. `Auto` only uses it for a
/// whole volume: a subfolder would still pay for reading the entire table.
pub(crate) fn mft_source(root: &Path, explicit: bool) -> io::Result<mft::MftSource> {
    if !explicit && file_reference(root)? & 0xFFFF_FFFF_FFFF != mft::ROOT_RECORD {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "not a volume root",
        ));
    }
    mft::MftSource::open(root)
}

/// The NTFS file reference (sequence << 48 | record number) of `path`.
fn file_reference(path: &Path) -> io::Result<u64> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // SAFETY: zeroed is a valid value for this plain structure.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open and info is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

/// Read the MFT of the volume holding `root` and emit the tree below `root`.
pub(crate) fn scan_mft(
    source: impl mft::ReadAt,
    root: &Path,
    control: &ScanControl,
    collector: &mut Collector,
) -> io::Result<()> {
    let reference = file_reference(root)?;
    let unit = allocation_unit(root)?;
    emit_mft(
        &source,
        reference & 0xFFFF_FFFF_FFFF,
        Some((reference >> 48) as u16),
        unit,
        control,
        collector,
    )
}

/// The MFT backend on an NTFS image file instead of a volume (the whole image).
pub(crate) fn scan_ntfs_image(
    image: File,
    control: &ScanControl,
    collector: &mut Collector,
) -> io::Result<()> {
    emit_mft(
        &mft::ImageSource(image),
        mft::ROOT_RECORD,
        None,
        None,
        control,
        collector,
    )
}

/// Read the table and emit the tree below `root_record`, breadth first, through
/// the collector. A cancelled read leaves only the root (not fully listed).
/// `unit` is the allocation unit (the table's cluster size when None).
fn emit_mft(
    source: &dyn mft::ReadAt,
    root_record: u64,
    root_sequence: Option<u16>,
    unit: Option<u64>,
    control: &ScanControl,
    collector: &mut Collector,
) -> io::Result<()> {
    use std::sync::atomic::Ordering;
    let whole_volume = root_record == mft::ROOT_RECORD;
    // While the table is read, show volume-wide counts (exact only for a whole
    // volume); the exact count restarts when entries are produced.
    let progress = |files: u64, folders: u64| {
        if whole_volume {
            control.files.fetch_add(files, Ordering::Relaxed);
            control.directories.fetch_add(folders, Ordering::Relaxed);
        }
    };
    let mut phase = std::time::Instant::now();
    // Parsing needs ~90 bytes per record whatever the scan keeps, so the entry
    // limit (the memory bound) also bounds the table this backend will read.
    let max_entries = control.remaining_entries.load(Ordering::Relaxed) as u64 + 1;
    let max_records = max_entries.saturating_mul(2);
    let table = match mft::Mft::read(source, max_records, &|| control.should_stop(), &progress) {
        Err(error) if error.kind() == io::ErrorKind::Interrupted && control.should_stop() => {
            return Ok(());
        }
        other => other?,
    };
    control.files.store(0, Ordering::Relaxed);
    control.directories.store(1, Ordering::Relaxed);
    let unit = unit.or(Some(table.cluster));
    table
        .slots
        .get(root_record as usize)
        .filter(|s| s.in_use() && s.is_directory() && root_sequence.is_none_or(|q| s.sequence == q))
        .ok_or_else(|| io::Error::other("the scan root is not in the MFT"))?;
    if table.torn > 0 {
        let path = crate::entry_path(collector.entries(), 0);
        collector.issue(ScanIssue::new(
            path,
            crate::IssueKind::Other,
            format!(
                "{} MFT records changed while they were read and were skipped; files they describe may be missing",
                table.torn
            ),
        ));
    }
    crate::trace("mft read and index", &mut phase);
    // Plan the breadth-first order on indexes only (ids, budget, hard links),
    // then build the entries in parallel: names and sizes for millions of
    // entries are the expensive part.
    let kind_of = |slot: &mft::Slot| {
        if slot.is_link() {
            EntryKind::Link
        } else if slot.is_directory() {
            EntryKind::Directory
        } else {
            EntryKind::File
        }
    };
    let base = collector.entries().len();
    // (name index, parent id, hard-link duplicate)
    let mut plan: Vec<(u32, u32, bool)> = Vec::new();
    // Record -> first entry id + 1, for records with several names.
    let mut first_link: Vec<u32> = Vec::new();
    let mut multiple: Vec<usize> = Vec::new();
    let mut listed: Vec<usize> = Vec::new();
    let mut placeholders = 0_u64;
    let mut queue = std::collections::VecDeque::from([(root_record as u32, 0_usize)]);
    'plan: while let Some((record, id)) = queue.pop_front() {
        if control.should_stop() {
            break;
        }
        for index in table.children(record) {
            // A folder with millions of names must not delay cancellation.
            if index % 4096 == 0 && control.should_stop() {
                break 'plan;
            }
            let name = table.name_at(index);
            let slot = &table.slots[name.record as usize];
            let kind = kind_of(slot);
            if !control.record(
                kind,
                if kind == EntryKind::File {
                    slot.logical
                } else {
                    0
                },
            ) {
                break 'plan;
            }
            let child = base + plan.len();
            let mut duplicate = false;
            if kind == EntryKind::File && slot.links > 1 {
                if first_link.is_empty() {
                    first_link = vec![0; table.slots.len()];
                }
                match first_link[name.record as usize] {
                    0 => first_link[name.record as usize] = child as u32 + 1,
                    first => {
                        duplicate = true;
                        multiple.push(first as usize - 1);
                    }
                }
            }
            plan.push((index, id as u32, duplicate));
            if kind == EntryKind::Directory {
                queue.push_back((name.record, child));
            }
        }
        if table.slots[record as usize].may_lack_children() {
            placeholders += 1;
        } else {
            listed.push(id);
        }
    }
    drop(first_link);
    if placeholders > 0 {
        let path = crate::entry_path(collector.entries(), 0);
        collector.issue(ScanIssue::new(
            path,
            crate::IssueKind::Other,
            format!(
                "{placeholders} cloud or ProjFS placeholder folders keep some contents only at their provider; \
                 the MFT shows what is on this disk (scan with --backend dir to ask the provider)"
            ),
        ));
    }
    if crate::tracing() {
        // Which reparse tags became links (not followed): counts only.
        let mut tags = std::collections::BTreeMap::<u32, u64>::new();
        for &(index, ..) in &plan {
            let slot = &table.slots[table.name_at(index).record as usize];
            if slot.is_link() {
                *tags.entry(slot.reparse_tag).or_default() += 1;
            }
        }
        for (tag, count) in tags {
            eprintln!("mft link tag {tag:#010x}: {count}");
        }
    }
    crate::trace("mft plan", &mut phase);
    let entries: Vec<crate::Entry> = plan
        .par_iter()
        .enumerate()
        .map(|(i, &(index, parent, duplicate))| {
            let name = table.name_at(index);
            let slot = &table.slots[name.record as usize];
            let kind = kind_of(slot);
            let is_file = kind == EntryKind::File;
            let logical = if is_file { slot.logical } else { 0 };
            // Same rule as the directory walk: data that fits in the record has
            // no clusters of its own.
            let allocated = if is_file && unit.is_none_or(|unit| slot.allocated >= unit) {
                slot.allocated
            } else {
                0
            };
            crate::Entry {
                id: base + i,
                parent_id: Some(parent as usize),
                name: table.name(name).into(),
                kind,
                logical_bytes: logical,
                unique_logical_bytes: if duplicate { 0 } else { logical },
                allocated_bytes: Some(if duplicate { 0 } else { allocated }),
                modified_unix_ms: filetime_to_unix_ms(slot.modified),
                file_count: u64::from(is_file),
                hard_link_duplicate: duplicate,
                multiple_links: duplicate,
                fully_listed: kind != EntryKind::Directory,
            }
        })
        .collect();
    drop(plan);
    collector.append(entries);
    for id in multiple {
        collector.mark_multiple_links(id);
    }
    for id in listed {
        collector.set_fully_listed(id, true);
    }
    crate::trace("mft emit", &mut phase);
    Ok(())
}

/// Mounted local and removable drive letters with capacity. Read-only metadata.
pub fn volumes() -> Vec<crate::Volume> {
    // SAFETY: GetLogicalDrives has no inputs.
    let mask = unsafe { GetLogicalDrives() };
    let mut result = Vec::new();
    for index in 0..26_u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = char::from(b'A' + index as u8);
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
        // SAFETY: wide is a terminated root path.
        let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
        if !matches!(drive_type, DRIVE_FIXED | DRIVE_REMOVABLE) {
            continue;
        }
        let (mut free, mut total, mut total_free) = (0_u64, 0_u64, 0_u64);
        // SAFETY: outputs are valid writable integers for this synchronous call.
        if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) }
            == 0
        {
            continue;
        }
        let mut label = vec![0_u16; 261];
        let mut filesystem = vec![0_u16; 261];
        // SAFETY: buffers are writable for their stated lengths; unused outputs are null.
        let named = unsafe {
            GetVolumeInformationW(
                wide.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        } != 0;
        let text = |buffer: &[u16]| {
            let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            String::from_utf16_lossy(&buffer[..end])
        };
        result.push(crate::Volume {
            root: root.into(),
            label: if named { text(&label) } else { String::new() },
            filesystem: if named {
                text(&filesystem)
            } else {
                String::new()
            },
            total_bytes: total,
            free_bytes: total_free,
            removable: drive_type == DRIVE_REMOVABLE,
        });
    }
    result
}

/// `PRIVILEGE_SET_ALL_NECESSARY` (Win32_System_SystemServices, not otherwise needed).
const PRIVILEGE_SET_ALL_NECESSARY: u32 = 1;

/// The process token, closed on drop.
struct Token(HANDLE);

impl Token {
    fn open(access: u32) -> io::Result<Self> {
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle from GetCurrentProcess needs no closing; the output
        // handle is written only on success.
        if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut handle) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn query<T>(&self, class: i32) -> io::Result<T> {
        let mut value = std::mem::MaybeUninit::<T>::zeroed();
        let mut returned = 0;
        // SAFETY: callers pair each token class with its exact fixed-size structure.
        let success = unsafe {
            GetTokenInformation(
                self.0,
                class,
                value.as_mut_ptr().cast(),
                size_of::<T>() as u32,
                &mut returned,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful call initialized the structure.
        Ok(unsafe { value.assume_init() })
    }

    fn backup_privilege_enabled(&self, luid: LUID) -> bool {
        let mut set = PRIVILEGE_SET {
            PrivilegeCount: 1,
            Control: PRIVILEGE_SET_ALL_NECESSARY,
            Privilege: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: 0,
            }],
        };
        let mut result = 0;
        // SAFETY: the set holds exactly the one privilege it declares.
        unsafe { PrivilegeCheck(self.0, &mut set, &mut result) != 0 && result != 0 }
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcessToken and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn backup_luid() -> Option<LUID> {
    let mut luid = LUID::default();
    // SAFETY: SE_BACKUP_NAME is a terminated constant; luid is writable.
    (unsafe { LookupPrivilegeValueW(std::ptr::null(), SE_BACKUP_NAME, &mut luid) } != 0)
        .then_some(luid)
}

impl Token {
    fn is_elevated(&self) -> bool {
        self.query::<TOKEN_ELEVATION>(TokenElevation)
            .is_ok_and(|e| e.TokenIsElevated != 0)
    }
}

pub(crate) fn is_elevated() -> bool {
    Token::open(TOKEN_QUERY).is_ok_and(|token| token.is_elevated())
}

/// Windows offers elevation through UAC; this only serves the shared API.
pub(crate) fn executable_on_path(program: &str) -> bool {
    let pathext = std::env::var("PATHEXT").ok();
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| launchable_in(&dir, program, pathext.as_deref()))
    })
}

/// A file in `dir` that Windows would start for `program`: `program` itself when
/// it already ends in a `PATHEXT` extension, otherwise `program` plus one of them
/// (`.COM;.EXE;.BAT;.CMD` when `PATHEXT` is unset). An extensionless file is never
/// launchable, so it does not count.
fn launchable_in(dir: &Path, program: &str, pathext: Option<&str>) -> bool {
    let extensions: Vec<&str> = pathext
        .unwrap_or(".COM;.EXE;.BAT;.CMD")
        .split(';')
        .map(str::trim)
        .filter(|ext| ext.len() > 1 && ext.starts_with('.'))
        .collect();
    let lower = program.to_ascii_lowercase();
    if extensions
        .iter()
        .any(|ext| lower.ends_with(&ext.to_ascii_lowercase()))
    {
        return dir.join(program).is_file();
    }
    extensions
        .iter()
        .any(|ext| dir.join(format!("{program}{ext}")).is_file())
}

pub(crate) fn elevation() -> crate::Elevation {
    let Ok(token) = Token::open(TOKEN_QUERY) else {
        return crate::Elevation::default();
    };
    let is_elevated = token.is_elevated();
    crate::Elevation {
        is_elevated,
        // UAC offers consent (administrators) or a credentials prompt (standard
        // users) for any unelevated process.
        can_elevate: !is_elevated,
        backup_privilege_enabled: backup_luid().is_some_and(|l| token.backup_privilege_enabled(l)),
    }
}

/// Enables `SeBackupPrivilege` when the token is elevated. Elevated administrator
/// tokens hold it disabled; unelevated tokens do not hold it at all, so this never
/// asks for anything new. Only the enabled flag of an existing privilege changes.
pub(crate) fn enable_backup_privilege() -> bool {
    let Some(luid) = backup_luid() else {
        return false;
    };
    let Ok(token) = Token::open(TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES) else {
        // Adjusting is not allowed; report the privilege as it already is.
        return Token::open(TOKEN_QUERY).is_ok_and(|t| t.backup_privilege_enabled(luid));
    };
    if !token.is_elevated() {
        return token.backup_privilege_enabled(luid);
    }
    let state = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: the state holds exactly the one privilege it declares; no previous
    // state is requested. ERROR_NOT_ALL_ASSIGNED leaves the token unchanged, and
    // PrivilegeCheck below reports the real outcome either way.
    unsafe {
        AdjustTokenPrivileges(
            token.0,
            0,
            &state,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    token.backup_privilege_enabled(luid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_launchable_path_files_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pkexec"), b"").unwrap();
        std::fs::write(dir.path().join("tool.exe"), b"").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"").unwrap();
        let default = |program| launchable_in(dir.path(), program, None);
        // An extensionless file is not something Windows can start.
        assert!(!default("pkexec"));
        assert!(default("tool"));
        assert!(default("TOOL.EXE"));
        assert!(!default("notes.txt"));
        assert!(!default("missing"));
        assert!(launchable_in(dir.path(), "notes", Some(".EXE;.TXT")));
        assert!(!launchable_in(dir.path(), "tool", Some(".COM")));
    }

    #[test]
    fn elevation_is_consistent() {
        let state = elevation();
        assert_eq!(state.can_elevate, !state.is_elevated);
        // Unelevated tokens never gain the privilege.
        if !state.is_elevated {
            assert!(!enable_backup_privilege());
            assert!(!elevation().backup_privilege_enabled);
        }
    }

    #[test]
    fn rejects_invalid_native_records() {
        for layout in [ID_BOTH, ID_EXTD] {
            assert!(parse_record(&[], 0, layout).is_err());
            let mut bytes = vec![0_u8; 256];
            assert!(parse_record(&bytes, 0, layout).is_err());
            bytes[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(parse_record(&bytes, 0, layout).is_err());
        }
    }

    #[test]
    fn verbatim_paths_become_nt_paths() {
        let nt = |p: &str| String::from_utf16(&nt_path(Path::new(p))).unwrap();
        assert_eq!(nt(r"\\?\C:\Users\x"), r"\??\C:\Users\x");
        assert_eq!(nt(r"\\?\UNC\server\share"), r"\??\UNC\server\share");
    }
}
