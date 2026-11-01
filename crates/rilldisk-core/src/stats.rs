// SPDX-License-Identifier: AGPL-3.0-only
//! Snapshot summaries shared by the CLI, MCP server and desktop app.
use crate::{EntryKind, Snapshot};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct TypeStat {
    /// Lowercase extension without the dot; empty for files without one.
    pub extension: String,
    pub files: u64,
    pub logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
}

/// Size per file extension, largest logical total first.
pub fn file_types(snapshot: &Snapshot) -> Vec<TypeStat> {
    let mut map: HashMap<String, TypeStat> = HashMap::new();
    for entry in &snapshot.entries {
        if entry.kind != EntryKind::File {
            continue;
        }
        let extension = std::path::Path::new(&entry.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .filter(|e| e.len() <= 16)
            .unwrap_or_default();
        let stat = map.entry(extension).or_insert_with_key(|key| TypeStat {
            extension: key.clone(),
            files: 0,
            logical_bytes: 0,
            allocated_bytes: Some(0),
        });
        stat.files += 1;
        stat.logical_bytes = stat.logical_bytes.saturating_add(entry.logical_bytes);
        stat.allocated_bytes = stat
            .allocated_bytes
            .zip(entry.allocated_bytes)
            .map(|(a, b)| a.saturating_add(b));
    }
    let mut stats: Vec<_> = map.into_values().collect();
    stats.sort_by(|a, b| {
        b.logical_bytes
            .cmp(&a.logical_bytes)
            .then_with(|| a.extension.cmp(&b.extension))
    });
    stats
}

/// IDs of the largest files by logical or allocated size, largest first.
pub fn largest_files(snapshot: &Snapshot, limit: usize, by_allocated: bool) -> Vec<usize> {
    let key = |id: usize| {
        let entry = &snapshot.entries[id];
        if by_allocated {
            entry.allocated_bytes.unwrap_or(0)
        } else {
            entry.logical_bytes
        }
    };
    let mut ids: Vec<usize> = snapshot
        .entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| e.id)
        .collect();
    let compare = |a: &usize, b: &usize| key(*b).cmp(&key(*a)).then(a.cmp(b));
    if ids.len() > limit && limit > 0 {
        ids.select_nth_unstable_by(limit, compare);
        ids.truncate(limit);
    }
    ids.sort_unstable_by(compare);
    ids.truncate(limit);
    ids
}
