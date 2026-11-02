// SPDX-License-Identifier: AGPL-3.0-only
//! Directory-metadata backends. Each one exports the same four items: `BACKEND`,
//! `VolumeInfo` (per-scan facts), `volume_info` and `read_directory`, plus the public
//! `volumes` listing. The parallel walk in `lib.rs` is shared by all of them.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::volumes;
#[cfg(windows)]
pub(crate) use windows::{
    BACKEND, VolumeInfo, elevation, enable_backup_privilege, read_directory, volume_info,
};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::volumes;
#[cfg(unix)]
pub(crate) use unix::{
    BACKEND, VolumeInfo, elevation, enable_backup_privilege, read_directory, volume_info,
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
