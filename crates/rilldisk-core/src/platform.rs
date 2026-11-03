// SPDX-License-Identifier: AGPL-3.0-only
//! Directory-metadata backends. Each OS module exports the same items: `VolumeInfo`
//! (per-scan facts, including which walk runs), `volume_info`, `Target` (how a
//! worker opens one folder), `root_target`, `Listing` and `read_directory` for the
//! parallel walk in `lib.rs`; `mft_source`, `scan_mft` and `BACKEND_MFT` for the
//! NTFS Master File Table backend (unavailable outside Windows); and the public
//! `volumes` listing.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::volumes;
#[cfg(windows)]
pub(crate) use windows::{
    BACKEND_MFT, Listing, Target, VolumeInfo, elevation, enable_backup_privilege,
    executable_on_path, is_elevated, mft_source, physical_memory, read_directory, root_target,
    scan_mft, scan_ntfs_image, volume_info,
};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::volumes;
#[cfg(unix)]
pub(crate) use unix::{
    BACKEND_MFT, Listing, Target, VolumeInfo, elevation, enable_backup_privilege,
    executable_on_path, is_elevated, mft_source, physical_memory, read_directory, root_target,
    scan_mft, volume_info,
};

#[cfg(not(any(windows, unix)))]
compile_error!("RillDisk supports Windows and Unix (Linux, macOS) targets");

/// A mounted volume that can be offered as a scan root.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Volume {
    #[serde(serialize_with = "crate::serialize_path")]
    pub root: std::path::PathBuf,
    pub label: String,
    pub filesystem: String,
    pub total_bytes: u64,
    /// Free bytes on the filesystem, including any space reserved for the
    /// administrator (Windows: total free bytes; Unix: `f_bfree`).
    pub free_bytes: u64,
    pub removable: bool,
}
