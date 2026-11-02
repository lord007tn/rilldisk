// SPDX-License-Identifier: AGPL-3.0-only
//! Read-only NTFS Master File Table backend.
//!
//! Every file and folder on an NTFS volume has a 1 KiB (or 4 KiB) record in the
//! MFT holding its names (with parent folder references), timestamps, attributes
//! and data sizes. Reading the MFT sequentially in large blocks replaces millions
//! of directory opens with a few gigabytes of streaming reads, which is why tools
//! such as WizTree finish a whole drive in seconds. Opening the volume for reading
//! needs administrator rights; [`MftSource::open`] fails otherwise and the scan
//! falls back to the directory walk.
//!
//! The records are read raw from the volume, so they reflect what NTFS has
//! written to disk: changes from the last few seconds may still be in memory.
//! Each record is validated (signature, multi-sector fixups, attribute bounds);
//! a record that fails is skipped and counted as an issue.
//!
//! Accounting matches the directory listing: a file's logical size is the
//! unnamed `$DATA` stream's length; allocation is its on-disk allocation (the
//! compressed/sparse total when present), zero for data stored inside the MFT
//! record; folders have zero size; any reparse point (junction, symlink, mount
//! point, cloud placeholder) is a link that is not entered; each hard link is one
//! entry and duplicates are found by file reference number. NTFS metadata files
//! (record numbers below 16 and everything under `$Extend`) are not listed, like
//! in a directory listing.
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{FileExt, OpenOptionsExt},
    },
    path::Path,
    sync::Mutex,
};

use rayon::prelude::*;

const FILE_MAGIC: &[u8; 4] = b"FILE";
const RECORD_IN_USE: u16 = 0x0001;
const RECORD_IS_DIRECTORY: u16 = 0x0002;

const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE_POINT: u32 = 0xC0;

const TAG_DEDUP: u32 = 0x8000_0013;
const TAG_WOF: u32 = 0x8000_0017;
const TAG_PROJFS: u32 = 0x9000_001C;
/// `IO_REPARSE_TAG_CLOUD` and its 16 variants `0x9000_x01A`.
const TAG_CLOUD_MASK: u32 = 0xFFFF_0FFF;
const TAG_CLOUD: u32 = 0x9000_001A;

/// The named stream that holds a WOF-compressed (CompactOS) file's data.
const WOF_STREAM: &str = "WofCompressedData";
const ATTR_END: u32 = 0xFFFF_FFFF;

const ATTR_FLAG_COMPRESSED: u16 = 0x0001;
const ATTR_FLAG_SPARSE: u16 = 0x8000;

const NAMESPACE_DOS: u8 = 2;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// NTFS applies update-sequence fixups every 512 bytes, whatever the sector size.
const FIXUP_STRIDE: usize = 512;
/// Records 0-15 are NTFS metadata (`$MFT`, `$Bitmap`, `$Extend`, ...), hidden from
/// directory listings. Record 5 is the root folder.
const FIRST_USER_RECORD: u64 = 16;
pub(crate) const ROOT_RECORD: u64 = 5;
/// Bytes per read. Large enough to stream at full device speed, small enough that
/// every worker has some.
const CHUNK_BYTES: u64 = 4 << 20;

/// Flags kept per record after parsing.
const SLOT_IN_USE: u8 = 0x01;
const SLOT_DIRECTORY: u8 = 0x02;
const SLOT_REPARSE: u8 = 0x04;
const SLOT_EXTENSION: u8 = 0x08;
const SLOT_HAS_DATA: u8 = 0x10;
const SLOT_TORN: u8 = 0x20;

/// One MFT record, after parsing. Indexed by record number.
#[derive(Clone, Copy, Default)]
pub(crate) struct Slot {
    pub flags: u8,
    pub sequence: u16,
    /// Extension records: the base record's sequence number.
    base_sequence: u16,
    /// Extension records: the base record number.
    base: u32,
    /// Names (hard links) this record has in listed folders.
    pub links: u16,
    /// Reparse tag (`IO_REPARSE_TAG_*`) when the record is a reparse point.
    pub reparse_tag: u32,
    /// Last write time, FILETIME (100 ns since 1601), or 0.
    pub modified: i64,
    pub logical: u64,
    pub allocated: u64,
}

impl Slot {
    pub fn in_use(&self) -> bool {
        self.flags & SLOT_IN_USE != 0 && self.flags & SLOT_EXTENSION == 0
    }
    pub fn is_directory(&self) -> bool {
        self.flags & SLOT_DIRECTORY != 0
    }
    pub fn is_reparse(&self) -> bool {
        self.flags & SLOT_REPARSE != 0
    }
    /// A reparse point that a directory listing shows as one: junctions,
    /// symlinks, mount points, app execution aliases and the like, which are
    /// never followed. WOF compression (CompactOS), deduplication, cloud-file
    /// placeholders (OneDrive) and ProjFS are reparse points too, but their
    /// filter drivers present them to programs as ordinary files and folders,
    /// so the walk lists, sizes and enters them; the MFT backend does the same.
    pub fn is_link(&self) -> bool {
        self.is_reparse() && !disguised(self.reparse_tag)
    }
}

fn disguised(tag: u32) -> bool {
    matches!(tag, TAG_WOF | TAG_DEDUP | TAG_PROJFS) || tag & TAG_CLOUD_MASK == TAG_CLOUD
}

/// Whether a non-resident or resident attribute is named `name`.
fn attribute_named(raw: &[u8], name: &str) -> bool {
    let (Some(&len), Some(at)) = (raw.get(9), u16_at(raw, 0x0A)) else {
        return false;
    };
    let at = usize::from(at);
    raw.get(at..at + usize::from(len) * 2).is_some_and(|bytes| {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .eq(name.encode_utf16())
    })
}

/// One (parent, name) link of a record. Hard links have several.
#[derive(Clone, Copy)]
pub(crate) struct Name {
    pub parent: u32,
    pub parent_sequence: u16,
    len: u16,
    pub record: u32,
    chunk: u32,
    offset: u32,
}

/// Parsed output of one block of records.
#[derive(Default)]
struct Chunk {
    names: Vec<Name>,
    /// Names, each one either UTF-8 or bytes from `OsStr::as_encoded_bytes`.
    text: Vec<u8>,
    torn: u64,
}

/// Size facts from the boot sector and `$MFT`'s own record.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    cluster: u64,
    record: u64,
    mft_offset: u64,
}

/// A contiguous run of the `$MFT` file on the volume: `bytes` starting at file
/// offset `vcn_byte` live at volume offset `disk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extent {
    vcn_byte: u64,
    bytes: u64,
    disk: Option<u64>,
}

