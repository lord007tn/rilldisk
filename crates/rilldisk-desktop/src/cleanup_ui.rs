// SPDX-License-Identifier: AGPL-3.0-only
//! Removing selected reclaim candidates from the Clean up view: "Select all
//! safe", the selection bar's remove buttons, the confirmation dialog, progress
//! and the result summary. The work runs in `rilldisk_core::cleanup`; see
//! docs/proposals/cleanup.md for the scope and safety rules.
use crate::app::{Data, RillApp, frees};
use crate::icons::{self, Icon};
use crate::style::{self, t};
use crate::widgets::{self as w, Kind};
use eframe::egui::{
    self, Align, Align2, Color32, Frame, Layout, Margin, RichText, Sense, Ui, pos2, vec2,
};
use rilldisk_core::{
    Category, CleanupMode, CleanupPlan, CleanupReport, ReclaimReport, Safety,
    cleanup::{self, CleanupControl, Guard, Outcome},
};
use std::{
    sync::{Arc, OnceLock, atomic::Ordering, mpsc},
    time::Instant,
};

/// Cleanup state kept on the app.
#[derive(Default)]
pub struct CleanupUi {
    confirm: Option<Confirm>,
    job: Option<Job>,
    result: Option<Finished>,
    /// "Select all safe" candidates (indices) for the data they were computed on.
    safe: Option<(usize, Vec<usize>)>,
}

struct Confirm {
    data: Arc<Data>,
    /// The report removal runs on: the scan's, with Git expectations attached
    /// to worktrees when Git checks are on.
    reclaim: Arc<ReclaimReport>,
    git: crate::app::PickNotes,
    ids: Vec<usize>,
    plan: CleanupPlan,
    /// Permanent deletion needs an extra tick.
    understood: bool,
    opened: Instant,
}

struct Job {
    data: Arc<Data>,
    mode: CleanupMode,
    control: Arc<CleanupControl>,
    rx: mpsc::Receiver<(CleanupReport, Option<Data>)>,
}

struct Finished {
    report: CleanupReport,
    /// The scan the cleanup started from, to state freed space in the same
    /// metric (Size or On disk) the dialog showed.
    data: Arc<Data>,
    /// The in-memory scan was adjusted (removed folders subtracted).
    adjusted: bool,
}

/// "1 folder is", "3 folders are".
fn are(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun} is")
    } else {
        format!("{} {noun}s are", style::count(n as u64))
    }
}

fn guard() -> &'static Guard {
    static GUARD: OnceLock<Guard> = OnceLock::new();
    GUARD.get_or_init(Guard::system)
}

impl CleanupUi {
    /// A dialog is open or a cleanup is running: keyboard shortcuts stay off.
    pub fn modal_open(&self) -> bool {
        self.confirm.is_some() || self.job.is_some() || self.result.is_some()
    }

    /// Indices of candidates "Select all safe" ticks: outermost Regenerable and
    /// Cache folders that pass the static safety checks. Never Review items.
    fn safe(&mut self, data: &Arc<Data>) -> &[usize] {
        let key = Arc::as_ptr(data) as usize;
        if self.safe.as_ref().is_none_or(|(k, _)| *k != key) {
            let ids = cleanup::select_safe(&data.snapshot, &data.reclaim, guard());
            let indices = ids
                .into_iter()
                .filter_map(|id| data.candidate_of.get(&id).copied())
                .collect();
            self.safe = Some((key, indices));
        }
        &self.safe.as_ref().expect("just computed").1
    }
}

