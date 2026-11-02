// SPDX-License-Identifier: AGPL-3.0-only
//! Update API for the desktop app. Network work runs on background threads;
//! the UI polls the returned handles once per frame.
//!
//! Intended use (scope in docs/proposals/updates.md):
//!
//! ```ignore
//! // On startup. Honours RILLDISK_NO_UPDATE_CHECK, the "automatic checks"
//! // preference and the once-a-day throttle; returns Ok(None) when skipped.
//! self.update_check = Some(update::check(ctx, update::Trigger::Automatic));
//!
//! // Each frame:
//! if let Some(result) = self.update_check.as_ref().and_then(|c| c.poll()) {
//!     self.available = result.ok().flatten(); // show a banner, never auto-install
//! }
//!
//! // Only after the user clicked "Install and restart":
//! if release.update_command.is_none() || release.msi {
//!     self.install = Some(update::install_and_restart(ctx, release.clone()));
//! } // other package-managed installs keep their own update command
//! // Each frame: install.progress() for a bar, install.poll() for errors.
//! // On success the app relaunches and this window receives a Close command.
//! ```

use eframe::egui;
use rilldisk_update::{self as up, Channel, InstallSource, Plan, State};
use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// A newer release the user can be offered.
#[derive(Clone, Debug)]
pub struct Release {
    /// Semver without the leading `v`, e.g. `0.3.0` or `0.4.0-alpha.1`.
    pub version: String,
    /// GitHub release page with the changelog.
    pub notes_url: String,
    /// Set when a package manager owns this install: show this command
    /// instead of an install button. `install_and_restart` refuses then.
    pub update_command: Option<String>,
    /// Direct download of the matching new package (MSI, .deb, .rpm,
    /// AppImage or .dmg) when one of RillDisk's own packages installed this.
    pub download_url: Option<String>,
    /// Set when this copy cannot replace itself (for example a renamed program):
    /// show this instead of an install button.
    pub blocked: Option<String>,
    pub msi: bool,
    inner: up::Release,
}

/// Why a check runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Startup check: skipped (Ok(None)) when disabled or checked < 24 h ago.
    Automatic,
    /// The user asked: always contacts GitHub unless the env opt-out is set.
    Manual,
}

/// Result of a background job, delivered once.
pub struct Pending<T> {
    rx: mpsc::Receiver<T>,
}

impl<T> Pending<T> {
    /// `Some` exactly once, when the job finishes; `None` before and after.
    pub fn poll(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Verifying,
    WaitingPermission,
    #[cfg(windows)]
    Installing,
    Restarting,
    Stopped,
}
impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Downloading => "Downloading update",
            Self::Verifying => "Verifying and applying update",
            Self::WaitingPermission => "Waiting for Windows permission",
            #[cfg(windows)]
            Self::Installing => "Installing with Windows Installer",
            Self::Restarting => "Restarting RillDisk",
            Self::Stopped => "Update worker stopped",
        }
    }
}

/// A running install with download progress.
pub struct Install {
    pub result: Pending<Result<(), String>>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    phase: Arc<std::sync::Mutex<Phase>>,
    finished: Cell<bool>,
}

impl Install {
    /// Download progress in `0.0..=1.0`, if the size is known.
    pub fn progress(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0 && self.phase() == Phase::Downloading)
            .then(|| (self.done.load(Ordering::Relaxed) as f32 / total as f32).clamp(0.0, 1.0))
    }

    pub fn phase(&self) -> Phase {
        self.phase.lock().map(|p| *p).unwrap_or(Phase::Stopped)
    }

    pub fn bytes(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    pub fn poll(&self) -> Option<Result<(), String>> {
        if self.finished.get() {
            return None;
        }
        match self.result.rx.try_recv() {
            Ok(result) => {
                self.finished.set(true);
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.finished.set(true);
                Some(Err(
                    "The update worker stopped unexpectedly. Try again.".into()
                ))
            }
        }
    }
}

fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    job: impl FnOnce() -> T + Send + 'static,
) -> Pending<T> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("rilldisk-update".into())
        .spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        })
        .expect("spawn update thread");
    Pending { rx }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn install_source() -> InstallSource {
    std::env::current_exe()
        .map(|exe| up::detect_install(&exe.canonicalize().unwrap_or(exe)))
        .unwrap_or(InstallSource::Standalone)
}

/// Look for a newer release in the background.
pub fn check(ctx: &egui::Context, trigger: Trigger) -> Pending<Result<Option<Release>, String>> {
    spawn(ctx, move || check_now(trigger))
}

