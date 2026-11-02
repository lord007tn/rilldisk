// SPDX-License-Identifier: AGPL-3.0-only
//! A cheap, growing picture of a scan in progress, so a UI can draw the largest
//! folders while the scanner is still walking them.
//!
//! The collector feeds every retained entry to [`LiveTracker`]; it keeps running
//! totals for the folders one and two levels below the scan root and publishes
//! a [`LiveView`] a few times per second through [`crate::ScanControl::live`].
//! Totals are what has been found so far: they only ever grow, and they become
//! the final numbers once the scan finishes.

use crate::EntryKind;
use std::{collections::HashMap, ffi::OsStr, sync::Arc, time::Instant};

/// Running totals of one folder below the scan root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveFolder {
    pub name: String,
    pub logical_bytes: u64,
    /// Allocated bytes found so far, hard links counted once.
    pub allocated_bytes: u64,
    pub files: u64,
    /// The largest subfolders (second level only), largest first.
    pub children: Vec<LiveFolder>,
}

/// A snapshot of a scan in progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveView {
    /// Folders directly below the scan root, largest (logical) first.
    pub folders: Vec<LiveFolder>,
    /// Files directly in the scan root.
    pub loose_logical_bytes: u64,
    pub loose_allocated_bytes: u64,
    pub loose_files: u64,
    /// Monotonic counter, bumped on every publish.
    pub generation: u64,
}

impl LiveView {
    pub fn total_logical(&self) -> u64 {
        self.loose_logical_bytes + self.folders.iter().map(|f| f.logical_bytes).sum::<u64>()
    }
}

/// Folders kept per level in a published view; the rest are summarized by
/// their parent's total.
const MAX_TOP: usize = 48;
const MAX_CHILDREN: usize = 24;
/// Publishing builds a small view; a few times per second is plenty for a UI.
const PUBLISH_EVERY_MS: u128 = 250;
const TOP: u32 = u32::MAX;

struct Group {
    name: Box<str>,
    /// `TOP` for a first-level folder, else the index of its first-level group.
    parent: u32,
    logical: u64,
    allocated: u64,
    files: u64,
}

pub(crate) struct LiveTracker {
    groups: Vec<Group>,
    /// Directory entry id -> the group its contents count toward.
    group_of_dir: HashMap<usize, u32, crate::IdentityHasher>,
    loose: (u64, u64, u64),
    generation: u64,
    last_publish: Instant,
}

impl LiveTracker {
    pub(crate) fn new() -> Self {
        Self {
            groups: Vec::new(),
            group_of_dir: HashMap::default(),
            loose: (0, 0, 0),
            generation: 0,
            last_publish: Instant::now(),
        }
    }

    /// Account for one retained entry. `parent` is its parent's entry id (0 is
    /// the scan root); `allocated` is already zero for hard-link duplicates.
    pub(crate) fn add(
        &mut self,
        id: usize,
        parent: usize,
        kind: EntryKind,
        name: &OsStr,
        logical: u64,
        allocated: Option<u64>,
    ) {
        let group = if parent == 0 {
            None
        } else {
            match self.group_of_dir.get(&parent) {
                Some(&g) => Some(g),
                None => return,
            }
        };
        match kind {
            EntryKind::Directory => {
                let own = match group {
                    None => self.push(name, TOP),
                    Some(g) if self.groups[g as usize].parent == TOP => self.push(name, g),
                    Some(g) => g,
                };
                self.group_of_dir.insert(id, own);
            }
            EntryKind::File => {
                let allocated = allocated.unwrap_or(0);
                match group {
                    None => {
                        self.loose.0 += logical;
                        self.loose.1 += allocated;
                        self.loose.2 += 1;
                    }
                    Some(g) => {
                        let mut at = g;
                        loop {
                            let entry = &mut self.groups[at as usize];
                            entry.logical += logical;
                            entry.allocated += allocated;
                            entry.files += 1;
                            if entry.parent == TOP {
                                break;
                            }
                            at = entry.parent;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn push(&mut self, name: &OsStr, parent: u32) -> u32 {
        self.groups.push(Group {
            name: name.to_string_lossy().into(),
            parent,
            logical: 0,
            allocated: 0,
            files: 0,
        });
        (self.groups.len() - 1) as u32
    }

    /// Publish a fresh view if enough time has passed (or `force`).
    pub(crate) fn publish(&mut self, slot: &std::sync::Mutex<Option<Arc<LiveView>>>, force: bool) {
        if !force && self.last_publish.elapsed().as_millis() < PUBLISH_EVERY_MS {
            return;
        }
        self.last_publish = Instant::now();
        self.generation += 1;
        let view = Arc::new(self.view());
        if let Ok(mut slot) = slot.lock() {
            *slot = Some(view);
        }
    }

    fn view(&self) -> LiveView {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut tops = Vec::new();
        for (i, g) in self.groups.iter().enumerate() {
            if g.parent == TOP {
                tops.push(i as u32);
            } else {
                children.entry(g.parent).or_default().push(i as u32);
            }
        }
        let folder = |i: u32| {
            let g = &self.groups[i as usize];
            LiveFolder {
                name: g.name.to_string(),
                logical_bytes: g.logical,
                allocated_bytes: g.allocated,
                files: g.files,
                children: Vec::new(),
            }
        };
        let by_size = |a: &u32, b: &u32| {
            self.groups[*b as usize]
                .logical
                .cmp(&self.groups[*a as usize].logical)
        };
        tops.sort_unstable_by(by_size);
        tops.truncate(MAX_TOP);
        let folders = tops
            .into_iter()
            .map(|top| {
                let mut f = folder(top);
                if let Some(kids) = children.get_mut(&top) {
                    kids.sort_unstable_by(by_size);
                    f.children = kids.iter().take(MAX_CHILDREN).map(|&k| folder(k)).collect();
                }
                f
            })
            .collect();
        LiveView {
            folders,
            loose_logical_bytes: self.loose.0,
            loose_allocated_bytes: self.loose.1,
            loose_files: self.loose.2,
            generation: self.generation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_roll_up_two_levels() {
        let mut t = LiveTracker::new();
        let n = |s: &str| OsStr::new(s).to_owned();
        // root(0) / a(1) / b(2) / c(3) / f(4); root / g(5); a / h(6)
        t.add(1, 0, EntryKind::Directory, &n("a"), 0, Some(0));
        t.add(2, 1, EntryKind::Directory, &n("b"), 0, Some(0));
        t.add(3, 2, EntryKind::Directory, &n("c"), 0, Some(0));
        t.add(4, 3, EntryKind::File, &n("f"), 100, Some(4096));
        t.add(5, 0, EntryKind::File, &n("g"), 7, Some(0));
        t.add(6, 1, EntryKind::File, &n("h"), 10, Some(4096));
        let slot = std::sync::Mutex::new(None);
        t.publish(&slot, true);
        let view = slot.lock().unwrap().clone().unwrap();
        assert_eq!(view.folders.len(), 1);
        let a = &view.folders[0];
        assert_eq!((a.name.as_str(), a.logical_bytes, a.files), ("a", 110, 2));
        assert_eq!(a.allocated_bytes, 8192);
        assert_eq!(a.children.len(), 1);
        // c's file counts toward b, the second-level folder.
        assert_eq!(
            (a.children[0].name.as_str(), a.children[0].logical_bytes),
            ("b", 100)
        );
        assert_eq!((view.loose_logical_bytes, view.loose_files), (7, 1));
        assert_eq!(view.total_logical(), 117);
    }
}