/// Explanation line with "Select all safe" and "Clear" above the list.
pub fn toolbar(app: &mut RillApp, ui: &mut Ui, data: &Arc<Data>) {
    let th = t();
    let category = app.category_filter;
    let report = &data.reclaim;
    let safe: Vec<usize> = app
        .cleanup
        .safe(data)
        .iter()
        .copied()
        .filter(|&i| category.is_none_or(|c| report.candidates[i].category == c))
        .collect();
    // Only when worktrees are shown: never tick what a filter hides.
    let idle: Vec<usize> = if category.is_none_or(|c| c == Category::GitWorktree)
        && app.safety_filter.is_none_or(|s| s == Safety::Review)
    {
        app.clean_idle_worktrees(data)
    } else {
        Vec::new()
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let buttons = if idle.is_empty() { 300.0 } else { 560.0 };
        let width = (ui.available_width() - buttons).max(200.0);
        ui.allocate_ui_with_layout(
            vec2(width, 32.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Tick folders to move them to the {}. Nothing is removed until you confirm, and Review-first items are never ticked for you.",
                            CleanupMode::trash_name()
                        ))
                        .font(style::regular(12.0))
                        .color(th.faint),
                    )
                    .wrap(),
                );
            },
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !app.picked.is_empty() && w::button(ui, Kind::Ghost, None, "Clear").clicked() {
                app.picked.clear();
            }
            let all = !safe.is_empty() && safe.iter().all(|i| app.picked.contains(i));
            ui.add_enabled_ui(!safe.is_empty() && !all, |ui| {
                if w::button(
                    ui,
                    Kind::Soft,
                    Some(Icon::Check),
                    &format!("Select all safe · {}", safe.len()),
                )
                .on_hover_text(
                    "Ticks every Rebuildable and Cache folder shown. Review-first items, system folders and Trash folders are never included.",
                )
                .on_disabled_hover_text(if safe.is_empty() {
                    "No Rebuildable or Cache folder here can be removed safely."
                } else {
                    "Every safe folder shown is already ticked."
                })
                .clicked()
                {
                    app.picked.extend(safe.iter().copied());
                }
            });
            // Worktrees with no Git-reported changes and no recent activity.
            if !idle.is_empty() {
                let all = idle.iter().all(|i| app.picked.contains(i));
                ui.add_enabled_ui(!all, |ui| {
                    if w::button(
                        ui,
                        Kind::Soft,
                        Some(Icon::Agent),
                        &format!("Select clean idle worktrees · {}", idle.len()),
                    )
                    .on_hover_text(format!(
                        "Ticks worktrees where Git reports no uncommitted or untracked changes, no commits that exist only there and no lock, and nothing changed for {} days or more (Settings). Git does not see ignored files such as .env, and RillDisk cannot tell whether an agent is still running there: they stay Review first. Branch commits stay in the repository.",
                        app.stale_days()
                    ))
                    .on_disabled_hover_text("Every clean idle worktree is already ticked.")
                    .clicked()
                    {
                        app.picked.extend(idle.iter().copied());
                    }
                });
            }
        });
    });
}

/// Buttons added to the selection bar (right to left): the remove button and
/// a menu with permanent deletion. Shift-click also deletes permanently.
pub fn bar_buttons(app: &mut RillApp, bar: &mut Ui, data: &Arc<Data>) {
    if app.elevate.removal_blocked() {
        bar.add_enabled_ui(false, |ui| {
            w::button(ui, Kind::Primary, Some(Icon::Trash), "Removal off as administrator")
                .on_disabled_hover_text(
                    "RillDisk is running as administrator, where it only lists. Copy the plan, or restart RillDisk normally to remove folders.",
                );
        });
        return;
    }
    let busy = app.cleanup.job.is_some();
    let menu = w::icon_button(bar, Icon::Chevron, "More removal options");
    egui::Popup::menu(&menu).show(|ui| {
        ui.set_min_width(220.0);
        if ui
            .button(RichText::new("Delete permanently…").color(t().danger))
            .on_hover_text("Skips the Recycle Bin / Trash. Cannot be undone.")
            .clicked()
        {
            open_confirm(app, data, CleanupMode::Permanent);
        }
        if ui
            .button(format!("Move to the {}…", CleanupMode::trash_name()))
            .clicked()
        {
            open_confirm(app, data, CleanupMode::Trash);
        }
    });
    let label = format!("Move to {}", CleanupMode::trash_name());
    let response = bar.add_enabled_ui(!busy, |ui| {
        w::button(ui, Kind::Primary, Some(Icon::Trash), &label).on_hover_text(format!(
            "Review the list, then move the ticked folders to the {}. Shift-click to delete permanently.",
            CleanupMode::trash_name()
        ))
    });
    if response.inner.clicked() {
        let shift = bar.input(|i| i.modifiers.shift);
        open_confirm(
            app,
            data,
            if shift {
                CleanupMode::Permanent
            } else {
                CleanupMode::Trash
            },
        );
    }
}