/// Positioned reads from a volume (or, in tests and benchmarks, an image file).
pub(crate) trait ReadAt: Sync {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()>;
    /// Reads must start and end on this boundary (sector size for raw volumes).
    fn alignment(&self) -> usize {
        1
    }
}

/// Raw read access to an NTFS volume. One handle per concurrent reader, because
/// a synchronous handle serializes its I/O.
pub(crate) struct MftSource {
    handles: Mutex<Vec<File>>,
    path: Vec<u16>,
    unbuffered: bool,
}

impl MftSource {
    /// Open the volume holding `root` for raw reading. Fails without administrator
    /// rights, or when the volume is not NTFS.
    pub(crate) fn open(root: &Path) -> io::Result<Self> {
        let volume = volume_device(root)?;
        let source = Self {
            handles: Mutex::new(Vec::new()),
            path: volume,
            unbuffered: true,
        };
        // Fail now (access denied, not NTFS) rather than in a worker.
        let handle = source.open_handle()?;
        source.release(handle);
        Ok(source)
    }

    fn open_handle(&self) -> io::Result<File> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_NO_BUFFERING, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let path = OsString::from_wide(&self.path[..self.path.len() - 1]);
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(if self.unbuffered {
                FILE_FLAG_NO_BUFFERING
            } else {
                0
            })
            .open(path)
    }

    fn take(&self) -> io::Result<File> {
        if let Some(file) = self.handles.lock().ok().and_then(|mut h| h.pop()) {
            return Ok(file);
        }
        self.open_handle()
    }

    fn release(&self, file: File) {
        if let Ok(mut handles) = self.handles.lock() {
            handles.push(file);
        }
    }
}

impl ReadAt for MftSource {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let file = self.take()?;
        let result = read_exact_at(&file, offset, buffer);
        self.release(file);
        result
    }
    fn alignment(&self) -> usize {
        // Covers 512-byte and 4 KiB sectors.
        4096
    }
}

/// An NTFS image file, read through the cache. Used by tests and benchmarks.
pub(crate) struct ImageSource(pub File);

impl ReadAt for ImageSource {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        read_exact_at(&self.0, offset, buffer)
    }
}

fn read_exact_at(file: &File, mut offset: u64, mut buffer: &mut [u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        match file.seek_read(buffer, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buffer = &mut buffer[n..];
                offset += n as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// `\\?\Volume{GUID}` (no trailing separator, NUL-terminated) for the volume
/// that holds `root`, after checking that it is NTFS.
fn volume_device(root: &Path) -> io::Result<Vec<u16>> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut mount = vec![0_u16; 32768];
    // SAFETY: wide is terminated; mount is writable for its stated length.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut filesystem = [0_u16; 64];
    // SAFETY: mount is terminated by Win32; unused outputs are null.
    let named = unsafe {
        GetVolumeInformationW(
            mount.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    } != 0;
    let end = filesystem.iter().position(|&c| c == 0).unwrap_or(0);
    if !named || filesystem[..end] != *"NTFS".encode_utf16().collect::<Vec<_>>() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the volume is not NTFS",
        ));
    }
    let mut name = vec![0_u16; 64];
    // SAFETY: mount is terminated; name is writable for its stated length.
    if unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), name.as_mut_ptr(), name.len() as u32)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let mut device: Vec<u16> = name[..end].to_vec();
    // `\\?\Volume{...}\` names the root folder; without the separator, the volume.
    if device.last() == Some(&u16::from(b'\\')) {
        device.pop();
    }
    device.push(0);
    Ok(device)
}

thread_local! {
    /// One read buffer per worker, reused for every block.
    static BUFFER: std::cell::RefCell<Aligned> = const {
        std::cell::RefCell::new(Aligned { storage: Vec::new(), start: 0, len: 0 })
    };
}

/// A buffer whose start is aligned for unbuffered I/O.
struct Aligned {
    storage: Vec<u8>,
    start: usize,
    len: usize,
}

impl Aligned {
    fn new(len: usize, alignment: usize) -> Self {
        let storage = vec![0_u8; len + alignment];
        let start = storage.as_ptr().align_offset(alignment);
        Self {
            storage,
            start,
            len,
        }
    }
    fn bytes(&mut self) -> &mut [u8] {
        &mut self.storage[self.start..self.start + self.len]
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn geometry(boot: &[u8]) -> io::Result<Geometry> {
    let invalid = || io::Error::other("not an NTFS boot sector");
    if boot.get(3..11) != Some(b"NTFS    ") {
        return Err(invalid());
    }
    let sector = u64::from(u16_at(boot, 0x0B).ok_or_else(invalid)?);
    let per_cluster = u64::from(*boot.get(0x0D).ok_or_else(invalid)?);
    // Values above 0x80 are a negative power of two (large clusters).
    let per_cluster = if per_cluster > 0x80 {
        1 << (256 - per_cluster)
    } else {
        per_cluster
    };
    let cluster = sector * per_cluster;
    let mft_lcn = u64_at(boot, 0x30).ok_or_else(invalid)?;
    let per_record = *boot.get(0x40).ok_or_else(invalid)? as i8;
    let record = if per_record < 0 {
        1_u64 << u32::from(per_record.unsigned_abs())
    } else {
        per_record as u64 * cluster
    };
    if !sector.is_power_of_two()
        || !cluster.is_power_of_two()
        || !record.is_power_of_two()
        || !(512..=4096).contains(&sector)
        || !(1024..=65536).contains(&record)
    {
        return Err(invalid());
    }
    Ok(Geometry {
        cluster,
        record,
        mft_offset: mft_lcn.checked_mul(cluster).ok_or_else(invalid)?,
    })
}

/// Apply the update-sequence fixups in place. False if the record is torn
/// (a sector was written after the others) or malformed.
fn fixup(record: &mut [u8]) -> bool {
    let (Some(offset), Some(count)) = (u16_at(record, 4), u16_at(record, 6)) else {
        return false;
    };
    let (offset, count) = (offset as usize, count as usize);
    // One check value plus one saved pair per 512-byte stride: the array must
    // cover the whole record, or a damaged sector could go unnoticed.
    if count != record.len() / FIXUP_STRIDE + 1 || offset + count * 2 > record.len() {
        return false;
    }
    let check = [record[offset], record[offset + 1]];
    for i in 1..count {
        let end = i * FIXUP_STRIDE - 2;
        if record[end..end + 2] != check {
            return false;
        }
        record[end] = record[offset + i * 2];
        record[end + 1] = record[offset + i * 2 + 1];
    }
    true
}

/// One attribute's header fields that matter here.
struct Attribute<'a> {
    kind: u32,
    named: bool,
    flags: u16,
    /// Resident value, or None for non-resident attributes.
    value: Option<&'a [u8]>,
    /// The whole attribute (header included).
    raw: &'a [u8],
}

/// The record's attributes in order. `Some(None)` (then the end) marks a
/// malformed record: an attribute out of bounds, `bytes_in_use` beyond the
/// record, or no end marker within the bytes in use.
fn attributes(record: &[u8]) -> impl Iterator<Item = Option<Attribute<'_>>> {
    let used = u32_at(record, 0x18)
        .map(|u| u as usize)
        .filter(|&u| u <= record.len());
    let mut offset = u16_at(record, 0x14).map_or(usize::MAX, usize::from);
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let Some(kind) = used.and_then(|used| u32_at(&record[..used], offset)) else {
            done = true;
            return Some(None);
        };
        if kind == ATTR_END {
            done = true;
            return None;
        }
        let used = used.expect("checked above");
        let parse = || {
            let length = u32_at(record, offset + 4)? as usize;
            if length < 0x18 || offset.checked_add(length)? > used || !length.is_multiple_of(8) {
                return None;
            }
            let raw = &record[offset..offset + length];
            let named = raw[9] != 0;
            let flags = u16_at(raw, 0x0C)?;
            let value = if raw[8] == 0 {
                let len = u32_at(raw, 0x10)? as usize;
                let at = usize::from(u16_at(raw, 0x14)?);
                Some(raw.get(at..at.checked_add(len)?)?)
            } else {
                if length < 0x40 {
                    return None;
                }
                None
            };
            Some((
                Attribute {
                    kind,
                    named,
                    flags,
                    value,
                    raw,
                },
                length,
            ))
        };
        match parse() {
            Some((attribute, length)) => {
                offset += length;
                Some(Some(attribute))
            }
            None => {
                done = true;
                Some(None)
            }
        }
    })
}

