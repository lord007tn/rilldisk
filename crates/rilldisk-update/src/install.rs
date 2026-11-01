// SPDX-License-Identifier: AGPL-3.0-only
//! Decide whether this installation may update itself, then swap binaries.
//!
//! The swap never runs another program. Each new executable is written next to
//! its target, the old one is renamed to `<name>.old` (Windows allows renaming
//! a running executable, not overwriting it) and the new one is renamed into
//! place. Any failure rolls every completed rename back.

use crate::{
    BINARIES, CHECKSUMS_ASSET, REPOSITORY, Release, archive_name, extract_binaries, net,
    parse_sha256sums, target, verify_sha256,
};
use anyhow::{Context, Result, bail};
use semver::Version;
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const MAX_ARCHIVE: u64 = 1 << 30;
const MAX_CHECKSUMS: u64 = 1 << 20;

/// How the running binaries were installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallSource {
    /// Unpacked from a release archive: RillDisk may replace its own files.
    Standalone,
    /// Owned by a package manager or a source build: never modified; the user
    /// is told which command updates it instead.
    Managed {
        manager: &'static str,
        command: &'static str,
    },
}

/// Classify the installation from the executable path. Packagers can also set
/// `RILLDISK_PACKAGE_MANAGER` (and optionally `RILLDISK_UPDATE_COMMAND`) at
/// build time to switch self-update off entirely.
pub fn detect_install(exe: &Path) -> InstallSource {
    if let Some(manager) = option_env!("RILLDISK_PACKAGE_MANAGER") {
        return InstallSource::Managed {
            manager,
            command: option_env!("RILLDISK_UPDATE_COMMAND")
                .unwrap_or("update RillDisk with the package manager that installed it"),
        };
    }
    detect_from_path(exe)
}

fn detect_from_path(exe: &Path) -> InstallSource {
    let path = exe
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let managed = |manager, command| InstallSource::Managed { manager, command };
    if path.contains("/scoop/apps/") {
        managed("Scoop", "scoop update rilldisk")
    } else if path.contains("/winget/packages/") || path.contains("/windowsapps/") {
        managed("winget", "winget upgrade rilldisk")
    } else if path.contains("/cellar/") || path.starts_with("/opt/homebrew/") {
        managed("Homebrew", "brew upgrade rilldisk")
    } else if path.starts_with("/nix/store/") {
        managed(
            "Nix",
            "update RillDisk through your Nix profile or configuration",
        )
    } else if path.starts_with("/snap/") {
        managed("Snap", "snap refresh rilldisk")
    } else if [
        "/usr/bin/",
        "/usr/sbin/",
        "/usr/lib/",
        "/usr/libexec/",
        "/bin/",
    ]
    .iter()
    .any(|p| path.starts_with(p))
    {
        managed(
            "a system package",
            "update RillDisk with your distribution's package manager",
        )
    } else if path.contains("/.cargo/bin/") {
        managed(
            "Cargo",
            "cargo install --locked --git https://github.com/lord007tn/rilldisk rilldisk rilldisk-desktop",
        )
    } else if path.contains("/target/debug/") || path.contains("/target/release/") {
        managed(
            "a source build",
            "git pull && cargo build --release --locked",
        )
    } else {
        InstallSource::Standalone
    }
}

fn exe_name(base: &str) -> String {
    format!("{base}{}", std::env::consts::EXE_SUFFIX)
}

/// The executables an update will replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub dir: PathBuf,
    /// The running executable plus any sibling RillDisk executable present.
    pub binaries: Vec<PathBuf>,
}

impl Plan {
    /// Plan for the running executable. Symlinks are resolved on Unix so a
    /// Homebrew-style link is classified by where the file really lives.
    pub fn for_current_exe() -> Result<Self> {
        let exe = std::env::current_exe().context("cannot locate the running executable")?;
        let exe = if cfg!(unix) {
            exe.canonicalize().unwrap_or(exe)
        } else {
            exe
        };
        Self::for_exe(&exe)
    }