fn open_confirm(app: &mut RillApp, data: &Arc<Data>, mode: CleanupMode) {
    if app.elevate.removal_blocked() {
        return;
    }
    let mut ids: Vec<usize> = app
        .picked
        .iter()
        .filter_map(|&i| data.reclaim.candidates.get(i).map(|c| c.entry_id))
        .collect();
    ids.sort_unstable();
    let reclaim = Arc::new(app.annotated(&data.reclaim));
    let picked: Vec<usize> = app.picked.iter().copied().collect();
    let git = app.pick_notes(data, &picked);
    let plan = cleanup::plan(&data.snapshot, &reclaim, &ids, mode, guard());
    app.cleanup.confirm = Some(Confirm {
        data: data.clone(),
        reclaim,
        git,
        ids,
        plan,
        understood: false,
        opened: Instant::now(),
    });
}

/// Poll the running cleanup and draw any open dialog. Call once per frame.
pub fn show(app: &mut RillApp, ctx: &egui::Context) {
    poll(app, ctx);
    if app.cleanup.confirm.is_some() {
        confirm_dialog(app, ctx);
    } else if app.cleanup.job.is_some() {
        progress_dialog(app, ctx);
    } else if app.cleanup.result.is_some() {
        result_dialog(app, ctx);
    }
}

fn poll(app: &mut RillApp, ctx: &egui::Context) {
    let Some(job) = &app.cleanup.job else { return };
    match job.rx.try_recv() {
        Ok((report, adjusted)) => {
            let job = app.cleanup.job.take().expect("job is running");
            // Install the adjusted scan only if the view still shows the scan
            // the cleanup started from (a rescan may have replaced it).
            let current = app.data.as_ref().is_some_and(|d| Arc::ptr_eq(d, &job.data));
            let adjusted = match adjusted {
                Some(data) if current => {
                    app.install(data);
                    true
                }
                _ => false,
            };
            app.cleanup.result = Some(Finished {
                report,
                data: job.data,
                adjusted,
            });
        }
        Err(mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            let job = app.cleanup.job.take().expect("job is running");
            app.cleanup.result = Some(Finished {
                report: CleanupReport {
                    mode: job.mode,
                    items: Vec::new(),
                    refused: Vec::new(),
                    removed: 0,
                    failed: 1,
                    freed_bytes: 0,
                    cancelled: false,
                    follow_ups: vec![
                        "The cleanup worker stopped unexpectedly. Rescan to see what changed."
                            .into(),
                    ],
                },
                data: job.data,
                adjusted: false,
            });
        }
    }
}