/// Size facts of the unnamed `$DATA` stream, or None for a fragment that does
/// not start at VCN 0 (sizes live in the first fragment only).
fn data_sizes(attribute: &Attribute<'_>) -> Option<(u64, u64)> {
    if let Some(value) = attribute.value {
        // Resident: the data lives inside the MFT record and has no clusters of
        // its own, whatever the cluster size.
        return Some((value.len() as u64, 0));
    }
    let raw = attribute.raw;
    if u64_at(raw, 0x10)? != 0 {
        return None;
    }
    let allocated = u64_at(raw, 0x28)?;
    let logical = u64_at(raw, 0x30)?;
    // Compressed and sparse streams also record what is actually allocated.
    let total = (attribute.flags & (ATTR_FLAG_COMPRESSED | ATTR_FLAG_SPARSE) != 0
        && raw.len() >= 0x48)
        .then(|| u64_at(raw, 0x40))
        .flatten();
    Some((logical, total.unwrap_or(allocated)))
}

/// Decode a non-resident attribute's mapping pairs into (vcn, lcn, clusters) runs.
fn runs(attribute: &[u8]) -> Option<Vec<(u64, Option<u64>, u64)>> {
    let mut vcn = u64_at(attribute, 0x10)?;
    let mut at = usize::from(u16_at(attribute, 0x20)?);
    let mut lcn: i64 = 0;
    let mut result = Vec::new();
    loop {
        let header = *attribute.get(at)?;
        if header == 0 {
            break;
        }
        let (len_size, off_size) = ((header & 0x0F) as usize, (header >> 4) as usize);
        if len_size == 0 || len_size > 8 || off_size > 8 {
            return None;
        }
        let field = |start: usize, size: usize, signed: bool| -> Option<i64> {
            let bytes = attribute.get(start..start + size)?;
            let mut value: i64 = 0;
            for (i, &b) in bytes.iter().enumerate() {
                value |= i64::from(b) << (8 * i);
            }
            if signed && size < 8 && bytes.last()? & 0x80 != 0 {
                value |= -1_i64 << (8 * size);
            }
            Some(value)
        };
        let length = field(at + 1, len_size, false)? as u64;
        let disk = if off_size == 0 {
            None
        } else {
            lcn = lcn.checked_add(field(at + 1 + len_size, off_size, true)?)?;
            Some(u64::try_from(lcn).ok()?)
        };
        result.push((vcn, disk, length));
        vcn = vcn.checked_add(length)?;
        at += 1 + len_size + off_size;
    }
    Some(result)
}

/// Parse one record into its slot (and, for its names, the chunk). The record's
/// fixups are applied in place.
fn parse_record(record: &mut [u8], index: u64, slot: &mut Slot, chunk: &mut Chunk, id: u32) {
    *slot = Slot::default();
    if record.get(0..4) != Some(FILE_MAGIC) {
        return;
    }
    let flags = u16_at(record, 0x16).unwrap_or(0);
    if flags & RECORD_IN_USE == 0 {
        return;
    }
    if !fixup(record) {
        slot.flags = SLOT_TORN;
        chunk.torn += 1;
        return;
    }
    let record: &[u8] = record;
    slot.sequence = u16_at(record, 0x10).unwrap_or(0);
    slot.flags = SLOT_IN_USE;
    if flags & RECORD_IS_DIRECTORY != 0 {
        slot.flags |= SLOT_DIRECTORY;
    }
    let base = u64_at(record, 0x20).unwrap_or(0);
    if base != 0 {
        slot.flags |= SLOT_EXTENSION;
        slot.base = (base & 0xFFFF_FFFF_FFFF) as u32;
        slot.base_sequence = (base >> 48) as u16;
    }
    let names_before = chunk.names.len();
    let text_before = chunk.text.len();
    for attribute in attributes(record) {
        let Some(attribute) = attribute else {
            // Bounds did not check out: drop what this record contributed.
            chunk.names.truncate(names_before);
            chunk.text.truncate(text_before);
            *slot = Slot {
                flags: SLOT_TORN,
                ..Slot::default()
            };
            chunk.torn += 1;
            return;
        };
        match attribute.kind {
            ATTR_STANDARD_INFORMATION => {
                if let Some(value) = attribute.value {
                    slot.modified = u64_at(value, 0x08).unwrap_or(0) as i64;
                    if u32_at(value, 0x20).unwrap_or(0) & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        slot.flags |= SLOT_REPARSE;
                    }
                }
            }
            ATTR_FILE_NAME => {
                let Some(value) = attribute.value else {
                    continue;
                };
                let (Some(parent), Some(&len), Some(&namespace)) =
                    (u64_at(value, 0), value.get(0x40), value.get(0x41))
                else {
                    continue;
                };
                // 8.3 aliases name the same link again.
                if namespace == NAMESPACE_DOS {
                    continue;
                }
                let Some(utf16) = value.get(0x42..0x42 + usize::from(len) * 2) else {
                    continue;
                };
                let offset = chunk.text.len() as u32;
                encode_name(utf16, &mut chunk.text);
                chunk.names.push(Name {
                    parent: (parent & 0xFFFF_FFFF_FFFF) as u32,
                    parent_sequence: (parent >> 48) as u16,
                    len: (chunk.text.len() - offset as usize) as u16,
                    record: index as u32,
                    chunk: id,
                    offset,
                });
            }
            ATTR_DATA if !attribute.named => {
                if let Some((logical, allocated)) = data_sizes(&attribute) {
                    slot.flags |= SLOT_HAS_DATA;
                    slot.logical = logical;
                    slot.allocated += allocated;
                }
            }
            // A WOF-compressed file keeps its logical size on the (sparse)
            // unnamed stream and its data here: count what this stream occupies.
            ATTR_DATA if attribute_named(attribute.raw, WOF_STREAM) => {
                if let Some((_, allocated)) = data_sizes(&attribute) {
                    slot.allocated += allocated;
                }
            }
            ATTR_REPARSE_POINT => {
                if let Some(tag) = attribute.value.and_then(|v| u32_at(v, 0)) {
                    slot.reparse_tag = tag;
                }
            }
            _ => {}
        }
    }
}

