// SPDX-License-Identifier: AGPL-3.0-only
//! Desktop update preferences and the once-a-day throttle.
//!
//! Stored as a small JSON file in the per-user app data folder. It holds only
//! the preference, the channel override and the time of the last check.

use crate::Channel;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

/// Minimum time between automatic checks.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Daily automatic checks in the desktop app. Explicit checks still work
    /// when this is off; `RILLDISK_NO_UPDATE_CHECK` blocks both.
    pub automatic_checks: bool,
    /// Overrides the channel derived from the running version.
    pub channel: Option<Channel>,
    /// Unix seconds of the last automatic check attempt, successful or not.
    pub last_check_unix: Option<u64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            automatic_checks: true,
            channel: None,
            last_check_unix: None,
        }
    }
}

impl State {
    /// Load the saved state; a missing or unreadable file means defaults.
    pub fn load() -> Self {
        state_path()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = state_path().context("no per-user data folder is available")?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&temp, &path).with_context(|| format!("cannot write {}", path.display()))
    }

    /// Whether an automatic check should run now. Clock changes that move
    /// time backwards count as "due" so a bad timestamp cannot block checks.
    pub fn automatic_check_due(&self, now_unix: u64, disabled_by_env: bool) -> bool {
        if disabled_by_env || !self.automatic_checks {
            return false;
        }
        match self.last_check_unix {
            None => true,
            Some(last) => now_unix < last || now_unix - last >= CHECK_INTERVAL_SECS,
        }
    }
}

/// `update.json` in the per-user RillDisk data folder:
/// `%LOCALAPPDATA%\RillDisk` on Windows, `~/Library/Application Support/RillDisk`
/// on macOS, `$XDG_STATE_HOME/rilldisk` (default `~/.local/state/rilldisk`) elsewhere.
pub fn state_path() -> Option<PathBuf> {
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
    Some(dir.join("update.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_throttle() {
        let day = CHECK_INTERVAL_SECS;
        let fresh = State::default();
        assert!(fresh.automatic_check_due(1_000_000, false));
        assert!(!fresh.automatic_check_due(1_000_000, true));
        let checked = State {
            last_check_unix: Some(1_000_000),
            ..State::default()
        };
        assert!(!checked.automatic_check_due(1_000_000 + day - 1, false));
        assert!(checked.automatic_check_due(1_000_000 + day, false));
        assert!(
            checked.automatic_check_due(999_000, false),
            "clock moved back"
        );
        let off = State {
            automatic_checks: false,
            ..State::default()
        };
        assert!(!off.automatic_check_due(1_000_000, false));
    }

    #[test]
    fn state_json_is_forward_compatible() {
        let state: State = serde_json::from_str(r#"{"channel":"prerelease","future":1}"#).unwrap();
        assert!(state.automatic_checks);
        assert_eq!(state.channel, Some(Channel::Prerelease));
        let state: State = serde_json::from_str(r#"{"automatic_checks":false}"#).unwrap();
        assert!(!state.automatic_checks);
    }
}
