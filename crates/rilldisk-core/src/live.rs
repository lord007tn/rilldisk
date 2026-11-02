// SPDX-License-Identifier: AGPL-3.0-only
//! A cheap, growing picture of a scan in progress, so a UI can draw the largest
//! folders while the scanner is still walking them.
//!
//! The collector feeds every retained entry to [`LiveTracker`]; it keeps running
//! own totals for every directory and publishes a focused two-level
//! [`LiveView`] a few times per second through [`crate::ScanControl::live`].
//! Totals are what has been found so far: they only ever grow, and they become
//! the final numbers once the scan finishes.

use crate::EntryKind;
use std::{collections::HashMap, ffi::OsStr, sync::Arc, time::Instant};

/// Running totals of one folder below the focused directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveFolder {
    /// Stable directory entry id within this scan.
    pub id: usize,
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
    /// Focused folder and its ancestors, excluding the scan root.
    pub ancestors: Vec<(usize, String)>,
    /// Folders directly below the focused directory, largest (logical) first.
    pub folders: Vec<LiveFolder>,
    /// Files directly in the focused directory.
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
const MAX_CHILDREN: usize = 24;
/// Publishing builds a small view; a few times per second is plenty for a UI.
const PUBLISH_EVERY_MS: u128 = 250;
const TOP: u32 = u32::MAX;

struct Group {
    id: usize,
    name: Box<str>,
    /// `TOP` for a first-level folder, else the index of its parent group.
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
    publish_cost: std::time::Duration,
    published_focus: usize,
}