fn start(app: &mut RillApp, ctx: &egui::Context, confirm: Confirm) {
    let control = Arc::new(CleanupControl::default());
    control
        .total
        .store(confirm.plan.items.len(), Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    let (data, reclaim, ids, mode) = (
        confirm.data.clone(),
        confirm.reclaim.clone(),
        confirm.ids,
        confirm.plan.mode,
    );
    let (thread_data, thread_control, repaint) = (data.clone(), control.clone(), ctx.clone());
    std::thread::Builder::new()
        .name("rilldisk-cleanup".into())
        .spawn(move || {
            let report = cleanup::execute(
                &thread_data.snapshot,
                &reclaim,
                &ids,
                mode,
                guard(),
                &thread_control,
            );
            let removed = report.removed_entry_ids();
            // Subtract removed folders so every view is right without a rescan.
            let adjusted = (!removed.is_empty())
                .then(|| Data::analyze(thread_data.snapshot.without(&removed)));
            let _ = tx.send((report, adjusted));
            repaint.request_repaint();
        })
        .expect("spawn cleanup thread");
    app.cleanup.job = Some(Job {
        data,
        mode,
        control,
        rx,
    });
}

fn modal(ctx: &egui::Context, id: &str, width: f32, content: impl FnOnce(&mut Ui)) -> bool {
    let th = t();
    let response = egui::Modal::new(egui::Id::unique(id))
        .backdrop_color(style::alpha(Color32::BLACK, if th.dark { 150 } else { 90 }))
        .frame(
            Frame::new()
                .fill(th.popup)
                .corner_radius(18)
                .inner_margin(Margin::same(24))
                .shadow(ctx.global_style().visuals.popup_shadow),
        )
        .show(ctx, |ui| {
            ui.set_width(width.min(ctx.content_rect().width() - 80.0));
            content(ui);
        });
    response.should_close()
}

fn title(ui: &mut Ui, icon: Icon, color: Color32, text: &str, sub: &str) {
    let th = t();
    ui.horizontal(|ui| {
        let (tile, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
        ui.painter()
            .rect_filled(tile, 12, style::alpha(color, if th.dark { 46 } else { 34 }));
        icons::paint(ui.painter(), tile.shrink(11.0), icon, color);
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(text)
                    .font(style::semibold(17.0))
                    .color(th.text),
            );
            ui.label(RichText::new(sub).font(style::regular(12.5)).color(th.dim));
        });
    });
}

fn note(ui: &mut Ui, color: Color32, icon: Icon, text: &str) {
    let th = t();
    Frame::new()
        .fill(style::alpha(color, if th.dark { 30 } else { 24 }))
        .corner_radius(10)
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                icons::paint(ui.painter(), r, icon, color);
                ui.add(
                    egui::Label::new(
                        RichText::new(text)
                            .font(style::regular(12.5))
                            .color(th.text),
                    )
                    .wrap(),
                );
            });
        });
}

/// One folder row: safety dot, label, size, then the full path.
fn item_row(
    ui: &mut Ui,
    safety: Safety,
    label: &str,
    size: Option<u64>,
    path: &str,
    detail: Option<(&str, Color32)>,
) {
    let th = t();
    let width = ui.available_width();
    let height = if detail.is_some() { 56.0 } else { 40.0 };
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let p = ui.painter();
    p.circle_filled(
        pos2(rect.left() + 6.0, rect.top() + 10.0),
        4.0,
        style::safety_color(safety),
    );
    let text_w = width - 110.0;
    let galley = w::fit(p, label.to_string(), style::semibold(13.0), th.text, text_w);
    p.galley(pos2(rect.left() + 18.0, rect.top() + 2.0), galley, th.text);
    if let Some(size) = size {
        p.text(
            pos2(rect.right(), rect.top() + 2.0),
            Align2::RIGHT_TOP,
            style::bytes(size),
            style::semibold(13.0),
            th.text,
        );
    }
    let galley = w::fit(
        p,
        path.to_string(),
        style::mono(11.0),
        th.faint,
        width - 18.0,
    );
    p.galley(
        pos2(rect.left() + 18.0, rect.top() + 21.0),
        galley,
        th.faint,
    );
    if let Some((text, color)) = detail {
        let galley = w::fit(
            p,
            text.to_string(),
            style::regular(11.5),
            color,
            width - 18.0,
        );
        p.galley(pos2(rect.left() + 18.0, rect.top() + 37.0), galley, color);
    }
}

