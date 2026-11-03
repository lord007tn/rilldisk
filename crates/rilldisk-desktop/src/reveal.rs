// SPDX-License-Identifier: AGPL-3.0-only
//! "Show in Explorer": hand one path to the platform file manager.
//!
//! Scoped by docs/proposals/reveal.md. The file manager is a fixed system program
//! started without a shell; it is given a single absolute path and never asked to
//! open a file, so nothing is executed or read. A folder opens in a window (Finder
//! reveals it instead, because a bundle folder would launch); a file is selected
//! in its folder. Only the desktop app does this; the CLI and MCP never do.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Menu wording for this platform.
pub fn label() -> &'static str {
    if cfg!(windows) {
        "Show in Explorer"
    } else if cfg!(target_os = "macos") {
        "Show in Finder"
    } else {
        "Show in file manager"
    }
}

/// What to start for `path`. `folder` is true only for a real directory
/// (not a link to one), so a file can never be handed over to be opened.
#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    /// Windows: one argument passed verbatim, because Explorer parses its own
    /// command line. Elsewhere: ordinary arguments.
    pub args: Vec<String>,
}

pub fn launch(path: &Path, folder: bool) -> Launch {
    let text = path.display().to_string();
    if cfg!(windows) {
        // `\\?\C:\x` is not understood by Explorer; `C:\x` is.
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text).to_string();
        let root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        // Windows names cannot contain `"`, so quoting is safe. A drive root keeps
        // no quotes: Explorer reads `"C:\"` as an escaped quote.
        let quoted = if text.ends_with('\\') {
            text
        } else {
            format!("\"{text}\"")
        };
        let arg = if folder {
            quoted
        } else {
            format!("/select,{quoted}")
        };
        Launch {
            program: root.join("explorer.exe"),
            args: vec![arg],
        }
    } else if cfg!(target_os = "macos") {
        Launch {
            program: PathBuf::from("/usr/bin/open"),
            args: vec!["-R".into(), text],
        }
    } else {
        // xdg-open only ever receives a folder: the item itself, or its parent.
        let dir = if folder {
            text
        } else {
            path.parent()
                .map_or(text.clone(), |p| p.display().to_string())
        };
        Launch {
            program: PathBuf::from("xdg-open"),
            args: vec![dir],
        }
    }
}

/// Show `path` in the file manager. Errors are short sentences for a toast.
pub fn show(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Only full paths can be shown".into());
    }
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| "It is no longer there. Rescan to refresh".to_string())?;
    let launch = launch(path, meta.is_dir());
    let mut command = Command::new(&launch.program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        for arg in &launch.args {
            command.raw_arg(arg);
        }
    }
    #[cfg(not(windows))]
    command.args(&launch.args);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start the file manager ({e})"))?;
    // Reap it so no zombie is left behind; Explorer's exit code means nothing.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn explorer_opens_folders_and_selects_files() {
        let folder = launch(Path::new(r"C:\Users\Me\My Projects"), true);
        assert!(folder.program.ends_with("explorer.exe"));
        assert_eq!(folder.args, vec![r#""C:\Users\Me\My Projects""#]);
        let file = launch(Path::new(r"C:\a,b\movie.mp4"), false);
        assert_eq!(file.args, vec![r#"/select,"C:\a,b\movie.mp4""#]);
        // Drive roots stay unquoted; verbatim prefixes are dropped.
        assert_eq!(launch(Path::new(r"D:\"), true).args, vec![r"D:\"]);
        assert_eq!(
            launch(Path::new(r"\\?\C:\long"), false).args,
            vec![r#"/select,"C:\long""#]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finder_always_reveals() {
        for folder in [true, false] {
            let l = launch(Path::new("/Users/me/App.app"), folder);
            assert_eq!(l.args, vec!["-R", "/Users/me/App.app"]);
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn xdg_open_only_gets_folders() {
        assert_eq!(
            launch(Path::new("/home/me/a.iso"), false).args,
            vec!["/home/me"]
        );
        assert_eq!(
            launch(Path::new("/home/me/dir"), true).args,
            vec!["/home/me/dir"]
        );
    }

    #[test]
    fn relative_and_missing_paths_are_refused() {
        assert!(show(Path::new("relative/path")).is_err());
        let missing = std::env::temp_dir().join("rilldisk-reveal-missing-7c1e");
        assert!(show(&missing).is_err());
    }
}
