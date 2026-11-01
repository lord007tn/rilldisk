// SPDX-License-Identifier: AGPL-3.0-only
use crate::{Batch, EntryKind, RawEntry, ScanControl, ScanIssue};
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
    path::Path,
};
use windows_sys::Win32::{
    Foundation::ERROR_NO_MORE_FILES,
    Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO,
        FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileAttributeTagInfo, FileIdBothDirectoryInfo,
        FileIdInfo, GetDiskFreeSpaceW, GetFileInformationByHandleEx, GetVolumePathNameW,
    },
};

pub(crate) const BACKEND: &str = "win32-batched-directory";

pub(crate) fn allocation_unit(root: &Path) -> io::Result<Option<u64>> {
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

pub(crate) fn read_directory(
    path: &Path,
    control: &ScanControl,
    limit: usize,
    allocation_unit: Option<u64>,
) -> Batch {
    let mut batch = Batch {
        entries: Vec::new(),
        issues: Vec::new(),
    };
    if let Err(error) = enumerate(path, control, limit, allocation_unit, &mut batch) {
        batch.issues.push(ScanIssue {
            path: path.into(),
            message: error.to_string(),
        });
    }
    batch
}

fn enumerate(
    path: &Path,
    control: &ScanControl,
    limit: usize,
    allocation_unit: Option<u64>,
    batch: &mut Batch,
) -> io::Result<()> {
    let directory = OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    // Recheck the opened handle: a directory may have become a junction since discovery.
    let attributes: FILE_ATTRIBUTE_TAG_INFO = info(&directory, FileAttributeTagInfo)?;
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(
            "directory became a reparse point; skipped",
        ));
    }
    let volume: FILE_ID_INFO = info(&directory, FileIdInfo)?;
    // 64 KiB, aligned for Win32 records. No file contents or per-file handles are needed.
    let mut storage = vec![0_u64; 8192];
    while !control.is_cancelled() {
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
                break;
            }
            return Err(error);
        }
        // SAFETY: viewing initialized storage as bytes is valid; parsing validates all offsets.
        let bytes =
            unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), storage.len() * 8) };
        let mut offset = 0;
        loop {
            if control.is_cancelled() {
                return Ok(());
            }
            let (entry, next) = parse_record(bytes, offset, path, volume.VolumeSerialNumber)?;
            if let Some(mut entry) = entry {
                if batch.entries.len() >= limit {
                    return Err(io::Error::other("directory entry limit reached"));
                }
                // NTFS resident data uses MFT space, not a separately allocated data cluster.
                // Directory records can report its byte-rounded resident length as AllocationSize.
                if entry.kind == EntryKind::File
                    && allocation_unit
                        .zip(entry.allocated)
                        .is_some_and(|(unit, allocated)| allocated < unit)
                {
                    entry.allocated = Some(0);
                }
                batch.entries.push(entry);
            }
            if next == 0 {
                break;
            }
            offset = offset
                .checked_add(next)
                .ok_or_else(|| io::Error::other("invalid directory offset"))?;
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

fn parse_record(
    bytes: &[u8],
    offset: usize,
    parent: &Path,
    volume: u64,
) -> io::Result<(Option<RawEntry>, usize)> {
    let header = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    let rest = bytes
        .get(offset..)
        .filter(|rest| rest.len() >= size_of::<FILE_ID_BOTH_DIR_INFO>())
        .ok_or_else(|| io::Error::other("truncated directory record"))?;
    // SAFETY: the bounds check covers the entire fixed-size structure. Unaligned reads are allowed.
    let record = unsafe { std::ptr::read_unaligned(rest.as_ptr().cast::<FILE_ID_BOTH_DIR_INFO>()) };
    let length = record.FileNameLength as usize;
    let next = record.NextEntryOffset as usize;
    if !length.is_multiple_of(2)
        || header + length > rest.len()
        || (next != 0 && (next < header + length || next > rest.len()))
    {
        return Err(io::Error::other("invalid directory record length"));
    }
    let wide: Vec<u16> = rest[header..header + length]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    if wide == [46] || wide == [46, 46] {
        return Ok((None, next));
    }
    let kind = if record.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        EntryKind::Link
    } else if record.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    let is_file = kind == EntryKind::File;
    Ok((
        Some(RawEntry {
            path: parent.join(OsString::from_wide(&wide)),
            kind,
            logical: if is_file {
                record.EndOfFile.max(0) as u64
            } else {
                0
            },
            allocated: Some(if is_file {
                record.AllocationSize.max(0) as u64
            } else {
                0
            }),
            identity: (record.FileId != 0).then_some((volume, record.FileId as u64)),
        }),
        next,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_native_records() {
        assert!(parse_record(&[], 0, Path::new("C:\\"), 1).is_err());
        let mut bytes = vec![0_u8; 256];
        let name_length = offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);
        bytes[name_length..name_length + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_record(&bytes, 0, Path::new("C:\\"), 1).is_err());
    }
}
