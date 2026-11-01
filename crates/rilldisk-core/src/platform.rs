// SPDX-License-Identifier: AGPL-3.0-only
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::volumes;
#[cfg(windows)]
pub(crate) use windows::{BACKEND, read_directory, volume_info};

/// A mounted volume that can be offered as a scan root.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Volume {
    #[serde(serialize_with = "crate::serialize_path")]
    pub root: std::path::PathBuf,
    pub label: String,
    pub filesystem: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub removable: bool,
}

/// Portable builds offer the filesystem root; capacity is not yet qualified there.
#[cfg(not(windows))]
pub fn volumes() -> Vec<Volume> {
    Vec::new()
}

#[cfg(not(windows))]
pub(crate) const BACKEND: &str = "portable-walk";

/// Per-scan volume facts shared by every directory read.
#[derive(Debug, Clone, Copy, Default)]
// The portable backend reads neither field yet; both are Windows facts today.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct VolumeInfo {
    pub allocation_unit: Option<u64>,
    pub serial: u64,
}

#[cfg(not(windows))]
pub(crate) fn volume_info(_root: &std::path::Path) -> std::io::Result<VolumeInfo> {
    Ok(VolumeInfo::default())
}

#[cfg(not(windows))]
pub(crate) fn read_directory(
    path: &std::path::Path,
    control: &crate::ScanControl,
    _volume: &VolumeInfo,
) -> crate::Batch {
    use crate::{Batch, EntryKind, RawEntry, ScanIssue};
    let mut batch = Batch::default();
    let result = (|| -> std::io::Result<bool> {
        for item in std::fs::read_dir(path)? {
            if control.should_stop() {
                return Ok(false);
            }
            let item = item?;
            let metadata = match item.path().symlink_metadata() {
                Ok(value) => value,
                Err(error) => {
                    batch.issues.push(ScanIssue {
                        path: item.path(),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let kind = if metadata.is_symlink() {
                EntryKind::Link
            } else if metadata.is_dir() {
                EntryKind::Directory
            } else if metadata.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            #[cfg(unix)]
            let (allocated, identity) = {
                use std::os::unix::fs::MetadataExt;
                (
                    Some(metadata.blocks().saturating_mul(512)),
                    Some((metadata.dev(), metadata.ino())),
                )
            };
            #[cfg(not(unix))]
            let (allocated, identity) = (None, None);
            let logical = if kind == EntryKind::File {
                metadata.len()
            } else {
                0
            };
            if !control.record(kind, logical) {
                return Ok(false);
            }
            batch.entries.push(RawEntry {
                name: item.file_name().into_boxed_os_str(),
                kind,
                logical,
                allocated: if kind == EntryKind::File {
                    allocated
                } else {
                    Some(0)
                },
                modified: metadata.modified().ok().and_then(crate::unix_ms),
                identity,
                key: None,
            });
        }
        Ok(true)
    })();
    match result {
        Ok(complete) => batch.complete = complete,
        Err(error) => batch.issues.push(ScanIssue {
            path: path.into(),
            message: error.to_string(),
        }),
    }
    batch
}
