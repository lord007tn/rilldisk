// SPDX-License-Identifier: AGPL-3.0-only
//! Updates in the interface: a quiet version button that checks on click, and a
//! card that appears only when there is something to say (a newer release, a
//! running install, a failure or a finished update).
use super::{Action, RillApp};
use crate::icons::{self, Icon};
use crate::live;
use crate::style::{self, t};
use crate::update;
use crate::widgets::{self as w, Kind};
use eframe::egui::{
    self, Align, Color32, CursorIcon, Frame, Layout, Margin, Rect, RichText, Sense, Ui, pos2, vec2,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

impl RillApp {
    /// Whether [`Self::update_card`] has anything to show.
    pub(super) fn update_pending(&self) -> bool {
        self.installing.is_some()
            || self.update_error.is_some()
            || self.available.is_some()
            || self.update_notice.is_some()
    }

    /// The update card. `wide` lays it out in a row (home and settings); otherwise
    /// it stacks for the sidebar.
    pub(super) fn update_card(&self, ui: &mut Ui, wide: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        if !self.update_pending() {
            return actions;
        }
        let th = t();
        let (tint, icon) = if self.installing.is_some() {
            (th.accent, Icon::Download)
        } else if self.update_error.is_some() {
            (th.danger, Icon::Warning)
        } else if self.available.is_some() {
            (th.accent, Icon::Sparkle)
        } else {
            (th.good, Icon::Check)
        };
        Frame::new()
            .fill(style::alpha(tint, if th.dark { 30 } else { 22 }))
            .corner_radius(12)
            .inner_margin(Margin::same(if wide { 16 } else { 12 }))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 4.0;
                let (title, detail) = self.card_text();
                let body = |ui: &mut Ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                        ui.painter().rect_filled(
                            rect,
                            7,
                            style::alpha(tint, if th.dark { 56 } else { 40 }),
                        );
                        icons::paint(ui.painter(), rect.shrink(5.0), icon, tint);
                        ui.label(
                            RichText::new(&title)
                                .font(style::semibold(13.0))
                                .color(th.text),
                        );
                    });
                    if !detail.is_empty() {
                        let detail = RichText::new(&detail)
                            .font(style::regular(12.0))
                            .color(th.dim);
                        let label = ui.add(egui::Label::new(detail).wrap());
                        if let Some(error) = &self.update_error {
                            label.on_hover_text(error);
                        }
                    }
                };
                if wide {
                    ui.horizontal(|ui| {
                        let room = (ui.available_width() - 300.0).max(220.0);
                        ui.vertical(|ui| {
                            ui.set_width(room);
                            body(ui);
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            self.card_buttons(ui, &mut actions);
                        });
                    });
                } else {
                    body(ui);
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        self.card_buttons(ui, &mut actions);
                    });
                }
                if let Some(install) = &self.installing {
                    ui.add_space(6.0);
                    let (bar, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                    match install.progress() {
                        Some(progress) => w::progress(ui.painter(), bar, progress, th.accent),
                        None => live::sweep(ui.painter(), bar, ui.input(|i| i.time), th.accent),
                    }
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                }
            });
        actions
    }

    /// Title and one line of detail for the current update state.
    fn card_text(&self) -> (String, String) {
        if let Some(install) = &self.installing {
            let version = self.available.as_ref().map_or("", |r| r.version.as_str());
            let (received, total) = install.bytes();
            let detail = match install.progress() {
                Some(progress) => format!(
                    "{} · {:.0}% · {} of {}",
                    install.phase().label(),
                    progress * 100.0,
                    style::bytes(received),
                    style::bytes(total)
                ),
                None if received > 0 && install.phase() == update::Phase::Downloading => format!(
                    "{} · {} received",
                    install.phase().label(),
                    style::bytes(received)
                ),
                None => install.phase().label().to_string(),
            };
            return (format!("Updating to {version}"), detail);
        }
        if let Some(error) = &self.update_error {
            // The first sentence; the whole message is on hover and in Details.
            let first = error.split(". ").next().unwrap_or(error);
            let mut short: String = first.chars().take(120).collect();
            if short.len() < first.len() {
                short.push('…');
            }
            return ("Update didn't finish".into(), short);
        }
        if let Some(release) = &self.available {
            let detail = if let Some(reason) = &release.blocked {
                reason.clone()
            } else if let (Some(command), false) = (&release.update_command, release.msi) {
                command.clone()
            } else {
                format!("You have {VERSION}. Your scan is kept; RillDisk reopens it.")
            };
            return (format!("RillDisk {} is ready", release.version), detail);
        }
        if let Some(notice) = &self.update_notice {
            return (notice.clone(), String::new());
        }
        (String::new(), String::new())
    }

    /// The card's buttons, main one first: the wide card lays them out right to
    /// left, so the main button ends up at the far right there.
    fn card_buttons(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if self.installing.is_some() {
            return;
        }
        if let Some(error) = &self.update_error {
            if w::button(ui, Kind::Soft, Some(Icon::Refresh), "Retry")
                .on_hover_text(error)
                .clicked()
            {
                actions.push(match &self.available {
                    Some(release) if installable(release) => {
                        Action::Install(Box::new(release.clone()))
                    }
                    _ => Action::CheckUpdates,
                });
            }
            if w::button(ui, Kind::Ghost, Some(Icon::Copy), "Copy details")
                .on_hover_text(error)
                .clicked()
            {
                actions.push(Action::Copy(error.clone(), "details"));
            }
        } else if let Some(release) = &self.available {
            if installable(release) {
                let allowed = self.running.is_none() && !self.cleanup.modal_open();
                let button = ui
                    .add_enabled_ui(allowed, |ui| {
                        w::button(ui, Kind::Primary, None, "Update and restart")
                    })
                    .inner;
                if self.tour.is_some() {
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(egui::Id::unique("update-install-button"), button.rect)
                    });
                }
                if button
                    .on_disabled_hover_text("Finish the scan or cleanup before restarting")
                    .on_hover_text(if release.msi {
                        "Downloads and verifies the MSI, then asks Windows to install it. RillDisk restarts afterward."
                    } else {
                        "Downloads, verifies and replaces both programs, then restarts RillDisk."
                    })
                    .clicked()
                {
                    actions.push(Action::Install(Box::new(release.clone())));
                }
            } else if let Some(url) = &release.download_url
                && w::button(ui, Kind::Primary, Some(Icon::Download), "Download")
                    .on_hover_text(url.rsplit('/').next().unwrap_or(url))
                    .clicked()
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
            if w::button(ui, Kind::Ghost, None, "What's new").clicked() {
                ui.ctx()
                    .open_url(egui::OpenUrl::new_tab(&release.notes_url));
            }
        } else if self.update_notice.is_some()
            && w::button(ui, Kind::Ghost, None, "Dismiss").clicked()
        {
            actions.push(Action::DismissUpdateNotice);
        }
    }

    /// `v0.1.2` with a refresh mark: click to check for updates, right-click for
    /// the automatic-check setting.
    pub(super) fn version_button(&self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let checking = self.update_check.is_some();
        let off = rilldisk_update::disabled_by_env();
        let text = if checking {
            "Checking…".to_string()
        } else {
            format!("v{VERSION}")
        };
        let galley = ui
            .painter()
            .layout_no_wrap(text, style::regular(11.5), Color32::PLACEHOLDER);
        let size = vec2(galley.size().x + 32.0, 24.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let hovered = response.hovered() && !checking && !off;
        let p = ui.painter();
        if hovered {
            p.rect_filled(rect, 7, th.hover);
        }
        let color = if hovered { th.text } else { th.faint };
        p.galley(
            pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        let mark =
            Rect::from_center_size(pos2(rect.right() - 12.0, rect.center().y), vec2(12.0, 12.0));
        if checking {
            spinner(p, mark, ui.input(|i| i.time), th.dim);
            ui.ctx().request_repaint();
        } else if !off {
            icons::paint(p, mark, Icon::Refresh, color);
        }
        let tip = if off {
            format!(
                "RillDisk {VERSION}. Update checks are turned off by {}.",
                rilldisk_update::DISABLE_ENV
            )
        } else if checking {
            "Asking GitHub for the latest version…".to_string()
        } else {
            format!(
                "RillDisk {VERSION} · click to check for updates\nRight-click for update settings"
            )
        };
        let response = if hovered {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        };
        let response = response.on_hover_text(tip);
        if response.clicked() && !checking && !off {
            actions.push(Action::CheckUpdates);
        }
        response.context_menu(|ui| {
            ui.set_min_width(230.0);
            let mut automatic = update::automatic_checks();
            ui.add_enabled_ui(!off, |ui| {
                if ui.checkbox(&mut automatic, "Check automatically").changed() {
                    let _ = update::set_automatic_checks(automatic);
                }
            });
            w::menu_gap(ui);
            if w::menu_item(ui, Icon::Gear, "Update settings", "").clicked() {
                actions.push(Action::Settings(true));
                ui.close();
            }
        });
        actions
    }
}

fn installable(release: &update::Release) -> bool {
    release.blocked.is_none() && (release.update_command.is_none() || release.msi)
}

/// A small three-quarter ring that turns.
fn spinner(p: &egui::Painter, rect: Rect, time: f64, color: Color32) {
    let start = (time * 5.0) as f32;
    w::arc(
        p,
        rect.center(),
        rect.width() / 2.0 - 1.0,
        1.6,
        start,
        start + std::f32::consts::TAU * 0.72,
        color,
    );
}