/// Append a UTF-16 name: UTF-8 when it is valid Unicode, otherwise the bytes
/// `OsStr::as_encoded_bytes` produces for it, so `from_encoded_bytes_unchecked`
/// can rebuild it exactly.
fn encode_name(utf16: &[u8], text: &mut Vec<u8>) {
    let units = utf16
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair));
    if units.clone().all(|u| u < 0x80) {
        text.extend(units.map(|u| u as u8));
        return;
    }
    let start = text.len();
    for decoded in char::decode_utf16(units.clone()) {
        match decoded {
            Ok(c) => {
                let mut buffer = [0; 4];
                text.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            }
            Err(_) => {
                text.truncate(start);
                let wide: Vec<u16> = units.collect();
                text.extend_from_slice(OsString::from_wide(&wide).as_encoded_bytes());
                return;
            }
        }
    }
}

/// The whole MFT, parsed: one slot per record and every name, grouped by parent.
pub(crate) struct Mft {
    pub slots: Vec<Slot>,
    /// Sorted by (parent, record); children of `p` are `names[first[p]..first[p+1]]`.
    names: Vec<Name>,
    first: Vec<u32>,
    texts: Vec<Vec<u8>>,
    /// Records that failed validation (torn by a concurrent write, or corrupt).
    pub torn: u64,
    /// Bytes per cluster.
    pub cluster: u64,
}

impl Mft {
    /// Read and index every record. `stop` is polled between blocks; `progress`
    /// receives the files and folders found in each block.
    /// Tables with more than `max_records` records are refused before anything
    /// is allocated for them (parsing needs ~90 bytes per record).
    pub(crate) fn read(
        source: &dyn ReadAt,
        max_records: u64,
        stop: &(dyn Fn() -> bool + Sync),
        progress: &(dyn Fn(u64, u64) + Sync),
    ) -> io::Result<Self> {
        let alignment = source.alignment();
        let mut boot = Aligned::new(4096, alignment);
        source.read_at(0, boot.bytes())?;
        let geometry = geometry(boot.bytes())?;
        let (extents, valid) = mft_extents(source, &geometry)?;
        // Bytes past the valid data length are undefined (often old records).
        let records = valid / geometry.record;
        if records > u64::from(u32::MAX) || records <= FIRST_USER_RECORD {
            return Err(io::Error::other("implausible MFT size"));
        }
        if records > max_records {
            return Err(io::Error::other(format!(
                "the MFT has {records} records, more than the entry limit allows reading \
                 ({max_records}); raise --max-entries or scan with --backend dir"
            )));
        }
        let unit = geometry.record.max(geometry.cluster);
        let chunk_bytes = CHUNK_BYTES.div_ceil(unit) * unit;
        let per_chunk = (chunk_bytes / geometry.record) as usize;
        let mut phase = std::time::Instant::now();
        let mut slots = vec![Slot::default(); records as usize];
        let chunks: Vec<io::Result<Chunk>> = slots
            .par_chunks_mut(per_chunk)
            .enumerate()
            .map(|(i, slots)| {
                if stop() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let start = i as u64 * chunk_bytes;
                let len = (slots.len() as u64 * geometry.record).div_ceil(unit) * unit;
                BUFFER.with(|cell| {
                    let mut buffer = cell.borrow_mut();
                    if buffer.len < len as usize
                        || !(buffer.storage.as_ptr() as usize + buffer.start)
                            .is_multiple_of(alignment)
                    {
                        *buffer = Aligned::new(chunk_bytes as usize, alignment);
                    }
                    let bytes = &mut buffer.bytes()[..len as usize];
                    read_mapped(source, &extents, start, bytes)?;
                    let mut chunk = Chunk::default();
                    let first = start / geometry.record;
                    let (mut files, mut folders) = (0, 0);
                    for (j, slot) in slots.iter_mut().enumerate() {
                        let at = j * geometry.record as usize;
                        let record = &mut bytes[at..at + geometry.record as usize];
                        parse_record(record, first + j as u64, slot, &mut chunk, i as u32);
                        if slot.in_use() {
                            if slot.is_directory() {
                                folders += 1;
                            } else {
                                files += 1;
                            }
                        }
                    }
                    progress(files, folders);
                    Ok(chunk)
                })
            })
            .collect();
        crate::trace("mft read and parse", &mut phase);
        let mut texts = Vec::with_capacity(chunks.len());
        let mut names = Vec::new();
        let mut torn = 0;
        for chunk in chunks {
            let chunk = chunk?;
            torn += chunk.torn;
            names.extend_from_slice(&chunk.names);
            texts.push(chunk.text);
        }
        // Extension records carry what did not fit in the base record: overflow
        // names (heavily hard-linked files), the first `$DATA` fragment of very
        // fragmented files, a WOF file's compressed stream, `$REPARSE_POINT`.
        // Fold them into their base record.
        for index in 0..slots.len() {
            let slot = slots[index];
            if slot.flags & SLOT_EXTENSION == 0 || slot.flags & SLOT_IN_USE == 0 {
                continue;
            }
            let Some(base) = slots.get(slot.base as usize).copied() else {
                continue;
            };
            if !base.in_use() || base.sequence != slot.base_sequence {
                continue;
            }
            let target = &mut slots[slot.base as usize];
            if slot.flags & SLOT_HAS_DATA != 0 && target.flags & SLOT_HAS_DATA == 0 {
                target.flags |= SLOT_HAS_DATA;
                target.logical = slot.logical;
            }
            // Allocation of the unnamed stream's first fragment and of a WOF
            // stream, whichever this record holds (each is counted once: only the
            // fragment that starts at VCN 0 reports sizes).
            target.allocated += slot.allocated;
            if target.reparse_tag == 0 {
                target.reparse_tag = slot.reparse_tag;
            }
        }
        for name in &mut names {
            let slot = slots[name.record as usize];
            if slot.flags & SLOT_EXTENSION != 0 {
                let base = slots.get(slot.base as usize);
                name.record =
                    if base.is_some_and(|b| b.in_use() && b.sequence == slot.base_sequence) {
                        slot.base
                    } else {
                        u32::MAX
                    };
            }
        }
        crate::trace("mft merge", &mut phase);
        // Keep links whose record and parent folder are live user entries.
        names.retain(|name| {
            let Some(slot) = slots.get(name.record as usize) else {
                return false;
            };
            let parent = slots.get(name.parent as usize);
            slot.in_use()
                && u64::from(name.record) >= FIRST_USER_RECORD
                && name.record != name.parent
                && parent.is_some_and(|p| {
                    p.in_use() && p.is_directory() && p.sequence == name.parent_sequence
                })
                && (u64::from(name.parent) >= FIRST_USER_RECORD
                    || u64::from(name.parent) == ROOT_RECORD)
        });
        for name in &names {
            let links = &mut slots[name.record as usize].links;
            *links = links.saturating_add(1);
        }
        names.par_sort_unstable_by_key(|name| (name.parent, name.record, name.chunk, name.offset));
        crate::trace("mft filter and sort", &mut phase);
        let mut first = vec![0_u32; slots.len() + 1];
        for name in &names {
            first[name.parent as usize + 1] += 1;
        }
        for i in 1..first.len() {
            first[i] += first[i - 1];
        }
        Ok(Self {
            slots,
            names,
            first,
            texts,
            torn,
            cluster: geometry.cluster,
        })
    }

