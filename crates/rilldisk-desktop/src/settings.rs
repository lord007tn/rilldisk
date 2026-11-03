// SPDX-License-Identifier: AGPL-3.0-only
//! Remembered preferences: palette, theme, the size metric and worktree checks. A small
//! `key=value` text file next to the update preferences; anything unreadable
//! falls back to defaults.
use crate::style::Identity;
use std::path::PathBuf;

/// What was saved; `None` means "never chosen", so the caller's default applies.
#[derive(Default)]
pub struct Saved {
    pub identity: Option<Identity>,
    /// `Some(None)`: follow the system theme. `Some(Some(dark))`: a fixed choice.
    pub theme: Option<Option<bool>>,
    /// Show sizes as allocated "On disk" instead of logical "Size".
    pub allocated: Option<bool>,
    /// Ask Git about worktree candidates.
    pub git_checks: Option<bool>,
    /// Days without work after which a clean worktree counts as idle.
    pub stale_days: Option<u64>,
    /// Entry limit of desktop scans (one of [`SCAN_LIMITS`]).
    pub max_entries: Option<usize>,
}

/// Entry limits the desktop offers: the default and two larger memory bounds.
pub const SCAN_LIMITS: [usize; 3] = [30_000_000, 60_000_000, 120_000_000];

/// Everything the Settings page can change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prefs {
    pub identity: Identity,
    /// None follows the operating system.
    pub theme: Option<bool>,
    pub allocated: bool,
    pub git_checks: bool,
    pub stale_days: u64,
    pub max_entries: usize,
}

fn path() -> Option<PathBuf> {
    rilldisk_update::state_path().map(|p| p.with_file_name("desktop.txt"))
}

pub fn load() -> Saved {
    let mut saved = Saved::default();
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return saved;
    };
    for line in text.lines() {
        match line.split_once('=') {
            Some(("palette", value)) => saved.identity = Identity::parse(value.trim()),
            Some(("theme", value)) => {
                saved.theme = match value.trim() {
                    "dark" => Some(Some(true)),
                    "light" => Some(Some(false)),
                    "system" => Some(None),
                    _ => None,
                }
            }
            Some(("sizes", value)) => {
                saved.allocated = match value.trim() {
                    "on-disk" => Some(true),
                    "size" => Some(false),
                    _ => None,
                }
            }
            Some(("git-checks", value)) => {
                saved.git_checks = match value.trim() {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                }
            }
            Some(("scan-limit", value)) => {
                saved.max_entries = value
                    .trim()
                    .parse()
                    .ok()
                    .filter(|n| SCAN_LIMITS.contains(n))
            }
            Some(("stale-days", value)) => {
                saved.stale_days = value.trim().parse().ok().filter(|d| (1..=3650).contains(d))
            }
            _ => {}
        }
    }
    saved
}

/// Best effort: a read-only profile simply keeps the defaults next time.
pub fn save(prefs: &Prefs) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let theme = match prefs.theme {
        None => "system",
        Some(true) => "dark",
        Some(false) => "light",
    };
    let _ = std::fs::write(
        path,
        format!(
            "palette={}\ntheme={theme}\nsizes={}\ngit-checks={}\nstale-days={}\nscan-limit={}\n",
            prefs.identity.name().to_lowercase(),
            if prefs.allocated { "on-disk" } else { "size" },
            if prefs.git_checks { "on" } else { "off" },
            prefs.stale_days,
            prefs.max_entries
        ),
    );
}
