// SPDX-License-Identifier: AGPL-3.0-only
//! Canvas layout: the chart fills the window and every control floats above it.
//! A top bar holds place and search, a dock switches views, and the right side
//! shows either the live folder inspector or a sheet (Clean up, Largest, Types).
use super::*;

const MARGIN: f32 = 18.0;

impl RillApp {
    pub(super) fn canvas(&mut self, ui: &mut Ui) -> Vec<Action> {
        let th = t();
        let data = self.data.clone().expect("canvas needs data");
        let central = egui::CentralPanel::default()
            .frame(Frame::new().fill(th.bg))
            .show(ui, |ui| {
                let mut actions = Vec::new();
                let full = ui.max_rect();
                ui.allocate_rect(full, Sense::hover());
                let top = Rect::from_min_size(
                    full.min + vec2(MARGIN, MARGIN),
                    vec2(full.width() - MARGIN * 2.0, 56.0),
                );
                let sheet = !matches!(self.tab, Tab::Explore | Tab::Overview);
                let side_w = if sheet {
                    (full.width() * 0.46).clamp(520.0, 680.0)
                } else {
                    (full.width() * 0.25).clamp(300.0, 360.0)
                };
                let side = Rect::from_min_max(
                    pos2(full.right() - MARGIN - side_w, top.bottom() + 14.0),
                    pos2(full.right() - MARGIN, full.bottom() - MARGIN),
                );
                let dock_h = 52.0;
                let stage = Rect::from_min_max(
                    pos2(full.left() + MARGIN, top.bottom() + 14.0),
                    pos2(side.left() - 16.0, full.bottom() - MARGIN - dock_h - 14.0),
                );
                let painter = ui.painter().clone();
                w::glow(
                    &painter,
                    stage.center(),
                    stage.width().max(stage.height()) * 0.62,
                    style::alpha(th.accent, if th.dark { 26 } else { 30 }),
                );
                if let Some(running) = &self.running {
                    let mut child = ui.new_child(UiBuilder::new().max_rect(full.shrink(MARGIN)));
                    actions.extend(scanning(&mut child, running));
                    return actions;
                }
                actions.extend(self.chart_area(ui, &data, stage, stage.expand(4.0)));
                actions.extend(self.canvas_top(ui, &data, top));
                if sheet {
                    actions.extend(self.canvas_sheet(ui, &data, side));
                } else {
                    w::shadow(&painter, side, 18);
                    actions.extend(self.side_panel(ui, &data, side, th.popup));
                }
                actions.extend(self.canvas_dock(
                    ui,
                    &data,
                    Rect::from_min_max(
                        pos2(stage.left(), stage.bottom() + 14.0),
                        pos2(stage.right(), full.bottom() - MARGIN),
                    ),
                ));
                actions
            });
        central.inner
    }

