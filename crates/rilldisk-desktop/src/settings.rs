// SPDX-License-Identifier: AGPL-3.0-only
//! Remembered preferences: palette, theme and the size metric. A small
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
}

/// Everything the Settings page can change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prefs {
    pub identity: Identity,
    /// None follows the operating system.
    pub theme: Option<bool>,
    pub allocated: bool,
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
            "palette={}\ntheme={theme}\nsizes={}\n",
            prefs.identity.name().to_lowercase(),
            if prefs.allocated { "on-disk" } else { "size" }
        ),
    );
}