fn check_now(trigger: Trigger) -> Result<Option<Release>, String> {
    if up::disabled_by_env() {
        return match trigger {
            Trigger::Automatic => Ok(None),
            Trigger::Manual => Err(format!(
                "Update checks are disabled by {}.",
                up::DISABLE_ENV
            )),
        };
    }
    let mut state = State::load();
    if trigger == Trigger::Automatic {
        let now = now_unix();
        if !state.automatic_check_due(now, false) {
            return Ok(None);
        }
        state.last_check_unix = Some(now);
        let _ = state.save();
    }
    let mut blocked = None;
    let mut package = None;
    let update_command = match install_source() {
        InstallSource::Managed {
            command,
            package: kind,
            ..
        } => {
            package = kind;
            Some(command.to_owned())
        }
        InstallSource::Standalone => {
            match Plan::for_current_exe() {
                Ok(plan) => up::cleanup_previous_update(&plan.dir),
                Err(error) => {
                    blocked = Some(format!(
                        "This copy cannot update itself ({error:#}). Download the new version from the release page."
                    ))
                }
            }
            None
        }
    };
    let current = up::current_version();
    let channel = Channel::resolve(state.channel, &current).map_err(|e| format!("{e:#}"))?;
    let check = up::check(channel).map_err(|e| format!("{e:#}"))?;
    Ok(check.update.map(|inner| Release {
        version: inner.version.to_string(),
        notes_url: inner.notes_url.clone(),
        download_url: package
            .zip(up::target())
            .and_then(|(package, target)| inner.package_asset(package.kind(), target))
            .map(|asset| asset.url.clone()),
        msi: cfg!(windows)
            && package == Some(up::Package::Msi)
            && package
                .zip(up::target())
                .is_some_and(|(p, target)| inner.package_asset(p.kind(), target).is_some()),
        update_command,
        blocked,
        inner,
    }))
}

