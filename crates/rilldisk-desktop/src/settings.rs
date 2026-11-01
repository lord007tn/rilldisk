// SPDX-License-Identifier: AGPL-3.0-only
//! Remembered appearance: palette and light/dark. A two-line text file next to
//! the update preferences; anything unreadable falls back to defaults.
use crate::style::Identity;
use std::path::PathBuf;

#[derive(Default)]
pub struct Saved {
    pub identity: Option<Identity>,
    pub dark: Option<bool>,
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
                saved.dark = match value.trim() {
                    "dark" => Some(true),
                    "light" => Some(false),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    saved
}

/// Best effort: a read-only profile simply keeps the defaults next time.
pub fn save(identity: Identity, dark: bool) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        path,
        format!(
            "palette={}\ntheme={}\n",
            identity.name().to_lowercase(),
            if dark { "dark" } else { "light" }
        ),
    );
}