    /// Links (names) directly inside folder `record`, in record order.
    /// Indexes into [`Mft::name_at`] of the links directly inside folder
    /// `record`, in record order.
    pub(crate) fn children(&self, record: u32) -> std::ops::Range<u32> {
        let record = record as usize;
        match (self.first.get(record), self.first.get(record + 1)) {
            (Some(&a), Some(&b)) => a..b,
            _ => 0..0,
        }
    }

    pub(crate) fn name_at(&self, index: u32) -> &Name {
        &self.names[index as usize]
    }

    pub(crate) fn name(&self, name: &Name) -> &OsStr {
        let bytes = &self.texts[name.chunk as usize]
            [name.offset as usize..name.offset as usize + usize::from(name.len)];
        // SAFETY: `encode_name` wrote either valid UTF-8 or the complete output of
        // `OsStr::as_encoded_bytes`, and this slice is exactly one such name.
        unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
    }
}

/// The extents of `$MFT` itself, from record 0 (and its attribute list, when the
/// MFT is fragmented enough to need extension records).
/// Also returns the valid data length (bytes of `$MFT` that hold records).
fn mft_extents(source: &dyn ReadAt, geometry: &Geometry) -> io::Result<(Vec<Extent>, u64)> {
    let invalid = |what: &str| io::Error::other(format!("cannot read $MFT: {what}"));
    let alignment = source.alignment();
    let unit = geometry.record.max(geometry.cluster).max(alignment as u64);
    let read_record = |offset: u64| -> io::Result<Vec<u8>> {
        let aligned = offset / unit * unit;
        let mut buffer = Aligned::new(
            ((offset - aligned + geometry.record).div_ceil(unit) * unit) as usize,
            alignment,
        );
        source.read_at(aligned, buffer.bytes())?;
        let at = (offset - aligned) as usize;
        let mut record = buffer.bytes()[at..at + geometry.record as usize].to_vec();
        if record.get(0..4) != Some(FILE_MAGIC) || !fixup(&mut record) {
            return Err(invalid("bad record"));
        }
        Ok(record)
    };
    let base = read_record(geometry.mft_offset)?;
    let mut fragments: Vec<Vec<u8>> = Vec::new();
    let mut list: Option<Vec<u8>> = None;
    for attribute in attributes(&base) {
        let attribute = attribute.ok_or_else(|| invalid("bad attribute"))?;
        match attribute.kind {
            ATTR_DATA if !attribute.named && attribute.value.is_none() => {
                fragments.push(attribute.raw.to_vec())
            }
            ATTR_ATTRIBUTE_LIST => {
                list = Some(match attribute.value {
                    Some(value) => value.to_vec(),
                    None => read_nonresident(source, geometry, attribute.raw)?,
                })
            }
            _ => {}
        }
    }
    let first = fragments.first().ok_or_else(|| invalid("no $DATA"))?;
    let valid = u64_at(first, 0x38).ok_or_else(|| invalid("no size"))?;
    let mut extents = to_extents(first, geometry).ok_or_else(|| invalid("bad runs"))?;
    if let Some(list) = list {
        // Entries: type u32, length u16, name length u8, name offset u8,
        // lowest VCN u64, segment reference u64, attribute id u16.
        let mut at = 0;
        while at + 0x1A <= list.len() {
            let kind = u32_at(&list, at).unwrap_or(ATTR_END);
            let length = usize::from(u16_at(&list, at + 4).unwrap_or(0));
            if length == 0 {
                break;
            }
            let lowest = u64_at(&list, at + 8).unwrap_or(0);
            let segment = u64_at(&list, at + 0x10).unwrap_or(0) & 0xFFFF_FFFF_FFFF;
            if kind == ATTR_DATA && list[at + 6] == 0 && lowest != 0 && segment != 0 {
                let offset = record_offset(&extents, segment * geometry.record)
                    .ok_or_else(|| invalid("extension record outside the known runs"))?;
                let record = read_record(offset)?;
                for attribute in attributes(&record) {
                    let attribute = attribute.ok_or_else(|| invalid("bad attribute"))?;
                    if attribute.kind == ATTR_DATA && !attribute.named && attribute.value.is_none()
                    {
                        extents.extend(
                            to_extents(attribute.raw, geometry)
                                .ok_or_else(|| invalid("bad runs"))?,
                        );
                    }
                }
            }
            at += length;
        }
        extents.sort_by_key(|e| e.vcn_byte);
        extents.dedup();
    }
    // The runs must cover the file from offset 0 without gaps or overlaps, and
    // every run must be on disk ($MFT is never sparse); anything else would
    // silently hide (or invent) records.
    let mut expected = 0;
    for extent in &extents {
        if extent.vcn_byte != expected || extent.bytes == 0 || extent.disk.is_none() {
            return Err(invalid("runs do not cover the table continuously"));
        }
        expected += extent.bytes;
    }
    // Only records below the valid data length hold anything. Reads still cover
    // whole clusters, as unbuffered I/O requires.
    let mut remaining = valid.div_ceil(geometry.cluster) * geometry.cluster;
    extents.retain_mut(|extent| {
        if remaining == 0 {
            return false;
        }
        extent.bytes = extent.bytes.min(remaining);
        remaining -= extent.bytes;
        true
    });
    if remaining != 0 {
        return Err(invalid("runs end before the valid data"));
    }
    Ok((extents, valid))
}

