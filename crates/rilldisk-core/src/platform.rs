// SPDX-License-Identifier: AGPL-3.0-only
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{BACKEND, read_directory};

#[cfg(not(windows))]
pub(crate) const BACKEND: &str = "portable-walk";

#[cfg(not(windows))]
pub(crate) fn read_directory(
    path: &std::path::Path,
    control: &crate::ScanControl,
    limit: usize,
) -> crate::Batch {
    use crate::{Batch, EntryKind, RawEntry, ScanIssue};
    let mut batch = Batch {
        entries: Vec::new(),
        issues: Vec::new(),
    };
    let result = (|| -> std::io::Result<()> {
        for item in std::fs::read_dir(path)? {
            if control.is_cancelled() {
                break;
            }
            let item = item?;
            if batch.entries.len() >= limit {
                batch.issues.push(ScanIssue {
                    path: path.into(),
                    message: "directory entry limit reached".into(),
                });
                break;
            }
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
            batch.entries.push(RawEntry {
                path: item.path(),
                kind,
                logical: if kind == EntryKind::File {
                    metadata.len()
                } else {
                    0
                },
                allocated: if kind == EntryKind::File {
                    allocated
                } else {
                    Some(0)
                },
                identity,
            });
        }
        Ok(())
    })();
    if let Err(error) = result {
        batch.issues.push(ScanIssue {
            path: path.into(),
            message: error.to_string(),
        });
    }
    batch
}