/// A button painted in the danger color, for permanent deletion.
fn danger_button(ui: &mut Ui, text: &str) -> egui::Response {
    let th = t();
    let font = style::semibold(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER);
    let size = vec2(galley.size().x + 28.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    let fill = if !enabled {
        style::alpha(th.danger, 90)
    } else if response.is_pointer_button_down_on() {
        style::scale(th.danger, 0.9)
    } else if response.hovered() {
        style::mix(th.danger, Color32::WHITE, 0.12)
    } else {
        th.danger
    };
    ui.painter().rect_filled(rect, 9, fill);
    ui.painter().galley(
        pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0),
        galley,
        Color32::WHITE,
    );
    response
}

fn confirm_dialog(app: &mut RillApp, ctx: &egui::Context) {
    let th = t();
    let Some(confirm) = &mut app.cleanup.confirm else {
        return;
    };
    let plan = &confirm.plan;
    let data = &confirm.data;
    let permanent = plan.mode == CleanupMode::Permanent;
    let frees_of = |entry_id: usize| {
        data.candidate_of
            .get(&entry_id)
            .map_or(0, |&i| frees(&data.reclaim.candidates[i]))
    };
    let known = crate::app::allocation_known(
        plan.items
            .iter()
            .filter_map(|i| data.candidate_of.get(&i.entry_id))
            .map(|&i| &data.reclaim.candidates[i]),
    );
    let floor = crate::app::floor_word(known);
    let total: u64 = plan.items.iter().map(|i| frees_of(i.entry_id)).sum();
    let reviews = plan
        .items
        .iter()
        .filter(|i| i.safety == Safety::Review)
        .count();
    let worktrees = plan
        .items
        .iter()
        .filter(|i| i.category == Category::GitWorktree)
        .count();
    let mut cancel = false;
    let mut go = false;
    let n = plan.items.len() as u64;
    let close = modal(ctx, "cleanup-confirm", 620.0, |ui| {
        let heading = if permanent {
            format!("Permanently delete {}?", style::plural(n, "folder"))
        } else {
            format!(
                "Move {} to the {}?",
                style::plural(n, "folder"),
                CleanupMode::trash_name()
            )
        };
        let sub = if plan.items.is_empty() {
            "None of the ticked folders can be removed.".to_string()
        } else if permanent {
            format!(
                "Frees {floor} {} on disk · {}. This cannot be undone.",
                style::bytes(total),
                style::plural(plan.total_files, "file")
            )
        } else {
            format!(
                "Frees {floor} {} on disk once the {} is emptied · {}. You can restore them from there.",
                style::bytes(total),
                CleanupMode::trash_name(),
                style::plural(plan.total_files, "file")
            )
        };
        title(
            ui,
            Icon::Trash,
            if permanent { th.danger } else { th.accent },
            &heading,
            &sub,
        );
        ui.add_space(14.0);
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for item in &plan.items {
                    item_row(
                        ui,
                        item.safety,
                        &format!("{} · {}", item.label, item.safety.label()),
                        Some(frees_of(item.entry_id)),
                        &item.path.display().to_string(),
                        None,
                    );
                }
                for refusal in &plan.refused {
                    let safety = data
                        .candidate_of
                        .get(&refusal.entry_id)
                        .map_or(Safety::Review, |&i| data.reclaim.candidates[i].safety);
                    item_row(
                        ui,
                        safety,
                        "Will be skipped",
                        None,
                        &refusal.path.display().to_string(),
                        Some((&format!("This folder {}.", refusal.reason), th.warn)),
                    );
                }
            });
        if plan.covered > 0 {
            ui.label(
                RichText::new(format!(
                    "{} ticked inside another ticked folder and removed with it.",
                    are(plan.covered, "folder"),
                ))
                .font(style::regular(12.0))
                .color(th.faint),
            );
        }
        ui.add_space(10.0);
        let git = &confirm.git;
        if !git.risky.is_empty() {
            note(
                ui,
                th.danger,
                Icon::Warning,
                &format!(
                    "Git reports changes in {}, so {} skipped: RillDisk asks Git again right before removing a worktree. {}",
                    style::plural(git.risky.len() as u64, "ticked worktree"),
                    if git.risky.len() == 1 {
                        "it will be"
                    } else {
                        "they will be"
                    },
                    git.risky.join(" ")
                ),
            );
            ui.add_space(6.0);
        }
        if !git.unchecked.is_empty() {
            note(
                ui,
                th.warn,
                Icon::Warning,
                &format!(
                    "Not checked by Git yet: {}. Git is asked right before removal; any worktree it reports changes for, or cannot check, is skipped.",
                    git.unchecked.join(", ")
                ),
            );
            ui.add_space(6.0);
        }
        if git.worktrees > 0 && git.checks_off {
            note(
                ui,
                th.warn,
                Icon::Warning,
                "Git checks are off (Settings), so these worktrees are removed without asking Git. Uncommitted work in them goes to the Trash with the folder.",
            );
            ui.add_space(6.0);
        } else if git.worktrees > 0 {
            note(
                ui,
                th.info,
                Icon::Eye,
                "Git does not see ignored files (such as .env or local settings), and RillDisk cannot tell whether an agent is still running in a worktree.",
            );
            ui.add_space(6.0);
        }
        if reviews > 0 {
            note(
                ui,
                th.warn,
                Icon::Warning,
                &format!(
                    "{} marked Review first: it may hold unique work such as uncommitted changes. Check before you continue.",
                    are(reviews, "folder"),
                ),
            );
            ui.add_space(6.0);
        }
        if worktrees > 0 {
            note(
                ui,
                th.info,
                Icon::Terminal,
                "Git still lists removed worktrees. Afterwards, run `git worktree prune` in each original repository. RillDisk only reads Git; it never changes a repository.",
            );
            ui.add_space(6.0);
        }
        if permanent {
            note(
                ui,
                th.danger,
                Icon::Warning,
                &format!(
                    "Permanent deletion skips the {}. The files cannot be restored.",
                    CleanupMode::trash_name()
                ),
            );
            ui.add_space(6.0);
            ui.checkbox(
                &mut confirm.understood,
                RichText::new("I understand these folders cannot be restored")
                    .font(style::regular(12.5)),
            );
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // A short delay keeps a double click from confirming by accident.
            let ready = !plan.items.is_empty()
                && (!permanent || confirm.understood)
                && confirm.opened.elapsed().as_millis() > 400;
            ui.add_enabled_ui(ready, |ui| {
                let response = if permanent {
                    danger_button(
                        ui,
                        &format!("Delete {} permanently", style::plural(n, "folder")),
                    )
                } else {
                    w::button(
                        ui,
                        Kind::Primary,
                        Some(Icon::Trash),
                        &format!("Move to {}", CleanupMode::trash_name()),
                    )
                };
                if response.clicked() {
                    go = true;
                }
            });
            let cancel_button = w::button(ui, Kind::Soft, None, "Cancel");
            // Cancel is the default: Enter or Space activates it.
            if !cancel_button.has_focus() && ui.memory(|m| m.focused().is_none()) {
                cancel_button.request_focus();
            }
            if cancel_button.clicked() {
                cancel = true;
            }
        });
    });
    if close || cancel {
        app.cleanup.confirm = None;
    } else if go {
        let confirm = app.cleanup.confirm.take().expect("dialog is open");
        start(app, ctx, confirm);
    }
}