fn to_extents(attribute: &[u8], geometry: &Geometry) -> Option<Vec<Extent>> {
    runs(attribute)?
        .into_iter()
        .map(|(vcn, lcn, clusters)| {
            Some(Extent {
                vcn_byte: vcn.checked_mul(geometry.cluster)?,
                bytes: clusters.checked_mul(geometry.cluster)?,
                disk: lcn.map(|l| l.checked_mul(geometry.cluster)).unwrap_or(None),
            })
        })
        .collect()
}

fn record_offset(extents: &[Extent], vcn_byte: u64) -> Option<u64> {
    extents
        .iter()
        .find(|e| (e.vcn_byte..e.vcn_byte + e.bytes).contains(&vcn_byte))
        .and_then(|e| Some(e.disk? + vcn_byte - e.vcn_byte))
}

fn read_nonresident(source: &dyn ReadAt, geometry: &Geometry, raw: &[u8]) -> io::Result<Vec<u8>> {
    let invalid = || io::Error::other("cannot read the $MFT attribute list");
    let size = u64_at(raw, 0x30).ok_or_else(invalid)?;
    let extents = to_extents(raw, geometry).ok_or_else(invalid)?;
    let total: u64 = extents.iter().map(|e| e.bytes).sum();
    if size > total || size > 64 << 20 {
        return Err(invalid());
    }
    let mut buffer = Aligned::new(total as usize, source.alignment());
    read_mapped(source, &extents, 0, buffer.bytes())?;
    Ok(buffer.bytes()[..size as usize].to_vec())
}

