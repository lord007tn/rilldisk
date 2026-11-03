// SPDX-License-Identifier: AGPL-3.0-only
//! What a result covers: the entry-limit notice with a bounded rescan, and the
//! Git worktree inventory (counts, scope and totals) above the worktree list.
use super::{Action, Data, RillApp, frees, size_metric, worktrees::GitView};
use crate::icons::{self, Icon};
use crate::settings::SCAN_LIMITS;
use crate::style::{self, t};
use crate::widgets::{self as w, Kind};
use eframe::egui::{Color32, Frame, Margin, RichText, Sense, Ui, vec2};
use rilldisk_core::{Category, git::Verdict, memory_for};

/// Physical memory available now, where the platform says.
pub fn available_memory() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        // SAFETY: the struct is zeroed with its length set, as the API requires.
        unsafe {
            let mut status: MEMORYSTATUSEX = std::mem::zeroed();
            status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            (GlobalMemoryStatusEx(&mut status) != 0).then_some(status.ullAvailPhys)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// "60M", "142M".
pub fn short_limit(limit: usize) -> String {
    format!("{}M", limit.div_ceil(1_000_000))
}

impl RillApp {
    /// Shown when the scan stopped at its entry limit: nothing below is complete.
    pub(super) fn limit_banner(&self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let mut actions = Vec::new();
        if !data.snapshot.summary.entry_limit_reached {
            return actions;
        }
        let th = t();
        let current = self.prefs.scan_limit();
        let next = SCAN_LIMITS.iter().copied().find(|&l| l > current);
        Frame::new()
            .fill(style::alpha(th.warn, if th.dark { 30 } else { 26 }))
            .corner_radius(12)
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (icon, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                    icons::paint(ui.painter(), icon, Icon::Warning, th.warn);
                    ui.add_space(4.0);
                    let room = ui.available_width() - if next.is_some() { 250.0 } else { 0.0 };
                    ui.allocate_ui(vec2(room.max(200.0), 40.0), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Incomplete: the scan stopped at its limit of {} entries, so some folders were not read. Totals, worktree counts and cleanup candidates are missing what lies there.",
                                style::count(current as u64)
                            ))
                            .font(style::regular(12.5))
                            .color(th.text),
                        );
                    });
                    let Some(next) = next else {
                        return;
                    };
                    let need = memory_for(next);
                    let free = available_memory();
                    let enough = free.is_none_or(|f| f >= need);
                    let label = format!("Rescan with a {} limit", short_limit(next));
                    let button = ui
                        .add_enabled_ui(enough && self.running.is_none(), |ui| {
                            w::button(ui, Kind::Soft, Some(Icon::Refresh), &label)
                        })
                        .inner;
                    let memory = match free {
                        Some(f) => format!(
                            "Needs up to about {} of memory; {} is free now.",
                            style::bytes(need),
                            style::bytes(f)
                        ),
                        None => format!("Needs up to about {} of memory.", style::bytes(need)),
                    };
                    if button
                        .on_hover_text(format!("{memory} The new limit is kept for later scans (Settings)."))
                        .on_disabled_hover_text(format!(
                            "{memory} Close other programs, or scan a smaller folder."
                        ))
                        .clicked()
                    {
                        actions.push(Action::ScanLimit(Some(next), true));
                    }
                });
            });
        ui.add_space(10.0);
        actions
    }

    /// Counts, scope and totals of the Git worktrees in this result.
    pub(super) fn worktree_inventory(&self, ui: &mut Ui, data: &Data) {
        let th = t();
        let worktrees: Vec<_> = data
            .reclaim
            .candidates
            .iter()
            .filter(|c| c.category == Category::GitWorktree)
            .collect();
        if worktrees.is_empty() {
            return;
        }
        let linked = worktrees.iter().filter(|c| c.is_linked_worktree()).count() as u64;
        let orphaned = worktrees.len() as u64 - linked;
        let outer = worktrees.iter().filter(|c| c.nested_in.is_none());
        let size: u64 = outer.clone().map(|c| size_metric(c, self.allocated)).sum();
        let known = super::allocation_known(outer.clone().copied());
        let freed: u64 = outer.map(|c| frees(c)).sum();
        let mut agents: Vec<(&str, u64)> = Vec::new();
        for c in &worktrees {
            let name = c.agent.unwrap_or("Not attributed");
            match agents.iter_mut().find(|a| a.0 == name) {
                Some(a) => a.1 += 1,
                None => agents.push((name, 1)),
            }
        }
        agents.sort_by_key(|a| std::cmp::Reverse(a.1));
        let summary = &data.snapshot.summary;
        let mut title = style::plural(linked, "linked Git worktree");
        if orphaned > 0 {
            title.push_str(&format!(
                " · {}",
                style::plural(orphaned, "orphaned agent folder")
            ));
        }
        Frame::new()
            .fill(th.card)
            .corner_radius(14)
            .inner_margin(Margin::symmetric(18, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(RichText::new(title).font(style::semibold(16.0)));
                ui.label(
                    RichText::new(format!(
                        "Under {} · {} in size · removing every one would free {} {} on disk (hard links counted). Neither figure means they are safe to remove.",
                        summary.root.display(),
                        style::bytes(size),
                        super::floor_word(known),
                        style::bytes(freed)
                    ))
                    .font(style::regular(12.0))
                    .color(th.dim),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    for (name, n) in &agents {
                        w::chip(ui, &format!("{name} {}", style::count(*n)), th.dim);
                    }
                });
                if self.prefs.git_checks && linked > 0 {
                    let mut counts: Vec<(String, Color32, u64)> = Vec::new();
                    let mut add = |label: &str, color: Color32| {
                        match counts.iter_mut().find(|c| c.0 == label) {
                            Some(c) => c.2 += 1,
                            None => counts.push((label.to_string(), color, 1)),
                        }
                    };
                    for c in worktrees.iter().filter(|c| c.is_linked_worktree()) {
                        match self.git_view(c) {
                            Some(GitView::Ready(r)) => {
                                let color = match r.verdict {
                                    Verdict::CleanIdle => th.good,
                                    Verdict::UncommittedChanges | Verdict::CommitsOnlyHere => th.danger,
                                    Verdict::Locked => th.info,
                                    Verdict::Active | Verdict::Unknown => th.dim,
                                };
                                add(r.verdict.label(), color);
                            }
                            _ => add("Checking Git", th.faint),
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        ui.label(RichText::new("Git:").font(style::regular(12.0)).color(th.faint));
                        for (label, color, n) in &counts {
                            w::chip(ui, &format!("{label} {}", style::count(*n)), *color);
                        }
                    });
                }
                let (text, color) = if summary.entry_limit_reached {
                    (
                        "Incomplete: the scan stopped at its entry limit, so more worktrees may exist under this folder.".to_string(),
                        th.warn,
                    )
                } else if !summary.complete {
                    (
                        format!(
                            "Partial scan: {} (see Issues). Worktrees inside unread folders are not counted.",
                            style::plural(summary.issue_count, "issue")
                        ),
                        th.warn,
                    )
                } else {
                    (
                        "Complete for this folder. Worktrees outside it are not included.".to_string(),
                        th.faint,
                    )
                };
                ui.label(RichText::new(text).font(style::regular(12.0)).color(color));
            });
        ui.add_space(8.0);
    }
}
