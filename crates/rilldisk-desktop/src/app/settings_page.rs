// SPDX-License-Identifier: AGPL-3.0-only
//! Settings: appearance, sizes, updates, privacy and about. Everything here is
//! saved immediately; there is no Apply button.
use super::{Action, RillApp};
use crate::icons::{self, Icon};
use crate::style::{self, Identity, t};
use crate::update;
use crate::widgets::{self as w, Kind};
use eframe::egui::{
    self, Align, Align2, CursorIcon, Layout, Margin, Rect, RichText, Sense, Ui, pos2, vec2,
};

const REPO: &str = "https://github.com/lord007tn/rilldisk";

impl RillApp {
    pub(super) fn settings_page(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        ui.horizontal(|ui| {
            ui.set_height(34.0);
            if w::icon_button(ui, Icon::Back, "Back (Esc)").clicked() {
                actions.push(Action::Settings(false));
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Settings").font(style::semibold(20.0)));
        });
        ui.add_space(14.0);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let width = ui.available_width().min(760.0);
            ui.set_max_width(width);

            // ---------------------------------------------------- appearance
            section(ui, "Appearance", |ui| {
                row(ui, "Theme", "Follow the system, or always use light or dark.", |ui| {
                    let mut choice = self.prefs.theme;
                    if w::segmented(
                        ui,
                        &mut choice,
                        &[
                            (None, None, "System"),
                            (Some(false), Some(Icon::Sun), "Light"),
                            (Some(true), Some(Icon::Moon), "Dark"),
                        ],
                        "Theme",
                    ) {
                        actions.push(Action::ThemeChoice(choice));
                    }
                });
                ui.add_space(14.0);
                ui.label(RichText::new("Palette").font(style::semibold(13.0)));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    for identity in Identity::ALL {
                        if palette_card(ui, identity, identity == self.identity, self.dark) {
                            actions.push(Action::Identity(identity));
                        }
                    }
                });
            });

            // --------------------------------------------------------- sizes
            section(ui, "Sizes", |ui| {
                row(
                    ui,
                    "Show sizes as",
                    "Size is each file's length. On disk counts allocated space, with hard-linked files counted once. Also switchable from the top bar.",
                    |ui| {
                        let mut allocated = self.allocated;
                        if w::segmented(
                            ui,
                            &mut allocated,
                            &[(false, None, "Size"), (true, None, "On disk")],
                            "Default size metric",
                        ) {
                            actions.push(Action::Metric(allocated));
                        }
                    },
                );
            });

            // ------------------------------------------------------- updates
            section(ui, "Updates", |ui| {
                row(
                    ui,
                    "Check automatically",
                    "Once a day on startup, RillDisk asks GitHub for the latest version. Nothing about you or your files is sent.",
                    |ui| {
                        let mut automatic = update::automatic_checks();
                        if toggle(ui, &mut automatic, !rilldisk_update::disabled_by_env()) {
                            let _ = update::set_automatic_checks(automatic);
                        }
                    },
                );
                ui.add_space(10.0);
                let busy = self.update_check.is_some();
                let status = if rilldisk_update::disabled_by_env() {
                    format!("Turned off by {}.", rilldisk_update::DISABLE_ENV)
                } else if let Some(release) = &self.available {
                    format!("RillDisk {} is available.", release.version)
                } else if busy {
                    "Checking…".into()
                } else {
                    "Check now to see if a newer version exists.".into()
                };
                row(
                    ui,
                    concat!("Version ", env!("CARGO_PKG_VERSION")),
                    &status,
                    |ui| {
                        ui.add_enabled_ui(!busy && !rilldisk_update::disabled_by_env(), |ui| {
                            if w::button(ui, Kind::Soft, Some(Icon::Refresh), "Check now").clicked() {
                                actions.push(Action::CheckUpdates);
                            }
                        });
                    },
                );
                if let Some(action) = self.update_card(ui) {
                    actions.push(action);
                }
            });

            // ------------------------------------------------------- privacy
            section(ui, "Privacy", |ui| {
                for (icon, text) in [
                    (Icon::Eye, "Reads file names, sizes and dates only. File contents are never opened."),
                    (Icon::Check, "Nothing is uploaded. The only network request is the optional update check."),
                    (Icon::Terminal, "The CLI and the MCP server for AI assistants are read-only."),
                ] {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                        icons::paint(ui.painter(), rect, icon, th.dim);
                        ui.add_space(4.0);
                        ui.label(RichText::new(text).font(style::regular(12.5)).color(th.dim));
                    });
                    ui.add_space(4.0);
                }
            });

            // --------------------------------------------------------- about
            section(ui, "About", |ui| {
                ui.label(
                    RichText::new(concat!("RillDisk ", env!("CARGO_PKG_VERSION")))
                        .font(style::semibold(14.0)),
                );
                ui.label(
                    RichText::new("Free software under the GNU AGPL 3.0. Made in Rust, drawn with your GPU.")
                        .font(style::regular(12.5))
                        .color(th.dim),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    for (label, url) in [
                        ("Source code", REPO.to_string()),
                        ("Release notes", format!("{REPO}/releases")),
                        ("Report a problem", format!("{REPO}/issues/new")),
                        ("Install guide", format!("{REPO}/blob/main/docs/INSTALL.md")),
                    ] {
                        if link(ui, label).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                        }
                    }
                });
            });
            ui.add_space(24.0);
        });
        actions
    }
}

fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    w::section_label(ui, title);
    ui.add_space(6.0);
    w::card_frame()
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
    ui.add_space(20.0);
}

/// A setting: title and explanation on the left, its control on the right.
fn row(ui: &mut Ui, title: &str, detail: &str, control: impl FnOnce(&mut Ui)) {
    let th = t();
    ui.horizontal(|ui| {
        let text_width = (ui.available_width() - 260.0).max(240.0);
        ui.vertical(|ui| {
            ui.set_width(text_width);
            ui.label(RichText::new(title).font(style::semibold(13.0)));
            ui.add(
                egui::Label::new(
                    RichText::new(detail)
                        .font(style::regular(12.0))
                        .color(th.dim),
                )
                .wrap(),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}

/// A small on/off switch. Returns true when flipped.
fn toggle(ui: &mut Ui, on: &mut bool, enabled: bool) -> bool {
    let th = t();
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    let response = response.on_hover_cursor(if enabled {
        CursorIcon::PointingHand
    } else {
        CursorIcon::NotAllowed
    });
    let clicked = enabled && response.clicked();
    if clicked {
        *on = !*on;
    }
    let anim = ui.ctx().animate_bool(response.id, *on);
    let p = ui.painter();
    let track = if enabled {
        style::mix(th.field, th.accent, anim)
    } else {
        th.field
    };
    p.rect_filled(rect, 11, track);
    let knob = pos2(rect.left() + 11.0 + anim * 18.0, rect.center().y);
    p.circle_filled(
        knob,
        8.0,
        if *on && enabled { th.on_accent } else { th.dim },
    );
    clicked
}

/// Text that looks and behaves like a link: pointer and underline on hover.
pub fn link(ui: &mut Ui, text: &str) -> egui::Response {
    let th = t();
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), style::regular(12.5), th.accent);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    let color = if response.hovered() {
        th.text
    } else {
        th.accent
    };
    ui.painter().galley(rect.min, galley, color);
    if response.hovered() {
        ui.painter().line_segment(
            [rect.left_bottom(), rect.right_bottom()],
            egui::Stroke::new(1.0, color),
        );
    }
    response
}

/// A palette preview card: its gradient, accent and name. Returns true on click.
fn palette_card(ui: &mut Ui, identity: Identity, active: bool, dark: bool) -> bool {
    let th = t();
    let theme = identity.theme(dark);
    let (rect, response) = ui.allocate_exact_size(vec2(170.0, 96.0), Sense::click());
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    let p = ui.painter();
    p.rect_filled(rect, 14, theme.bg);
    if active || response.hovered() {
        p.rect_stroke(
            rect,
            14,
            egui::Stroke::new(
                if active { 2.0 } else { 1.0 },
                if active { theme.accent } else { th.faint },
            ),
            egui::StrokeKind::Inside,
        );
    }
    // Gradient preview: six tiles like a tiny treemap row.
    let strip = Rect::from_min_size(rect.min + vec2(14.0, 14.0), vec2(rect.width() - 28.0, 38.0));
    let swatches = theme.swatches();
    let tile = strip.width() / swatches.len() as f32;
    for (i, color) in swatches.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(strip.left() + i as f32 * tile, strip.top()),
            vec2(tile - 3.0, strip.height()),
        );
        p.rect_filled(r, 5, *color);
    }
    p.circle_filled(
        pos2(rect.left() + 21.0, rect.bottom() - 22.0),
        6.0,
        theme.accent,
    );
    p.text(
        pos2(rect.left() + 34.0, rect.bottom() - 22.0),
        Align2::LEFT_CENTER,
        identity.name(),
        if active {
            style::semibold(13.0)
        } else {
            style::regular(13.0)
        },
        theme.text,
    );
    if active {
        let check = Rect::from_center_size(
            pos2(rect.right() - 22.0, rect.bottom() - 22.0),
            vec2(14.0, 14.0),
        );
        icons::paint(p, check, Icon::Check, theme.accent);
    }
    response.clicked() && !active
}
