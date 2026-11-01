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
//! if release.update_command.is_none() {
//!     self.install = Some(update::install_and_restart(ctx, release.clone()));
//! } // else show release.update_command (package-managed install)
//! // Each frame: install.progress() for a bar, install.poll() for errors.
//! // On success the app relaunches and this window receives a Close command.
//! ```

use eframe::egui;
use rilldisk_update::{self as up, Channel, InstallSource, Plan, State};
use std::{
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
    /// Set when this copy cannot replace itself (for example a renamed program):
    /// show this instead of an install button.
    pub blocked: Option<String>,
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

/// A running install with download progress.
pub struct Install {
    pub result: Pending<Result<(), String>>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

impl Install {
    /// Download progress in `0.0..=1.0`, if the size is known.
    pub fn progress(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| self.done.load(Ordering::Relaxed) as f32 / total as f32)
    }

    pub fn poll(&self) -> Option<Result<(), String>> {
        self.result.poll()
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
    let update_command = match install_source() {
        InstallSource::Managed { command, .. } => Some(command.to_owned()),
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
    let (job_done, job_total, job_ctx) = (done.clone(), total.clone(), ctx.clone());
    let result = spawn(ctx, move || {
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
            if received - last_repaint >= 1 << 20 {
                last_repaint = received;
                job_ctx.request_repaint();
            }
        })
        .map_err(|e| format!("{e:#}"))?;
        restart(&job_ctx, &plan)
    });
    Install {
        result,
        done,
        total,
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
