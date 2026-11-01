// SPDX-License-Identifier: AGPL-3.0-only
//! Pull named executables out of a verified release archive, in memory.
//!
//! Archive paths are never written to disk: entries are matched by their file
//! name only, so path traversal entries cannot escape anywhere.

use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
};

/// Largest single executable accepted from an archive.
const MAX_ENTRY: u64 = 512 << 20;

/// Extract every entry whose file name is in `wanted` from a `.zip` or
/// `.tar.gz` archive. Missing names are simply absent from the result.
pub fn extract_binaries(
    archive_name: &str,
    bytes: &[u8],
    wanted: &[String],
) -> Result<HashMap<String, Vec<u8>>> {
    let mut found = HashMap::new();
    let mut take = |path: &str, size: u64, reader: &mut dyn Read| -> Result<()> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if !wanted.iter().any(|w| w == name) {
            return Ok(());
        }
        if size > MAX_ENTRY {
            bail!("{name} in the archive is implausibly large ({size} bytes)");
        }
        if found.contains_key(name) {
            bail!("{name} appears more than once in the archive");
        }
        let mut data = Vec::with_capacity(size as usize);
        reader.take(MAX_ENTRY + 1).read_to_end(&mut data)?;
        if data.len() as u64 > MAX_ENTRY {
            bail!("{name} in the archive is implausibly large");
        }
        found.insert(name.to_owned(), data);
        Ok(())
    };
    if archive_name.ends_with(".zip") {
        let mut zip =
            zip::ZipArchive::new(Cursor::new(bytes)).context("not a valid zip archive")?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_file() {
                let path = entry.name()?.into_owned();
                take(&path, entry.size(), &mut entry)?;
            }
        }
    } else if archive_name.ends_with(".tar.gz") {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in tar.entries().context("not a valid tar.gz archive")? {
            let mut entry = entry?;
            if entry.header().entry_type().is_file() {
                let path = entry.path()?.to_string_lossy().into_owned();
                let size = entry.size();
                take(&path, size, &mut entry)?;
            }
        }
    } else {
        bail!("unsupported archive format: {archive_name}");
    }
    Ok(found)
}