/// Fill `buffer` with the file bytes starting at file offset `start`.
fn read_mapped(
    source: &dyn ReadAt,
    extents: &[Extent],
    start: u64,
    buffer: &mut [u8],
) -> io::Result<()> {
    let end = start + buffer.len() as u64;
    let mut filled = 0;
    for extent in extents {
        let from = extent.vcn_byte.max(start);
        let to = (extent.vcn_byte + extent.bytes).min(end);
        if from >= to {
            continue;
        }
        let piece = &mut buffer[(from - start) as usize..(to - start) as usize];
        match extent.disk {
            Some(disk) => source.read_at(disk + (from - extent.vcn_byte), piece)?,
            None => piece.fill(0),
        }
        filled += to - from;
    }
    // Extents never overlap (checked when they are built), so this means every
    // requested byte came from the volume rather than a previous block.
    if filled != buffer.len() as u64 {
        return Err(io::Error::other("$MFT runs do not cover a requested block"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a FILE record with fixups applied the way NTFS writes them.
    fn record(size: usize, flags: u16, sequence: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0_u8; size];
        r[0..4].copy_from_slice(FILE_MAGIC);
        let count = size / FIXUP_STRIDE + 1;
        r[4..6].copy_from_slice(&0x30_u16.to_le_bytes());
        r[6..8].copy_from_slice(&(count as u16).to_le_bytes());
        r[0x10..0x12].copy_from_slice(&sequence.to_le_bytes());
        let first = (0x30 + count * 2).div_ceil(8) * 8;
        r[0x14..0x16].copy_from_slice(&(first as u16).to_le_bytes());
        r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
        r[0x20..0x28].copy_from_slice(&base.to_le_bytes());
        let mut at = first;
        for attr in attrs {
            r[at..at + attr.len()].copy_from_slice(attr);
            at += attr.len();
        }
        r[at..at + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        r[0x18..0x1C].copy_from_slice(&((at + 8) as u32).to_le_bytes());
        // Update sequence: the check value replaces each stride's last two bytes.
        let usn = [0xAB_u8, 0xCD];
        r[0x30..0x32].copy_from_slice(&usn);
        for i in 1..count {
            let end = i * FIXUP_STRIDE - 2;
            r[0x30 + i * 2] = r[end];
            r[0x30 + i * 2 + 1] = r[end + 1];
            r[end..end + 2].copy_from_slice(&usn);
        }
        r
    }

    fn resident(kind: u32, value: &[u8], name: &[u16]) -> Vec<u8> {
        let header = 0x18 + name.len() * 2;
        let len = (header + value.len()).div_ceil(8) * 8;
        let mut a = vec![0_u8; len];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[9] = name.len() as u8;
        a[0x0A..0x0C].copy_from_slice(&0x18_u16.to_le_bytes());
        for (i, c) in name.iter().enumerate() {
            a[0x18 + i * 2..0x1A + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
        a[0x14..0x16].copy_from_slice(&(header as u16).to_le_bytes());
        a[header..header + value.len()].copy_from_slice(value);
        a
    }

    fn nonresident(
        flags: u16,
        lowest: u64,
        allocated: u64,
        logical: u64,
        total: u64,
        runs: &[u8],
    ) -> Vec<u8> {
        let len = (0x48 + runs.len() + 1).div_ceil(8) * 8;
        let mut a = vec![0_u8; len];
        a[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = 1;
        a[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
        a[0x10..0x18].copy_from_slice(&lowest.to_le_bytes());
        a[0x20..0x22].copy_from_slice(&0x48_u16.to_le_bytes());
        a[0x28..0x30].copy_from_slice(&allocated.to_le_bytes());
        a[0x30..0x38].copy_from_slice(&logical.to_le_bytes());
        a[0x38..0x40].copy_from_slice(&logical.to_le_bytes());
        a[0x40..0x48].copy_from_slice(&total.to_le_bytes());
        a[0x48..0x48 + runs.len()].copy_from_slice(runs);
        a
    }

    fn file_name(parent: u64, parent_seq: u16, namespace: u8, name: &str) -> Vec<u8> {
        let wide: Vec<u16> = name.encode_utf16().collect();
        let mut v = vec![0_u8; 0x42 + wide.len() * 2];
        v[0..8].copy_from_slice(&(parent | (u64::from(parent_seq) << 48)).to_le_bytes());
        v[0x40] = wide.len() as u8;
        v[0x41] = namespace;
        for (i, c) in wide.iter().enumerate() {
            v[0x42 + i * 2..0x44 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        resident(ATTR_FILE_NAME, &v, &[])
    }

    fn standard(attributes: u32, modified: i64) -> Vec<u8> {
        let mut v = vec![0_u8; 0x48];
        v[0x08..0x10].copy_from_slice(&modified.to_le_bytes());
        v[0x20..0x24].copy_from_slice(&attributes.to_le_bytes());
        resident(ATTR_STANDARD_INFORMATION, &v, &[])
    }

    fn parse(r: &mut [u8], index: u64) -> (Slot, Chunk) {
        let mut slot = Slot::default();
        let mut chunk = Chunk::default();
        parse_record(r, index, &mut slot, &mut chunk, 0);
        (slot, chunk)
    }

    #[test]
    fn parses_names_sizes_and_links() {
        let mut r = record(
            1024,
            RECORD_IN_USE,
            7,
            0,
            &[
                standard(0x20, 133_000_000_000_000_000),
                file_name(40, 3, NAMESPACE_DOS, "LONGNA~1.TXT"),
                file_name(40, 3, 1, "long name.txt"),
                file_name(41, 9, 0, "مرحبا"),
                resident(ATTR_DATA, &[1, 2, 3], &[u16::from(b'x')]),
                nonresident(0, 0, 8192, 5000, 0, &[0x11, 0x02, 0x10, 0x00]),
            ],
        );
        let (slot, chunk) = parse(&mut r, 100);
        assert!(slot.in_use() && !slot.is_directory() && !slot.is_reparse());
        assert_eq!(
            (slot.sequence, slot.logical, slot.allocated),
            (7, 5000, 8192)
        );
        assert_eq!(slot.modified, 133_000_000_000_000_000);
        assert_eq!(chunk.names.len(), 2, "the DOS alias is not a link");
        assert_eq!(
            (chunk.names[0].parent, chunk.names[0].parent_sequence),
            (40, 3)
        );
        let text = |n: &Name| {
            std::str::from_utf8(&chunk.text[n.offset as usize..n.offset as usize + n.len as usize])
                .unwrap()
                .to_string()
        };
        assert_eq!(text(&chunk.names[0]), "long name.txt");
        assert_eq!(text(&chunk.names[1]), "مرحبا");
    }

    #[test]
    fn resident_sparse_and_reparse_records() {
        let mut r = record(
            1024,
            RECORD_IN_USE,
            1,
            0,
            &[
                standard(0, 0),
                file_name(5, 5, 1, "tiny"),
                resident(ATTR_DATA, &[9; 17], &[]),
            ],
        );
        let (slot, _) = parse(&mut r, 30);
        assert_eq!((slot.logical, slot.allocated), (17, 0));
        let mut r = record(
            1024,
            RECORD_IN_USE,
            1,
            0,
            &[
                standard(0, 0),
                file_name(5, 5, 1, "sparse"),
                nonresident(
                    ATTR_FLAG_SPARSE,
                    0,
                    1 << 30,
                    1 << 30,
                    65536,
                    &[0x01, 0x40, 0x00],
                ),
            ],
        );
        let (slot, _) = parse(&mut r, 31);
        assert_eq!((slot.logical, slot.allocated), (1 << 30, 65536));
        let mut r = record(
            4096,
            RECORD_IN_USE | RECORD_IS_DIRECTORY,
            1,
            0,
            &[
                standard(FILE_ATTRIBUTE_REPARSE_POINT | 0x10, 0),
                file_name(5, 5, 1, "junction"),
            ],
        );
        let (slot, _) = parse(&mut r, 32);
        assert!(slot.is_directory() && slot.is_reparse());
    }

    #[test]
    fn rejects_torn_free_and_malformed_records() {
        let attrs = [standard(0, 0), file_name(5, 5, 1, "a")];
        let mut r = record(1024, RECORD_IN_USE, 1, 0, &attrs);
        r[1022] ^= 0xFF; // the second sector was rewritten after the first
        let (slot, chunk) = parse(&mut r, 40);
        assert!(!slot.in_use());
        assert_eq!(chunk.torn, 1);
        let mut r = record(1024, 0, 1, 0, &attrs);
        let (slot, chunk) = parse(&mut r, 41);
        assert!(!slot.in_use());
        assert_eq!(chunk.torn, 0, "free records are not errors");
        let mut r = record(1024, RECORD_IN_USE, 1, 0, &attrs);
        let first = u16_at(&r, 0x14).unwrap() as usize;
        r[first + 4..first + 8].copy_from_slice(&5000_u32.to_le_bytes());
        let (slot, chunk) = parse(&mut r, 42);
        assert!(!slot.in_use());
        assert_eq!((chunk.torn, chunk.names.len()), (1, 0));
        let mut garbage = vec![0x5A_u8; 1024];
        garbage[0..4].copy_from_slice(FILE_MAGIC);
        let (slot, _) = parse(&mut garbage, 43);
        assert!(!slot.in_use());
    }

    #[test]
    fn rejects_truncated_attribute_lists_and_partial_fixups() {
        let attrs = [
            standard(0, 0),
            file_name(5, 5, 1, "a"),
            resident(ATTR_DATA, &[1; 40], &[]),
        ];
        // bytes_in_use ends right after $FILE_NAME: no $DATA and no end marker.
        let mut r = record(1024, RECORD_IN_USE, 1, 0, &attrs);
        let first = u16_at(&r, 0x14).unwrap() as usize;
        let cut = first + attrs[0].len() + attrs[1].len();
        r[0x18..0x1C].copy_from_slice(&(cut as u32).to_le_bytes());
        let (slot, chunk) = parse(&mut r, 50);
        assert!(!slot.in_use());
        assert_eq!((chunk.torn, chunk.names.len()), (1, 0));
        // bytes_in_use beyond the record.
        let mut r = record(1024, RECORD_IN_USE, 1, 0, &attrs);
        r[0x18..0x1C].copy_from_slice(&2048_u32.to_le_bytes());
        assert_eq!(parse(&mut r, 51).1.torn, 1);
        // A fixup array that only covers the first sector.
        let mut r = record(1024, RECORD_IN_USE, 1, 0, &attrs);
        r[6..8].copy_from_slice(&2_u16.to_le_bytes());
        assert_eq!(parse(&mut r, 52).1.torn, 1);
    }

    #[test]
    fn mapped_reads_must_cover_every_byte() {
        struct Zeros;
        impl ReadAt for Zeros {
            fn read_at(&self, _offset: u64, buffer: &mut [u8]) -> io::Result<()> {
                buffer.fill(1);
                Ok(())
            }
        }
        let gap = [
            Extent {
                vcn_byte: 0,
                bytes: 4096,
                disk: Some(0),
            },
            Extent {
                vcn_byte: 8192,
                bytes: 4096,
                disk: Some(65536),
            },
        ];
        let mut buffer = vec![0_u8; 12288];
        assert!(read_mapped(&Zeros, &gap, 0, &mut buffer).is_err());
        let whole = [Extent {
            vcn_byte: 0,
            bytes: 12288,
            disk: Some(0),
        }];
        assert!(read_mapped(&Zeros, &whole, 0, &mut buffer).is_ok());
    }

    #[test]
    fn disguised_reparse_points_are_files_and_wof_data_counts() {
        let reparse = |tag: u32| {
            let mut v = vec![0_u8; 8];
            v[0..4].copy_from_slice(&tag.to_le_bytes());
            resident(ATTR_REPARSE_POINT, &v, &[])
        };
        let wof_name: Vec<u16> = WOF_STREAM.encode_utf16().collect();
        let mut wof_stream = nonresident(0, 0, 61_440, 61_440, 0, &[0x11, 0x0F, 0x20, 0x00]);
        // Name the stream: grow the header and move the runs.
        let mut named = wof_stream[..0x48].to_vec();
        named[9] = wof_name.len() as u8;
        named[0x0A..0x0C].copy_from_slice(&0x48_u16.to_le_bytes());
        for c in &wof_name {
            named.extend_from_slice(&c.to_le_bytes());
        }
        while !named.len().is_multiple_of(8) {
            named.push(0);
        }
        let runs_at = named.len();
        named.extend_from_slice(&[0x11, 0x0F, 0x20, 0x00, 0, 0, 0, 0]);
        named[0x20..0x22].copy_from_slice(&(runs_at as u16).to_le_bytes());
        let len = named.len() as u32;
        named[4..8].copy_from_slice(&len.to_le_bytes());
        wof_stream = named;
        let mut r = record(
            4096,
            RECORD_IN_USE,
            1,
            0,
            &[
                standard(FILE_ATTRIBUTE_REPARSE_POINT, 0),
                file_name(5, 5, 1, "compact.dll"),
                nonresident(
                    ATTR_FLAG_SPARSE,
                    0,
                    900_000,
                    900_000,
                    0,
                    &[0x01, 0xDC, 0x00],
                ),
                wof_stream,
                reparse(TAG_WOF),
            ],
        );
        let (slot, _) = parse(&mut r, 60);
        assert!(slot.is_reparse() && !slot.is_link());
        assert_eq!((slot.logical, slot.allocated), (900_000, 61_440));
        for (tag, link) in [
            (0xA000_0003, true),  // junction / mount point
            (0xA000_000C, true),  // symbolic link
            (0x8000_001B, true),  // app execution alias
            (0x9000_101A, false), // OneDrive placeholder
            (TAG_DEDUP, false),
            (TAG_PROJFS, false),
        ] {
            let mut r = record(
                1024,
                RECORD_IN_USE,
                1,
                0,
                &[
                    standard(FILE_ATTRIBUTE_REPARSE_POINT, 0),
                    file_name(5, 5, 1, "x"),
                    reparse(tag),
                ],
            );
            assert_eq!(parse(&mut r, 61).0.is_link(), link, "{tag:#x}");
        }
    }

    #[test]
    fn extension_records_bring_the_reparse_tag_and_wof_allocation() {
        let mut v = vec![0_u8; 8];
        v[0..4].copy_from_slice(&TAG_WOF.to_le_bytes());
        let reparse = resident(ATTR_REPARSE_POINT, &v, &[]);
        let base = record(
            1024,
            RECORD_IN_USE,
            3,
            0,
            &[
                standard(FILE_ATTRIBUTE_REPARSE_POINT, 0),
                file_name(5, 5, 1, "hardlinked.dll"),
                nonresident(
                    ATTR_FLAG_SPARSE,
                    0,
                    900_000,
                    900_000,
                    0,
                    &[0x01, 0xDC, 0x00],
                ),
            ],
        );
        // The extension record (base reference: record 70, sequence 3) holds the
        // reparse point, as for files whose names fill the base record.
        let extension = record(1024, RECORD_IN_USE, 1, (3_u64 << 48) | 70, &[reparse]);
        let (base_slot, _) = parse(&mut base.clone(), 70);
        assert_eq!(base_slot.reparse_tag, 0);
        assert!(base_slot.is_link(), "unknown tag on its own: a link");
        let (extension_slot, _) = parse(&mut extension.clone(), 71);
        assert_eq!(extension_slot.reparse_tag, TAG_WOF);
        // Fold as Mft::read does.
        let mut slots = vec![Slot::default(); 72];
        slots[70] = base_slot;
        slots[71] = extension_slot;
        let slot = slots[71];
        let target = &mut slots[slot.base as usize];
        target.allocated += slot.allocated;
        if target.reparse_tag == 0 {
            target.reparse_tag = slot.reparse_tag;
        }
        assert!(
            !slots[70].is_link(),
            "a WOF file once its extension is folded in"
        );
    }

    #[test]
    fn decodes_mapping_pairs() {
        // 0x21: 1-byte length, 2-byte offset. Second run moves backwards; third is sparse.
        let attr = nonresident(
            0,
            0,
            0,
            0,
            0,
            &[0x21, 0x10, 0x00, 0x01, 0x21, 0x08, 0x00, 0xFF, 0x01, 0x04],
        );
        let runs = runs(&attr).unwrap();
        assert_eq!(
            runs,
            vec![(0, Some(256), 16), (16, Some(0), 8), (24, None, 4)]
        );
        assert!(super::runs(&nonresident(0, 0, 0, 0, 0, &[0x91, 0x01])).is_none());
    }

    #[test]
    fn invalid_names_round_trip_exactly() {
        let mut text = Vec::new();
        let wide = [98_u16, 0xD800, 99];
        let bytes: Vec<u8> = wide.iter().flat_map(|u| u.to_le_bytes()).collect();
        encode_name(&bytes, &mut text);
        // SAFETY: produced by encode_name for exactly this name.
        let name = unsafe { OsStr::from_encoded_bytes_unchecked(&text) };
        assert_eq!(name.encode_wide().collect::<Vec<_>>(), wide);
    }

    #[test]
    fn reads_boot_sector_geometry() {
        let mut boot = vec![0_u8; 512];
        boot[3..11].copy_from_slice(b"NTFS    ");
        boot[0x0B..0x0D].copy_from_slice(&512_u16.to_le_bytes());
        boot[0x0D] = 8;
        boot[0x30..0x38].copy_from_slice(&786_432_u64.to_le_bytes());
        boot[0x40] = 0xF6; // -10: 1024-byte records
        let g = geometry(&boot).unwrap();
        assert_eq!(
            (g.cluster, g.record, g.mft_offset),
            (4096, 1024, 786_432 * 4096)
        );
        boot[3] = b'X';
        assert!(geometry(&boot).is_err());
    }
}