impl LiveTracker {
    pub(crate) fn new() -> Self {
        Self {
            groups: Vec::new(),
            group_of_dir: HashMap::default(),
            loose: (0, 0, 0),
            generation: 0,
            last_publish: Instant::now(),
            publish_cost: std::time::Duration::ZERO,
            published_focus: 0,
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
                    None => self.push(id, name, TOP),
                    Some(g) => self.push(id, name, g),
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
                        let entry = &mut self.groups[g as usize];
                        entry.logical += logical;
                        entry.allocated += allocated;
                        entry.files += 1;
                    }
                }
            }
            _ => {}
        }
    }

    fn push(&mut self, id: usize, name: &OsStr, parent: u32) -> u32 {
        self.groups.push(Group {
            id,
            name: name.to_string_lossy().into(),
            parent,
            logical: 0,
            allocated: 0,
            files: 0,
        });
        (self.groups.len() - 1) as u32
    }

    /// Publish a fresh view if enough time has passed (or `force`).
    pub(crate) fn publish(
        &mut self,
        slot: &std::sync::Mutex<Option<Arc<LiveView>>>,
        force: bool,
        focus: usize,
    ) {
        // Bound publication to roughly 5% of scan time on a large tree.
        let interval = std::time::Duration::from_millis(PUBLISH_EVERY_MS as u64)
            .max(self.publish_cost.saturating_mul(20));
        if !force && focus == self.published_focus && self.last_publish.elapsed() < interval {
            return;
        }
        let started = Instant::now();
        self.generation += 1;
        let view = Arc::new(self.view(focus));
        if let Ok(mut slot) = slot.lock() {
            *slot = Some(view);
        }
        self.publish_cost = started.elapsed();
        self.published_focus = focus;
        self.last_publish = Instant::now();
    }

    fn view(&self, focus: usize) -> LiveView {
        // Own counters keep ingestion O(1) per file at any depth. Roll up once
        // per publication, in parent-first insertion order reversed: O(D).
        let mut totals: Vec<_> = self
            .groups
            .iter()
            .map(|g| (g.logical, g.allocated, g.files))
            .collect();
        for i in (0..self.groups.len()).rev() {
            let parent = self.groups[i].parent;
            if parent != TOP {
                let own = totals[i];
                let to = &mut totals[parent as usize];
                to.0 += own.0;
                to.1 += own.1;
                to.2 += own.2;
            }
        }
        let focused = self.group_of_dir.get(&focus).copied().unwrap_or(TOP);
        let mut ancestors = Vec::new();
        let mut at = focused;
        while at != TOP {
            let g = &self.groups[at as usize];
            ancestors.push((g.id, g.name.to_string()));
            at = g.parent;
        }
        ancestors.reverse();
        let mut tops = Vec::new();
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (i, g) in self.groups.iter().enumerate() {
            if g.parent == focused {
                tops.push(i as u32);
            } else if g.parent != TOP && self.groups[g.parent as usize].parent == focused {
                children.entry(g.parent).or_default().push(i as u32);
            }
        }
        let folder = |i: u32| {
            let g = &self.groups[i as usize];
            let total = totals[i as usize];
            LiveFolder {
                id: g.id,
                name: g.name.to_string(),
                logical_bytes: total.0,
                allocated_bytes: total.1,
                files: total.2,
                children: Vec::new(),
            }
        };
        let by_size = |a: &u32, b: &u32| {
            totals[*b as usize]
                .0
                .cmp(&totals[*a as usize].0)
                .then(a.cmp(b))
        };
        tops.sort_unstable_by(by_size);
        // Keep all immediate folders navigable, including empty ones. Only the
        // decorative nested preview is bounded.
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
        let loose = if focused == TOP {
            self.loose
        } else {
            let g = &self.groups[focused as usize];
            (g.logical, g.allocated, g.files)
        };
        LiveView {
            ancestors,
            folders,
            loose_logical_bytes: loose.0,
            loose_allocated_bytes: loose.1,
            loose_files: loose.2,
            generation: self.generation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_changes_publish_without_new_entries_or_waiting_for_the_interval() {
        let mut tracker = LiveTracker::new();
        tracker.add(1, 0, EntryKind::Directory, OsStr::new("folder"), 0, Some(0));
        let slot = std::sync::Mutex::new(None);
        tracker.publish(&slot, true, 0);
        tracker.publish(&slot, false, 1);
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().ancestors[0].0, 1);
        tracker.publish(&slot, false, 0);
        assert!(slot.lock().unwrap().as_ref().unwrap().ancestors.is_empty());
    }

    #[test]
    fn browse_deep_folders_and_return_without_losing_totals() {
        let mut t = LiveTracker::new();
        for id in 1..=8 {
            t.add(
                id,
                id - 1,
                EntryKind::Directory,
                OsStr::new("nested"),
                0,
                Some(0),
            );
        }
        t.add(9, 8, EntryKind::File, OsStr::new("deep"), 100, Some(4096));
        t.add(
            10,
            5,
            EntryKind::File,
            OsStr::new("hard-link"),
            100,
            Some(0),
        );
        let root = t.view(0);
        let deep = t.view(5);
        assert_eq!(
            deep.ancestors.iter().map(|a| a.0).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(deep.folders[0].id, 6);
        assert_eq!(deep.folders[0].allocated_bytes, 4096);
        assert_eq!(deep.loose_logical_bytes, 100);
        assert_eq!(deep.total_logical(), 200);
        assert_eq!(t.view(8).loose_allocated_bytes, 4096);
        assert_eq!(t.view(0), root);
        assert_eq!(t.view(usize::MAX), root);
    }

    #[test]
    fn empty_and_small_folders_remain_navigable_and_totals_are_not_truncated() {
        let mut t = LiveTracker::new();
        for i in 1..=60 {
            t.add(i, 0, EntryKind::Directory, OsStr::new("folder"), 0, Some(0));
            t.add(60 + i, i, EntryKind::File, OsStr::new("file"), 1, Some(0));
        }
        t.add(
            121,
            0,
            EntryKind::Directory,
            OsStr::new("empty"),
            0,
            Some(0),
        );
        let view = t.view(0);
        assert_eq!(view.folders.len(), 61);
        assert_eq!(view.total_logical(), 60);
        assert_eq!(view.folders.last().unwrap().id, 121);
    }

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
        t.publish(&slot, true, 0);
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
