// SPDX-License-Identifier: AGPL-3.0-only
//! `rilldisk reclaim --delete`: list, confirm, then remove through the shared
//! core cleanup module. See docs/proposals/cleanup.md.
use crate::human;
use anyhow::{Result, bail};
use rilldisk_core::{
    CleanupMode, ReclaimReport, Snapshot,
    cleanup::{self, CleanupControl, Guard, Outcome},
};
use std::{
    io::{self, BufRead, IsTerminal, Write},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct Options {
    pub permanent: bool,
    pub yes: bool,
    pub json: bool,
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static CONTROL: LazyLock<Arc<CleanupControl>> = LazyLock::new(Arc::default);

/// Called on Ctrl+C. Returns true when a cleanup was running and will stop.
pub fn cancel_if_running() -> bool {
    if !RUNNING.load(Ordering::Relaxed) {
        return false;
    }
    if !CONTROL.is_cancelled() {
        eprintln!("Stopping after the current item...");
    }
    CONTROL.cancel();
    true
}

/// Returns the process exit code.
pub fn run(
    writer: &mut dyn Write,
    snapshot: &Snapshot,
    report: &ReclaimReport,
    options: Options,
) -> Result<u8> {
    if snapshot.summary.cancelled {
        bail!("the scan was cancelled; nothing was removed");
    }
    let mode = if options.permanent {
        CleanupMode::Permanent
    } else {
        CleanupMode::Trash
    };
    let guard = Guard::system();
    let selected: Vec<usize> = report.candidates.iter().map(|c| c.entry_id).collect();
    let plan = cleanup::plan(snapshot, report, &selected, mode, &guard);
    let mut err = io::stderr().lock();
    if plan.items.is_empty() {
        for refusal in &plan.refused {
            writeln!(err, "skip  {}: {}", refusal.path.display(), refusal.reason)?;
        }
        if options.json {
            serde_json::to_writer(
                &mut *writer,
                &cleanup::execute(
                    snapshot,
                    report,
                    &selected,
                    mode,
                    &guard,
                    &CleanupControl::default(),
                ),
            )?;
            writeln!(writer)?;
        } else {
            writeln!(
                writer,
                "Nothing to remove: no candidate matches the filters."
            )?;
        }
        return Ok(if plan.refused.is_empty() { 0 } else { 3 });
    }
    let destination = match mode {
        CleanupMode::Trash => format!("Move to the {}", CleanupMode::trash_name()),
        CleanupMode::Permanent => "PERMANENTLY DELETE".to_string(),
    };
    if !options.yes {
        if !(io::stdin().is_terminal() && io::stderr().is_terminal()) {
            bail!(
                "refusing to remove {} folders without confirmation: stdin is not a terminal. Review the list with `rilldisk reclaim`, then pass --yes",
                plan.items.len()
            );
        }
        if !snapshot.summary.complete {
            writeln!(
                err,
                "Note: the scan was partial ({} issues); sizes may be incomplete.",
                snapshot.summary.issue_count
            )?;
        }
        writeln!(err, "{destination}:")?;
        for item in &plan.items {
            writeln!(
                err,
                "  {:>10}  {:<12}  {:<24}  {}",
                human(item.exclusive_bytes),
                item.safety.label(),
                item.label,
                item.path.display()
            )?;
        }
        for refusal in &plan.refused {
            writeln!(
                err,
                "  {:>10}  skipped: {} {}",
                "",
                refusal.path.display(),
                refusal.reason
            )?;
        }
        writeln!(
            err,
            "{} folders, {} files, frees at least {}{}.",
            plan.items.len(),
            plan.total_files,
            human(plan.total_exclusive_bytes),
            if mode == CleanupMode::Trash {
                format!(" once the {} is emptied", CleanupMode::trash_name())
            } else {
                String::new()
            }
        )?;
        if mode == CleanupMode::Permanent {
            write!(err, "This cannot be undone. Type 'delete' to continue: ")?;
        } else {
            write!(err, "Continue? [y/N] ")?;
        }
        err.flush()?;
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer)?;
        let answer = answer.trim().to_ascii_lowercase();
        let confirmed = match mode {
            CleanupMode::Permanent => answer == "delete",
            CleanupMode::Trash => answer == "y" || answer == "yes",
        };
        if !confirmed {
            writeln!(err, "Cancelled; nothing was removed.")?;
            return Ok(1);
        }
    }
    drop(err);
    RUNNING.store(true, Ordering::Relaxed);
    let control = CONTROL.clone();
    let progress = {
        let control = control.clone();
        let total = plan.items.len();
        std::thread::spawn(move || {
            while RUNNING.load(Ordering::Relaxed) {
                std::thread::park_timeout(std::time::Duration::from_secs(1));
                if RUNNING.load(Ordering::Relaxed) {
                    eprintln!(
                        "Removing {}/{}: {}",
                        control.done.load(Ordering::Relaxed) + 1,
                        total,
                        control.current().display()
                    );
                }
            }
        })
    };
    let result = cleanup::execute(snapshot, report, &selected, mode, &guard, &control);
    RUNNING.store(false, Ordering::Relaxed);
    progress.thread().unpark();
    let _ = progress.join();
    if options.json {
        serde_json::to_writer(&mut *writer, &result)?;
        writeln!(writer)?;
    } else {
        let verb = match mode {
            CleanupMode::Trash => format!(
                "Moved {} folders to the {}",
                result.removed,
                CleanupMode::trash_name()
            ),
            CleanupMode::Permanent => format!("Deleted {} folders", result.removed),
        };
        writeln!(
            writer,
            "{verb}; frees at least {}{}. {} failed, {} skipped{}.",
            human(result.freed_bytes),
            if mode == CleanupMode::Trash {
                " once it is emptied"
            } else {
                ""
            },
            result.failed,
            result.refused.len()
                + result
                    .items
                    .iter()
                    .filter(|r| matches!(r.outcome, Outcome::Refused { .. }))
                    .count(),
            if result.cancelled { ", cancelled" } else { "" }
        )?;
        writeln!(writer, "\n{:<9} {:>10}  PATH", "STATUS", "FREES")?;
        for r in &result.items {
            let (status, frees, note) = match &r.outcome {
                Outcome::Removed { freed_bytes } => ("removed", human(*freed_bytes), String::new()),
                Outcome::Refused { reason } => ("skipped", "-".into(), format!("  ({reason})")),
                Outcome::Failed {
                    error,
                    maybe_partial,
                } => (
                    "failed",
                    "-".into(),
                    format!(
                        "  ({error}{})",
                        if *maybe_partial {
                            "; some content may be gone, rescan to check"
                        } else {
                            ""
                        }
                    ),
                ),
                Outcome::NotRun => ("not run", "-".into(), "  (cancelled)".into()),
            };
            writeln!(
                writer,
                "{status:<9} {frees:>10}  {}{note}",
                r.item.path.display()
            )?;
        }
        for refusal in &result.refused {
            writeln!(
                writer,
                "{:<9} {:>10}  {}  ({})",
                "skipped",
                "-",
                refusal.path.display(),
                refusal.reason
            )?;
        }
        for follow_up in &result.follow_ups {
            writeln!(writer, "\n{follow_up}")?;
        }
    }
    Ok(if result.cancelled {
        130
    } else if result.failed > 0
        || !result.refused.is_empty()
        || result
            .items
            .iter()
            .any(|r| matches!(r.outcome, Outcome::Refused { .. }))
    {
        3
    } else {
        0
    })
}