    fn canvas_top(&mut self, ui: &mut Ui, data: &Data, rect: Rect) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let snapshot = &data.snapshot;
        w::floating(ui.painter(), rect, 18);
        let mut bar = ui.new_child(
            UiBuilder::new()
                .max_rect(rect.shrink2(vec2(12.0, 12.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        bar.spacing_mut().item_spacing.x = 6.0;
        let (mark, _) = bar.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
        logo(bar.painter(), mark);
        bar.add_space(4.0);
        // Place picker: drives, places and a folder dialog in one popup.
        let root = snapshot.summary.root.display().to_string();
        let place = w::button(&mut bar, Kind::Soft, Some(Icon::Drive), &root)
            .on_hover_text("Scan another drive or folder");
        egui::Popup::menu(&place).width(280.0).show(|ui| {
            ui.set_min_width(260.0);
            w::section_label(ui, "Drives");
            for volume in &self.volumes {
                let letter = volume.root.display().to_string();
                let label = if volume.label.is_empty() {
                    "Local Disk"
                } else {
                    &volume.label
                };
                if ui
                    .button(format!(
                        "{label} ({})  ·  {} free",
                        letter.trim_end_matches(['\\', '/']),
                        style::bytes(volume.free_bytes)
                    ))
                    .clicked()
                {
                    actions.push(Action::Scan(letter));
                    ui.close();
                }
            }
            ui.add_space(4.0);
            w::section_label(ui, "Places");
            for (name, _, path) in &self.places {
                if ui.button(*name).on_hover_text(path).clicked() {
                    actions.push(Action::Scan(path.clone()));
                    ui.close();
                }
            }
            if ui.button("Choose folder…").clicked() {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    actions.push(Action::Scan(folder.display().to_string()));
                }
                ui.close();
            }
        });
        bar.add_space(6.0);
        bar.add_enabled_ui(self.zoom != 0, |ui| {
            if w::icon_button(ui, Icon::Back, "Up one level (Backspace)").clicked() {
                actions.push(Action::Up);
            }
        });
        let chain = ancestors(snapshot, self.zoom);
        let shown: Vec<usize> = chain.iter().copied().skip(1).collect();
        let n = shown.len();
        for (i, &id) in shown.iter().enumerate() {
            if n > 4 && i < n - 3 {
                if i == 0 {
                    crumb_separator(&mut bar);
                    bar.label(RichText::new("…").color(th.faint));
                }
                continue;
            }
            crumb_separator(&mut bar);
            if crumb(&mut bar, &snapshot.name(id), i == n - 1).clicked() {
                actions.push(Action::Zoom(id));
            }
        }
        bar.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let root = snapshot.summary.root.display().to_string();
            ui.menu_button(RichText::new("···").font(style::semibold(15.0)), |ui| {
                ui.set_min_width(240.0);
                w::section_label(ui, "Layout");
                if let Some(shell) = shell_picker(ui, self.shell) {
                    actions.push(Action::Shell(shell));
                }
                ui.add_space(6.0);
                w::section_label(ui, "Palette");
                if let Some(identity) = identity_picker(ui, self.identity) {
                    actions.push(Action::Identity(identity));
                }
                ui.add_space(6.0);
                let mut dark = self.dark;
                if w::segmented(
                    ui,
                    &mut dark,
                    &[
                        (false, Some(Icon::Sun), "Light"),
                        (true, Some(Icon::Moon), "Dark"),
                    ],
                    "Theme",
                ) {
                    actions.push(Action::Theme(dark));
                }
            });
            if w::icon_button(ui, Icon::Refresh, "Rescan (F5)").clicked() {
                actions.push(Action::Scan(root));
            }
            w::segmented(
                ui,
                &mut self.allocated,
                &[(false, None, "Size"), (true, None, "On disk")],
                "Size: file length. On disk: allocated clusters, hard links counted once.",
            );
            let search = w::search_field(ui, &mut self.search, "Find files   Ctrl+F", 220.0);
            if search.changed() {
                self.search_edited = Some(ui.input(|i| i.time));
            }
            if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.search_edited = None;
                self.run_search();
                actions.push(Action::Tab(Tab::Search));
            }
            if std::mem::take(&mut self.focus_search) {
                search.request_focus();
            }
            let s = &snapshot.summary;
            if !s.complete {
                let text = if s.cancelled {
                    "Stopped early · partial".to_string()
                } else {
                    format!("Partial · {} unread", style::short_count(s.issue_count))
                };
                if w::chip(ui, &text, th.warn)
                    .interact(Sense::click())
                    .clicked()
                {
                    actions.push(Action::Tab(Tab::Issues));
                }
            }
        });
        actions
    }

    fn canvas_dock(&mut self, ui: &mut Ui, data: &Data, area: Rect) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let width = 860.0_f32.min(area.width());
        let rect = Rect::from_center_size(area.center(), vec2(width, area.height()));
        w::floating(ui.painter(), rect, 18);
        let mut dock = ui.new_child(
            UiBuilder::new()
                .max_rect(rect.shrink2(vec2(10.0, 10.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        dock.spacing_mut().item_spacing.x = 8.0;
        self.chart_controls(&mut dock);
        dock.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (tab, label) in [(Tab::Types, "Types"), (Tab::Largest, "Largest")] {
                let kind = if self.tab == tab {
                    Kind::Soft
                } else {
                    Kind::Ghost
                };
                if w::button(ui, kind, None, label).clicked() {
                    actions.push(Action::Tab(if self.tab == tab {
                        Tab::Explore
                    } else {
                        tab
                    }));
                }
            }
            let frees = frees_total(&data.reclaim, self.allocated);
            let label = format!("Clean up · {}", style::bytes(frees));
            let kind = if self.tab == Tab::Cleanup {
                Kind::Soft
            } else {
                Kind::Primary
            };
            if w::button(ui, kind, Some(Icon::Sparkle), &label).clicked() {
                actions.push(Action::Tab(if self.tab == Tab::Cleanup {
                    Tab::Explore
                } else {
                    Tab::Cleanup
                }));
            }
            let _ = th;
        });
        actions
    }

    fn canvas_sheet(&mut self, ui: &mut Ui, data: &Data, rect: Rect) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        w::floating(ui.painter(), rect, 18);
        let title = match self.tab {
            Tab::Cleanup => "Clean up",
            Tab::Largest => "Largest files",
            Tab::Types => "File types",
            Tab::Issues => "Unread folders",
            Tab::Search => "Search",
            Tab::Overview | Tab::Explore => "",
        };
        let head =
            Rect::from_min_size(rect.min + vec2(20.0, 14.0), vec2(rect.width() - 34.0, 32.0));
        ui.painter().text(
            head.left_center(),
            Align2::LEFT_CENTER,
            title,
            style::semibold(17.0),
            th.text,
        );
        let mut close = ui.new_child(
            UiBuilder::new()
                .max_rect(head)
                .layout(Layout::right_to_left(Align::Center)),
        );
        if w::button(&mut close, Kind::Ghost, None, "Close").clicked() {
            actions.push(Action::Tab(Tab::Explore));
        }
        let body = Rect::from_min_max(
            pos2(rect.left() + 14.0, head.bottom() + 10.0),
            rect.max - vec2(14.0, 12.0),
        );
        let mut child = ui.new_child(UiBuilder::new().max_rect(body));
        match self.tab {
            Tab::Cleanup => actions.extend(self.cleanup(&mut child, data)),
            Tab::Largest => actions.extend(self.largest(&mut child, data)),
            Tab::Types => self.types(&mut child, data),
            Tab::Issues => self.issues(&mut child, data),
            Tab::Search => actions.extend(self.search_tab(&mut child, data)),
            Tab::Overview | Tab::Explore => {}
        }
        actions
    }
}