fn progress_dialog(app: &mut RillApp, ctx: &egui::Context) {
    let th = t();
    let Some(job) = &app.cleanup.job else { return };
    let control = &job.control;
    let done = control.done.load(Ordering::Relaxed);
    let total = control.total.load(Ordering::Relaxed).max(1);
    let freed = control.freed.load(Ordering::Relaxed);
    let stopping = control.is_cancelled();
    let verb = match job.mode {
        CleanupMode::Trash => format!("Moving to the {}…", CleanupMode::trash_name()),
        CleanupMode::Permanent => "Deleting permanently…".to_string(),
    };
    let mut stop = false;
    modal(ctx, "cleanup-progress", 560.0, |ui| {
        title(
            ui,
            Icon::Trash,
            th.accent,
            &verb,
            &format!(
                "{} of {} · {} freed so far",
                style::count(done.min(total) as u64),
                style::count(total as u64),
                style::bytes(freed)
            ),
        );
        ui.add_space(14.0);
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), Sense::hover());
        // Show the item in progress as half done so the bar moves on long items.
        w::progress(
            ui.painter(),
            bar,
            ((done as f32 + 0.5) / total as f32).min(1.0),
            th.accent,
        );
        ui.add_space(10.0);
        let current = control.current();
        let galley = w::fit(
            ui.painter(),
            current.display().to_string(),
            style::mono(11.0),
            th.faint,
            ui.available_width(),
        );
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
        ui.painter().galley(r.left_top(), galley, th.faint);
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_enabled_ui(!stopping, |ui| {
                if w::button(
                    ui,
                    Kind::Soft,
                    Some(Icon::Stop),
                    if stopping {
                        "Stopping after this folder…"
                    } else {
                        "Stop"
                    },
                )
                .on_hover_text("Stops after the folder in progress. A folder already being removed is finished.")
                .clicked()
                {
                    stop = true;
                }
            });
        });
    });
    if stop {
        control.cancel();
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(50));
}