    pub fn for_exe(exe: &Path) -> Result<Self> {
        let dir = exe
            .parent()
            .context("the executable has no parent directory")?
            .to_path_buf();
        let file = exe.file_name().unwrap_or_default().to_string_lossy();
        if !BINARIES.iter().any(|b| exe_name(b) == file) {
            bail!(
                "{} is not named like a RillDisk release binary; update it manually from https://github.com/{REPOSITORY}/releases",
                exe.display()
            );
        }
        let binaries: Vec<PathBuf> = BINARIES
            .iter()
            .map(|b| dir.join(exe_name(b)))
            .filter(|p| p == exe || p.is_file())
            .collect();
        // Replacing a link would leave the file it points to on the old version.
        for binary in &binaries {
            if binary != exe
                && std::fs::symlink_metadata(binary).is_ok_and(|m| m.file_type().is_symlink())
            {
                bail!(
                    "{} is a symbolic link; update the file it points to, or download the release from https://github.com/{REPOSITORY}/releases",
                    binary.display()
                );
            }
        }
        Ok(Self { dir, binaries })
    }
}

/// Result of a successful install.
#[derive(Clone, Debug)]
pub struct Installed {
    pub version: Version,
    pub replaced: Vec<PathBuf>,
}

/// Download `release` for this platform, verify it against the release's
/// `SHA256SUMS` and replace the binaries in `plan`. Call only after the user
/// agreed. `progress` receives `(bytes, total)` for the archive download.
pub fn install(
    release: &Release,
    plan: &Plan,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Installed> {
    // Package-managed files are never modified, whoever calls this.
    for binary in &plan.binaries {
        if let InstallSource::Managed { manager, command } = detect_install(binary) {
            anyhow::bail!(
                "{} is managed by {manager}; update it with: {command}",
                binary.display()
            );
        }
    }
    let target = target().context("no prebuilt RillDisk release exists for this platform")?;
    let name = archive_name(&release.tag, target);
    let archive = release
        .asset(&name)
        .with_context(|| format!("release {} has no {name}", release.tag))?;
    // The update feed carries the checksum; API-listed releases use SHA256SUMS.
    let expected = match release.checksums.get(&name) {
        Some(sha) => sha.clone(),
        None => {
            let sums = release.asset(CHECKSUMS_ASSET).with_context(|| {
                format!(
                    "release {} has no {CHECKSUMS_ASSET}; refusing to install unverified files",
                    release.tag
                )
            })?;
            let sums = net::download(&sums.url, MAX_CHECKSUMS, &mut |_, _| {})?;
            parse_sha256sums(&String::from_utf8_lossy(&sums))
                .remove(&name)
                .with_context(|| format!("{CHECKSUMS_ASSET} does not list {name}"))?
        }
    };
    let bytes = net::download(&archive.url, MAX_ARCHIVE, progress)?;
    apply_archive(&name, &bytes, &expected, plan)?;
    Ok(Installed {
        version: release.version.clone(),
        replaced: plan.binaries.clone(),
    })
}

/// Verify `bytes` against `expected_sha256`, then replace every binary in
/// `plan` with the same-named file from the archive. Nothing on disk changes
/// unless the checksum matches and every needed binary is in the archive.
pub fn apply_archive(
    archive_name: &str,
    bytes: &[u8],
    expected_sha256: &str,
    plan: &Plan,
) -> Result<()> {
    verify_sha256(bytes, expected_sha256)?;
    if plan.binaries.is_empty() {
        bail!("nothing to update: the plan lists no RillDisk binaries");
    }
    let names: Vec<String> = plan
        .binaries
        .iter()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let mut found = extract_binaries(archive_name, bytes, &names)?;
    let files = plan
        .binaries
        .iter()
        .zip(&names)
        .map(|(path, name)| {
            found
                .remove(name)
                .map(|data| (path.clone(), data))
                .with_context(|| format!("the release archive does not contain {name}"))
        })
        .collect::<Result<Vec<_>>>()?;
    replace_files(&files)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

fn write_executable(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let _ = fs::remove_file(path);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o755);
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

/// A free `<name>.old[.N]` path. A previous `.old` may still be locked on
/// Windows when that older program is still running.
fn backup_path(target: &Path) -> PathBuf {
    let base = with_suffix(target, ".old");
    if !base.exists() || fs::remove_file(&base).is_ok() {
        return base;
    }
    (1..)
        .map(|n| with_suffix(target, &format!(".old.{n}")))
        .find(|p| !p.exists())
        .expect("an unused backup name exists")
}

fn replace_files(files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    let staged: Vec<PathBuf> = files.iter().map(|(t, _)| with_suffix(t, ".new")).collect();
    let discard = |staged: &[PathBuf]| {
        for path in staged {
            let _ = fs::remove_file(path);
        }
    };
    for ((target, data), new) in files.iter().zip(&staged) {
        if let Err(error) = write_executable(new, data) {
            discard(&staged);
            return Err(error).with_context(|| {
                format!(
                    "cannot write next to {}; is the folder writable?",
                    target.display()
                )
            });
        }
    }
    let mut swapped: Vec<(PathBuf, PathBuf)> = Vec::new();
    for ((target, _), new) in files.iter().zip(&staged) {
        let old = backup_path(target);
        let result = fs::rename(target, &old).and_then(|()| {
            fs::rename(new, target).inspect_err(|_| {
                let _ = fs::rename(&old, target);
            })
        });
        if let Err(error) = result {
            for (done, backup) in swapped.iter().rev() {
                let _ = fs::remove_file(done);
                let _ = fs::rename(backup, done);
            }
            discard(&staged);
            return Err(error).with_context(|| format!("cannot replace {}", target.display()));
        }
        swapped.push((target.clone(), old));
    }
    // Running executables cannot be deleted on Windows; the next start cleans up.
    for (_, old) in swapped {
        let _ = fs::remove_file(old);
    }
    Ok(())
}

/// Best-effort removal of `*.old` / `*.new` files left by an earlier update.
pub fn cleanup_previous_update(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let names: Vec<String> = BINARIES.iter().map(|b| exe_name(b)).collect();
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let leftover = names.iter().any(|n| {
            file.strip_prefix(n.as_str()).is_some_and(|rest| {
                rest == ".new"
                    || rest == ".old"
                    || rest
                        .strip_prefix(".old.")
                        .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            })
        });
        if leftover {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_manager_paths_are_left_alone() {
        let cases = [
            (
                r"C:\Users\me\scoop\apps\rilldisk\current\rilldisk.exe",
                Some("Scoop"),
            ),
            (
                r"C:\Users\me\AppData\Local\Microsoft\WinGet\Packages\RillDisk_x\rilldisk.exe",
                Some("winget"),
            ),
            (
                "/opt/homebrew/Cellar/rilldisk/0.3.0/bin/rilldisk",
                Some("Homebrew"),
            ),
            (
                "/usr/local/Cellar/rilldisk/0.3.0/bin/rilldisk",
                Some("Homebrew"),
            ),
            ("/nix/store/abc-rilldisk-0.3.0/bin/rilldisk", Some("Nix")),
            ("/usr/bin/rilldisk", Some("a system package")),
            ("/home/me/.cargo/bin/rilldisk", Some("Cargo")),
            (
                "/home/me/src/rilldisk/target/release/rilldisk",
                Some("a source build"),
            ),
            (r"C:\Tools\RillDisk\rilldisk.exe", None),
            ("/home/me/apps/rilldisk-v0.3.0/rilldisk", None),
            ("/usr/local/bin/rilldisk", None),
        ];
        for (path, expected) in cases {
            let got = match detect_from_path(Path::new(path)) {
                InstallSource::Standalone => None,
                InstallSource::Managed { manager, .. } => Some(manager),
            };
            assert_eq!(got, expected, "{path}");
        }
    }

    #[test]
    fn leftover_names() {
        let dir = tempfile::tempdir().unwrap();
        let exe = exe_name("rilldisk");
        for name in [
            format!("{exe}.old"),
            format!("{exe}.old.3"),
            format!("{exe}.new"),
            exe.clone(),
            format!("{exe}.older"),
            "notes.old".to_owned(),
        ] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        cleanup_previous_update(dir.path());
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected = vec![exe.clone(), format!("{exe}.older"), "notes.old".to_owned()];
        expected.sort();
        assert_eq!(left, expected);
    }
}
