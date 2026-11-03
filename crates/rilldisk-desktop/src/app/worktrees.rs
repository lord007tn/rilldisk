// SPDX-License-Identifier: AGPL-3.0-only
//! Git checks of worktree candidates (`rilldisk_core::git`), run in the
//! background after each scan. Git states are kept as they come back; verdicts
//! are computed when drawn, so changing the idle threshold applies at once.
use super::{Data, RillApp, now_ms};
use crate::style::{self, t};
use eframe::egui::{self, Color32};
use rilldisk_core::{
    Candidate, ReclaimReport,
    git::{GitReport, GitState, Verdict},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

/// Git processes running at once.
const WORKERS: usize = 4;

type Message = (usize, usize, Result<GitState, String>);

#[derive(Default)]
pub struct GitChecks {
    /// The results the states belong to (an `Arc<Data>` address).
    key: usize,
    states: HashMap<usize, Result<GitState, String>>,
    pending: usize,
    rx: Option<mpsc::Receiver<Message>>,
}

/// What the interface knows about one worktree.
pub enum GitView {
    Pending,
    Ready(Box<GitReport>),
}

impl RillApp {
    /// Start checks for new results and collect finished ones.
    pub(super) fn poll_git(&mut self, ctx: &egui::Context) {
        if !self.prefs.git_checks {
            return;
        }
        let Some(data) = self.data.clone() else {
            return;
        };
        let key = Arc::as_ptr(&data) as usize;
        if self.git.key != key {
            self.start_git(&data, key, ctx);
        }
        if let Some(rx) = &self.git.rx {
            while let Ok((from, id, state)) = rx.try_recv() {
                if from == self.git.key {
                    self.git.states.insert(id, state);
                    self.git.pending = self.git.pending.saturating_sub(1);
                }
            }
            if self.git.pending == 0 {
                self.git.rx = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
    }

    fn start_git(&mut self, data: &Arc<Data>, key: usize, ctx: &egui::Context) {
        let jobs: Vec<(usize, PathBuf)> = data
            .reclaim
            .candidates
            .iter()
            .filter(|c| c.is_linked_worktree())
            .map(|c| (c.entry_id, c.path.clone()))
            .collect();
        self.git = GitChecks {
            key,
            pending: jobs.len(),
            ..GitChecks::default()
        };
        if jobs.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.git.rx = Some(rx);
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name("rilldisk-git".into())
            .spawn(move || {
                let next = AtomicUsize::new(0);
                std::thread::scope(|scope| {
                    for _ in 0..WORKERS.min(jobs.len()) {
                        let (jobs, next, tx, repaint) = (&jobs, &next, tx.clone(), repaint.clone());
                        scope.spawn(move || {
                            while let Some((id, path)) =
                                jobs.get(next.fetch_add(1, Ordering::Relaxed))
                            {
                                let state = rilldisk_core::git::inspect(path);
                                if tx.send((key, *id, state)).is_err() {
                                    return;
                                }
                                repaint.request_repaint();
                            }
                        });
                    }
                });
            })
            .expect("spawn git thread");
    }

    /// Git's view of a candidate, or `None` for anything that is not a linked
    /// worktree or when checks are off.
    pub(crate) fn git_view(&self, c: &Candidate) -> Option<GitView> {
        if !self.prefs.git_checks || !c.is_linked_worktree() {
            return None;
        }
        match self.git.states.get(&c.entry_id) {
            Some(state) => Some(GitView::Ready(Box::new(GitReport::new(
                state.clone(),
                &c.path,
                c.newest_modified_unix_ms,
                now_ms(),
                self.prefs.stale_days,
            )))),
            None if self.git.pending > 0 => Some(GitView::Pending),
            None => None,
        }
    }

    /// Git has answered for every worktree of the shown results.
    pub(crate) fn git_settled(&self) -> bool {
        self.git.pending == 0
    }

    pub(crate) fn stale_days(&self) -> u64 {
        self.prefs.stale_days
    }

    /// Outermost worktrees Git found clean, with nothing only there, and idle.
    pub(crate) fn clean_idle_worktrees(&self, data: &Data) -> Vec<usize> {
        data.reclaim
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.nested_in.is_none() && super::listed(c))
            .filter(|(_, c)| {
                matches!(self.git_view(c), Some(GitView::Ready(r)) if r.verdict == Verdict::CleanIdle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The report a removal runs on. With Git checks on, every linked worktree
    /// carries what was shown (or "not checked yet"), so cleanup asks Git again
    /// right before removing it and skips it unless Git reports no changes.
    /// With checks off, worktrees take the explicit unchecked path.
    pub(crate) fn annotated(&self, report: &ReclaimReport) -> ReclaimReport {
        let mut report = report.clone();
        if !self.prefs.git_checks {
            return report;
        }
        for c in report
            .candidates
            .iter_mut()
            .filter(|c| c.is_linked_worktree())
        {
            c.git = Some(match self.git_view(c) {
                Some(GitView::Ready(r)) => *r,
                _ => GitReport::new(
                    Err("not checked yet".into()),
                    &c.path,
                    c.newest_modified_unix_ms,
                    now_ms(),
                    self.prefs.stale_days,
                ),
            });
        }
        report
    }

    /// What the removal dialog says about ticked worktrees.
    pub(crate) fn pick_notes(&self, data: &Data, picked: &[usize]) -> PickNotes {
        let mut notes = PickNotes {
            checks_off: !self.prefs.git_checks,
            ..PickNotes::default()
        };
        for c in picked
            .iter()
            .filter_map(|&i| data.reclaim.candidates.get(i))
            .filter(|c| c.is_linked_worktree())
        {
            notes.worktrees += 1;
            let name = c.path.file_name().map_or_else(
                || c.path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            match self.git_view(c) {
                Some(GitView::Ready(r)) if r.verdict == Verdict::Unknown => {
                    notes.unchecked.push(name)
                }
                Some(GitView::Ready(r)) if !r.verdict.no_reported_loss() => {
                    notes.risky.push(format!("{name}: {}", r.summary))
                }
                Some(GitView::Ready(_)) => {}
                _ if self.prefs.git_checks => notes.unchecked.push(name),
                _ => {}
            }
        }
        notes
    }
}

/// Git notes for the removal dialog.
#[derive(Default)]
pub struct PickNotes {
    /// Ticked linked worktrees.
    pub worktrees: usize,
    /// Git checks are off: worktrees are removed without asking Git.
    pub checks_off: bool,
    /// Git reports work that would be lost (they will be skipped).
    pub risky: Vec<String>,
    /// Not answered yet or unknown (asked again at removal, skipped unless clean).
    pub unchecked: Vec<String>,
}

/// Short chip text and color for a card.
pub fn chip(view: &GitView) -> (String, Color32) {
    let th = t();
    let GitView::Ready(report) = view else {
        return ("Checking Git…".into(), th.faint);
    };
    let state = report.state.as_ref();
    match report.verdict {
        Verdict::UncommittedChanges => {
            let n = state.map_or(0, |s| s.changed_files + s.untracked_items);
            (format!("{} uncommitted", style::count(n)), th.danger)
        }
        Verdict::CommitsOnlyHere => {
            let n = state.map_or(0, |s| s.commits_only_here);
            let text = if n == 1 {
                "1 commit only here".to_string()
            } else {
                format!("{} commits only here", style::count(n))
            };
            (text, th.danger)
        }
        Verdict::Locked => ("Locked".into(), th.info),
        Verdict::Active => ("No Git changes · active".into(), th.dim),
        Verdict::CleanIdle => ("No Git changes · idle".into(), th.good),
        Verdict::Unknown => ("Git unknown".into(), th.faint),
    }
}