fn result_dialog(app: &mut RillApp, ctx: &egui::Context) {
    let th = t();
    let allocated = app.allocated;
    let Some(finished) = &app.cleanup.result else {
        return;
    };
    let report = &finished.report;
    // The confirmation showed this metric; report the same one.
    let freed: u64 = report
        .items
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Removed { .. }))
        .filter_map(|r| finished.data.candidate_of.get(&r.item.entry_id))
        .map(|&i| frees(&finished.data.reclaim.candidates[i]))
        .sum();
    let known = crate::app::allocation_known(
        report
            .items
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Removed { .. }))
            .filter_map(|r| finished.data.candidate_of.get(&r.item.entry_id))
            .map(|&i| &finished.data.reclaim.candidates[i]),
    );
    let skipped = report.refused.len()
        + report
            .items
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Refused { .. }))
            .count();
    let not_run = report
        .items
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::NotRun))
        .count();
    let clean = report.failed == 0 && skipped == 0 && not_run == 0;
    let mut done = false;
    let mut rescan = false;
    let close = modal(ctx, "cleanup-result", 600.0, |ui| {
        let verb = match report.mode {
            CleanupMode::Trash => format!("moved to the {}", CleanupMode::trash_name()),
            CleanupMode::Permanent => "deleted".to_string(),
        };
        let mut parts = vec![format!(
            "{} {verb}",
            style::plural(report.removed as u64, "folder")
        )];
        if !allocated && report.removed > 0 {
            parts.push(format!("{} on disk", style::bytes(report.freed_bytes)));
        }
        if report.failed > 0 {
            parts.push(format!("{} failed", style::count(report.failed as u64)));
        }
        if skipped > 0 {
            parts.push(format!("{} skipped", style::count(skipped as u64)));
        }
        if not_run > 0 {
            parts.push(format!("{} not started", style::count(not_run as u64)));
        }
        title(
            ui,
            if clean { Icon::Check } else { Icon::Warning },
            if clean { th.good } else { th.warn },
            &format!(
                "Freed {} {} on disk",
                crate::app::floor_word(known),
                style::bytes(freed)
            ),
            &parts.join(" · "),
        );
        ui.add_space(12.0);
        let problems: Vec<(String, String, Color32)> = report
            .items
            .iter()
            .filter_map(|r| {
                let path = r.item.path.display().to_string();
                match &r.outcome {
                    Outcome::Failed {
                        error,
                        maybe_partial,
                    } => Some((
                        path,
                        format!(
                            "Failed: {error}.{}",
                            if *maybe_partial {
                                " Some content may already be gone."
                            } else {
                                ""
                            }
                        ),
                        th.danger,
                    )),
                    Outcome::Refused { reason } => {
                        Some((path, format!("Skipped: this folder {reason}."), th.warn))
                    }
                    Outcome::NotRun => Some((path, "Not started: stopped.".into(), th.faint)),
                    Outcome::Removed { .. } => None,
                }
            })
            .chain(report.refused.iter().map(|r| {
                (
                    r.path.display().to_string(),
                    format!("Skipped: this folder {}.", r.reason),
                    th.warn,
                )
            }))
            .collect();
        if !problems.is_empty() {
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    for (path, text, color) in &problems {
                        let label = std::path::Path::new(path)
                            .file_name()
                            .map_or(path.clone(), |n| n.to_string_lossy().into_owned());
                        item_row(ui, Safety::Review, &label, None, path, Some((text, *color)));
                    }
                });
            ui.add_space(8.0);
        }
        for follow_up in &report.follow_ups {
            note(ui, th.info, Icon::Terminal, follow_up);
            ui.add_space(6.0);
        }
        let needs_rescan = report.needs_rescan() || !finished.adjusted && report.removed > 0;
        ui.label(
            RichText::new(if needs_rescan {
                "Some sizes may be out of date. Rescan to see exactly what is left."
            } else if finished.adjusted {
                "Sizes were updated without a rescan. Rescan any time for exact figures."
            } else {
                "Nothing changed on disk."
            })
            .font(style::regular(12.0))
            .color(th.faint),
        );
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if w::button(ui, Kind::Primary, None, "Done").clicked() {
                done = true;
            }
            if w::button(
                ui,
                if needs_rescan {
                    Kind::Soft
                } else {
                    Kind::Ghost
                },
                Some(Icon::Refresh),
                "Rescan",
            )
            .clicked()
            {
                rescan = true;
            }
        });
    });
    if close || done || rescan {
        app.cleanup.result = None;
    }
    if rescan && let Some(data) = &app.data {
        app.path = data.snapshot.summary.root.display().to_string();
        app.start_scan(ctx);
    }
}

