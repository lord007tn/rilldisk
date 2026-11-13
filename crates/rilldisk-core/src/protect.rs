// SPDX-License-Identifier: AGPL-3.0-only
//! Paths the user never wants suggested or removed, whatever a rule says.
//!
//! One absolute path per line in [`config_path`] (`#` starts a comment, `~` at the
//! start is the home folder), plus any the caller adds (`rilldisk reclaim
//! --protect`). A protected path hides every reclaim candidate that is the path,
//! is inside it or contains it ([`crate::ReclaimReport::exclude_protected`]), and
//! cleanup refuses the same candidates ([`crate::cleanup::Guard::system`] loads
//! the file). Reading the file is the only thing this module does; it never
//! writes it.
use std::path::{Path, PathBuf};

/// File name of the list, in the RillDisk state folder next to the desktop
/// settings.
pub const FILE_NAME: &str = "protected-paths.txt";

/// Where the list lives: `%LOCALAPPDATA%\RillDisk` on Windows,
/// `~/Library/Application Support/RillDisk` on macOS and
/// `$XDG_STATE_HOME/rilldisk` (default `~/.local/state/rilldisk`) elsewhere.
pub fn config_path() -> Option<PathBuf> {
    let env = |key: &str| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let dir = if cfg!(windows) {
        env("LOCALAPPDATA")?.join("RillDisk")
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library/Application Support/RillDisk")
    } else {
        env("XDG_STATE_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".local/state")))?
            .join("rilldisk")
    };
    Some(dir.join(FILE_NAME))
}

/// A set of protected paths.
#[derive(Debug, Clone, Default)]
pub struct Protected {
    /// Each path as written and, when it differs, as resolved, so either
    /// spelling matches.
    spellings: Vec<PathBuf>,
    /// The paths as the user gave them, for display.
    paths: Vec<PathBuf>,
    /// Lines that were not usable, as sentences.
    pub problems: Vec<String>,
}

impl Protected {
    /// The user's list from [`config_path`]. A missing file is an empty list; a
    /// file that exists but cannot be read is an error, because silently
    /// ignoring it would drop protections the user relies on.
    pub fn load() -> Result<Self, String> {
        match config_path() {
            Some(path) => Self::from_file(&path),
            None => Ok(Self::default()),
        }
    }

    /// The list in `path`; a missing file is an empty list.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!(
                "could not read the protected paths in {}: {error}",
                path.display()
            )),
        }
    }

    /// Parse the file format: one path per line, `#` comments, blank lines.
    pub fn parse(text: &str) -> Self {
        let mut protected = Self::default();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim().trim_start_matches('\u{feff}');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Err(problem) = protected.add(Path::new(line)) {
                protected
                    .problems
                    .push(format!("line {}: {problem}", number + 1));
            }
        }
        protected
    }

    /// Protect `path`. It must be absolute after `~` expansion.
    pub fn add(&mut self, path: &Path) -> Result<(), String> {
        let path = expand_home(path);
        if !path.is_absolute() {
            return Err(format!(
                "{} is not an absolute path, so it was ignored",
                path.display()
            ));
        }
        if crate::cleanup::is_volume_root(&path) {
            // Protecting a whole drive would hide every candidate on it; allowed,
            // but it is almost always a typo, so say so.
            self.problems.push(format!(
                "{} is a whole drive or volume: nothing on it will be suggested",
                path.display()
            ));
        }
        self.paths.push(path.clone());
        if let Some(resolved) = path.canonicalize().ok().map(crate::simplify)
            && !crate::cleanup::same(&resolved, &path)
        {
            self.spellings.push(resolved);
        }
        self.spellings.push(path);
        Ok(())
    }

    /// Add every path of `other`.
    pub fn extend(&mut self, other: &Protected) {
        self.spellings.extend(other.spellings.iter().cloned());
        self.paths.extend(other.paths.iter().cloned());
        self.problems.extend(other.problems.iter().cloned());
    }

    pub fn is_empty(&self) -> bool {
        self.spellings.is_empty()
    }

    /// The protected paths as given.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Every spelling, for guards.
    pub(crate) fn spellings(&self) -> &[PathBuf] {
        &self.spellings
    }

    /// The protected path that `path` is, lies inside or contains, if any.
    pub fn covering(&self, path: &Path) -> Option<&Path> {
        self.spellings
            .iter()
            .find(|p| crate::cleanup::within(path, p) || crate::cleanup::within(p, path))
            .map(PathBuf::as_path)
    }
}

fn expand_home(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let rest = match text.strip_prefix('~') {
        Some("") => "",
        Some(rest) if rest.starts_with(['/', '\\']) => &rest[1..],
        _ => return path.to_path_buf(),
    };
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty());
    match home {
        Some(home) if rest.is_empty() => PathBuf::from(home),
        Some(home) => PathBuf::from(home).join(rest),
        None => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_blank_lines_home_and_rejects_relative_paths() {
        let base = tempfile::tempdir().unwrap();
        let keep = base.path().join("Work").join("Client");
        let text = format!(
            "\u{feff}# never touch\n\n{}\n  relative/path  \n~/notes\n",
            keep.display()
        );
        let protected = Protected::parse(&text);
        assert_eq!(protected.paths().len(), 2, "{:?}", protected.paths());
        assert_eq!(protected.paths()[0], keep);
        assert!(protected.paths()[1].is_absolute());
        assert!(protected.paths()[1].ends_with("notes"));
        assert_eq!(protected.problems.len(), 1, "{:?}", protected.problems);
        assert!(protected.problems[0].starts_with("line 4:"));
    }

    #[test]
    fn covers_the_path_what_is_inside_and_what_contains_it() {
        let base = tempfile::tempdir().unwrap();
        let keep = base.path().join("Work").join("Client");
        std::fs::create_dir_all(&keep).unwrap();
        let mut protected = Protected::default();
        protected.add(&keep).unwrap();
        let canonical = crate::simplify(keep.canonicalize().unwrap());
        assert!(protected.covering(&canonical).is_some());
        assert!(
            protected
                .covering(&canonical.join("node_modules"))
                .is_some()
        );
        assert!(protected.covering(canonical.parent().unwrap()).is_some());
        assert!(
            protected
                .covering(&canonical.parent().unwrap().join("Other"))
                .is_none()
        );
        // A sibling whose name only starts the same is not covered.
        assert!(
            protected
                .covering(&canonical.with_file_name("Client-old"))
                .is_none()
        );
        #[cfg(windows)]
        assert!(
            protected
                .covering(Path::new(
                    &canonical.to_string_lossy().to_uppercase().to_string()
                ))
                .is_some(),
            "Windows paths compare without case"
        );
    }

    #[test]
    fn a_missing_file_is_empty_and_an_unreadable_one_is_an_error() {
        let base = tempfile::tempdir().unwrap();
        let missing = Protected::from_file(&base.path().join("nope.txt")).unwrap();
        assert!(missing.is_empty());
        // A folder in place of the file cannot be read as text.
        let folder = base.path().join(FILE_NAME);
        std::fs::create_dir(&folder).unwrap();
        assert!(Protected::from_file(&folder).is_err());
    }
}