/// Download, verify (SHA-256 against the release's SHA256SUMS) and replace
/// `rilldisk-desktop` plus a sibling `rilldisk` CLI, then relaunch the app and
/// close this window. Call only after the user agreed. On failure nothing is
/// replaced and the error is returned for display.
pub fn install_and_restart(ctx: &egui::Context, release: Release) -> Install {
    let done = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let phase = Arc::new(std::sync::Mutex::new(Phase::Downloading));
    let (job_done, job_total, job_ctx, job_phase) =
        (done.clone(), total.clone(), ctx.clone(), phase.clone());
    let result = spawn(ctx, move || {
        let set_phase = |value: Phase| {
            if let Ok(mut phase) = job_phase.lock() {
                *phase = value;
            }
            job_ctx.request_repaint();
        };
        #[cfg(windows)]
        if release.msi {
            let prepared = up::msi::prepare(&release.inner, &mut |received, size| {
                job_done.store(received, Ordering::Relaxed);
                job_total.store(size.unwrap_or(0), Ordering::Relaxed);
                if size.is_some_and(|total| received >= total) {
                    set_phase(Phase::Verifying);
                }
                job_ctx.request_repaint();
            })
            .map_err(|e| format!("{e:#}"))?;
            set_phase(Phase::WaitingPermission);
            let mut handoff = prepared.start().map_err(|e| format!("{e:#}"))?;
            loop {
                match handoff.status().map_err(|e| format!("{e:#}"))? {
                    up::msi::Status::AwaitingConsent => {}
                    up::msi::Status::InstallerStarted | up::msi::Status::Finished { .. } => {
                        set_phase(Phase::Installing);
                        job_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        return Ok(());
                    }
                    up::msi::Status::Failed(error) => return Err(error),
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        if let Some(command) = release.update_command {
            return Err(format!(
                "This copy is managed by a package manager. Update with: {command}"
            ));
        }
        if let Some(reason) = release.blocked {
            return Err(reason);
        }
        let plan = Plan::for_current_exe().map_err(|e| format!("{e:#}"))?;
        let mut last_repaint = 0;
        up::install(&release.inner, &plan, &mut |received, size| {
            job_done.store(received, Ordering::Relaxed);
            job_total.store(size.unwrap_or(0), Ordering::Relaxed);
            if size.is_some_and(|total| received >= total) {
                set_phase(Phase::Verifying);
            }
            if received.saturating_sub(last_repaint) >= 1 << 20 {
                last_repaint = received;
                job_ctx.request_repaint();
            }
        })
        .map_err(|e| format!("{e:#}"))?;
        set_phase(Phase::Restarting);
        restart(&job_ctx, &plan)
    });
    Install {
        result,
        done,
        total,
        phase,
        finished: Cell::new(false),
    }
}

/// Launch the freshly installed desktop binary with the same arguments, then
/// ask this window to close.
fn restart(ctx: &egui::Context, plan: &Plan) -> Result<(), String> {
    let exe = plan
        .dir
        .join(format!("rilldisk-desktop{}", std::env::consts::EXE_SUFFIX));
    std::process::Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .spawn()
        .map_err(|e| {
            format!(
                "Updated, but could not relaunch {}: {e}. Start it again manually.",
                exe.display()
            )
        })?;
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    Ok(())
}

/// The "check for updates automatically" preference.
pub fn automatic_checks() -> bool {
    !up::disabled_by_env() && State::load().automatic_checks
}

pub fn set_automatic_checks(enabled: bool) -> Result<(), String> {
    let mut state = State::load();
    state.automatic_checks = enabled;
    state.save().map_err(|e| format!("{e:#}"))
}

/// Installer receipts are displayed by the desktop, never fabricated from a launch.
pub fn installer_notice() -> Option<Result<String, String>> {
    #[cfg(windows)]
    match up::msi::take_results() {
        Ok(results) if !results.is_empty() => {
            let mut notices = Vec::new();
            let mut failed = false;
            for result in results {
                match result {
                    up::msi::Status::Finished {
                        version,
                        reboot_required,
                    } => notices.push(if reboot_required {
                        format!("Restart Windows to finish updating to {version}")
                    } else {
                        format!("Updated to RillDisk {version}")
                    }),
                    up::msi::Status::Failed(error) => {
                        failed = true;
                        notices.push(error);
                    }
                    _ => {}
                }
            }
            let message = notices.join("\n");
            return Some(if failed { Err(message) } else { Ok(message) });
        }
        Err(error) => {
            return Some(Err(format!(
                "Could not read the installer result: {error:#}"
            )));
        }
        _ => {}
    }
    None
}

#[derive(Clone, Copy)]
pub enum Preview {
    Available,
    Downloading,
    UnknownLength,
    Verifying,
    Permission,
    Declined,
    BadChecksum,
}

/// Scripted evidence uses fictional release metadata and a controlled worker;
/// it does not contact GitHub or execute an installer.
pub fn preview_release() -> Release {
    let feed = include_bytes!("../../rilldisk-update/tests/fixtures/latest-packages.yml");
    let feed = String::from_utf8_lossy(feed).replace("0.1.1", "0.1.3");
    let inner = up::parse_feed(feed.as_bytes()).expect("valid update preview fixture");
    Release {
        version: "0.1.3 (preview)".into(),
        notes_url: inner.notes_url.clone(),
        update_command: None,
        download_url: None,
        blocked: None,
        msi: true,
        inner,
    }
}

pub fn preview_install(
    phase: Phase,
    done: u64,
    total: u64,
) -> (Install, mpsc::Sender<Result<(), String>>) {
    let (tx, rx) = mpsc::channel();
    (
        Install {
            result: Pending { rx },
            done: Arc::new(AtomicU64::new(done)),
            total: Arc::new(AtomicU64::new(total)),
            phase: Arc::new(std::sync::Mutex::new(phase)),
            finished: Cell::new(false),
        },
        tx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentages_represent_only_known_download_bytes() {
        let (known, _tx) = preview_install(Phase::Downloading, 50, 100);
        assert_eq!(known.progress(), Some(0.5));
        let (unknown, _tx) = preview_install(Phase::Downloading, 50, 0);
        assert_eq!(unknown.progress(), None);
        let (verifying, _tx) = preview_install(Phase::Verifying, 100, 100);
        assert_eq!(verifying.progress(), None);
    }
    #[test]
    fn failed_or_disconnected_worker_reports_once_and_can_be_retried() {
        let (failed, tx) = preview_install(Phase::Downloading, 0, 0);
        tx.send(Err("interrupted".into())).unwrap();
        assert_eq!(failed.poll(), Some(Err("interrupted".into())));
        assert_eq!(failed.poll(), None);
        let (disconnected, tx) = preview_install(Phase::Downloading, 0, 0);
        drop(tx);
        assert!(disconnected.poll().unwrap().is_err());
        assert_eq!(disconnected.poll(), None);
    }
}