/// Hooks for scripted captures (`--capture-cleanup`, capture.rs).
pub mod tour {
    use super::*;

    pub fn select_safe(app: &mut RillApp) {
        if let Some(data) = app.data.clone() {
            let safe = app.cleanup.safe(&data).to_vec();
            app.picked.extend(safe);
        }
    }

    pub fn confirm(app: &mut RillApp, mode: CleanupMode) {
        if let Some(data) = app.data.clone() {
            open_confirm(app, &data, mode);
        }
    }

    /// Run the open confirmation, but only on a synthetic fixture (made by
    /// scripts/make_demo_fixture.py) or a scan inside the temp folder, so a tour
    /// never removes anything from a real location.
    pub fn run(app: &mut RillApp, ctx: &egui::Context) {
        let Some(mut confirm) = app.cleanup.confirm.take() else {
            return;
        };
        // Tours delete synthetic data outright so they never fill the Trash.
        confirm.plan.mode = CleanupMode::Permanent;
        let root = confirm.data.snapshot.summary.root.clone();
        let temp = std::env::temp_dir()
            .canonicalize()
            .map(rilldisk_core::simplify)
            .unwrap_or_default();
        let synthetic = root.join(".rilldisk-synthetic-fixture").is_file()
            || (!temp.as_os_str().is_empty() && root.starts_with(&temp));
        if synthetic {
            start(app, ctx, confirm);
        } else {
            eprintln!(
                "capture: not removing anything from a folder that is not a synthetic fixture"
            );
        }
    }

    pub fn dismiss(app: &mut RillApp) {
        app.cleanup.confirm = None;
        app.cleanup.result = None;
    }

    pub fn busy(app: &RillApp) -> bool {
        app.cleanup.job.is_some()
    }
}
