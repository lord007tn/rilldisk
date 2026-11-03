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
    /// Entry limit of desktop scans: `Some(None)` is Automatic, `Some(Some(n))`
    /// one of [`SCAN_LIMITS`].
    pub max_entries: Option<Option<usize>>,
}

/// Fixed entry limits the desktop offers besides Automatic
/// ([`rilldisk_core::auto_max_entries`]), for folders larger than that.
pub const SCAN_LIMITS: [usize; 4] = [60_000_000, 120_000_000, 240_000_000, 480_000_000];

/// Everything the Settings page can change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prefs {
    pub identity: Identity,
    /// None follows the operating system.
    pub theme: Option<bool>,
    pub allocated: bool,
    pub git_checks: bool,
    pub stale_days: u64,
    /// None: Automatic, sized from installed memory.
    pub max_entries: Option<usize>,
}

impl Prefs {
    /// The entry limit desktop scans use.
    pub fn scan_limit(&self) -> usize {
        self.max_entries
            .unwrap_or_else(rilldisk_core::auto_max_entries)
    }
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
            Some(("scan-limit", value)) => saved.max_entries = parse_limit(value.trim()),
            Some(("stale-days", value)) => {
                saved.stale_days = value.trim().parse().ok().filter(|d| (1..=3650).contains(d))
            }
            _ => {}
        }
    }
    saved
}

/// A saved `scan-limit`. Versions before 0.1.4 always wrote their fixed
/// default, 30,000,000, so it reads as Automatic, which is never smaller.
fn parse_limit(value: &str) -> Option<Option<usize>> {
    match value {
        "auto" => Some(None),
        _ => match value.parse().ok()? {
            rilldisk_core::DEFAULT_MAX_ENTRIES => Some(None),
            n if SCAN_LIMITS.contains(&n) => Some(Some(n)),
            _ => None,
        },
    }
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
            prefs
                .max_entries
                .map_or_else(|| "auto".to_string(), |n| n.to_string())
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_default_limit_reads_as_automatic() {
        assert_eq!(parse_limit("auto"), Some(None));
        assert_eq!(parse_limit("30000000"), Some(None));
        assert_eq!(parse_limit("120000000"), Some(Some(120_000_000)));
        assert_eq!(parse_limit("12345"), None);
        assert_eq!(parse_limit("junk"), None);
    }
}
