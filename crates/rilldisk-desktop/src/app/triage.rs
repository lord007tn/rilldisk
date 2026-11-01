// SPDX-License-Identifier: AGPL-3.0-only
//! Triage layout: a reclaim inbox. Categories on the left, suggestions in the
//! middle, and an inspector that explains one suggestion before you add it to
//! the cleanup plan. The disk views remain one click away in the same sidebar.
use super::*;

impl RillApp {
    pub(super) fn triage(&mut self, ui: &mut Ui) -> Vec<Action> {
        let th = t();
        let mut actions = Vec::new();
        let data = self.data.clone().expect("triage needs data");
        let side = egui::Panel::left("triage-nav")
            .exact_size(264.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(th.sidebar).inner_margin(Margin {
                left: 14,
                right: 14,
                top: 18,
                bottom: 16,
            }))
            .show(ui, |ui| self.triage_nav(ui, &data));
        actions.extend(side.inner);
        let central = egui::CentralPanel::default()
            .frame(Frame::new().fill(th.bg).inner_margin(Margin {
                left: 24,
                right: 24,
                top: 18,
                bottom: 20,
            }))
            .show(ui, |ui| {
                let mut actions = self.header(ui, &data);
                ui.add_space(14.0);
                if let Some(error) = &self.error {
                    error_banner(ui, error, Some(&data));
                    ui.add_space(10.0);
                }
                if let Some(running) = &self.running {
                    actions.extend(scanning(ui, running));
                    return actions;
                }
                match self.tab {
                    Tab::Cleanup => actions.extend(self.inbox(ui, &data)),
                    Tab::Overview => actions.extend(self.overview(ui, &data)),
                    Tab::Explore => actions.extend(self.explore(ui, &data)),
                    Tab::Largest => actions.extend(self.largest(ui, &data)),
                    Tab::Types => self.types(ui, &data),
                    Tab::Issues => self.issues(ui, &data),
                    Tab::Search => actions.extend(self.search_tab(ui, &data)),
                }
                actions
            });
        actions.extend(central.inner);
        actions
    }

    fn triage_nav(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let report = &data.reclaim;
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
            logo(ui.painter(), rect);
            ui.add_space(2.0);
            ui.label(RichText::new("RillDisk").font(style::semibold(17.0)));
        });
        ui.add_space(20.0);
        w::section_label(ui, "Reclaim");
        ui.add_space(2.0);
        let all_active = self.tab == Tab::Cleanup && self.category_filter.is_none();
        if nav_row(
            ui,
            Icon::Sparkle,
            th.accent,
            "All suggestions",
            &style::bytes(frees_total(report, self.allocated)),
            all_active,
        )
        .clicked()
        {
            self.tab = Tab::Cleanup;
            self.category_filter = None;
            self.inspect = None;
        }
        let mut categories: Vec<(Category, u64)> = Category::ALL
            .iter()
            .map(|&cat| {
                (
                    cat,
                    report
                        .candidates
                        .iter()
                        .filter(|c| c.category == cat && c.nested_in.is_none())
                        .map(|c| frees(c, self.allocated))
                        .sum(),
                )
            })
            .filter(|&(cat, _)| report.candidates.iter().any(|c| c.category == cat))
            .collect();
        categories.sort_by_key(|c| std::cmp::Reverse(c.1));
        for (cat, bytes) in categories {
            let active = self.tab == Tab::Cleanup && self.category_filter == Some(cat);
            let (icon, color) = category_look(cat);
            if nav_row(
                ui,
                icon,
                color,
                category_title(cat),
                &style::bytes(bytes),
                active,
            )
            .clicked()
            {
                self.tab = Tab::Cleanup;
                self.category_filter = Some(cat);
                self.inspect = None;
            }
        }
        ui.add_space(16.0);
        w::section_label(ui, "Explore");
        ui.add_space(2.0);
        let total = style::bytes(self.size(&data.snapshot, 0));
        let issues = data.snapshot.summary.issue_count;
        let mut views = vec![
            (Tab::Overview, Icon::Home, "Overview", total),
            (Tab::Explore, Icon::Rings, "Map and rings", String::new()),
            (Tab::Largest, Icon::Document, "Largest files", String::new()),
            (Tab::Types, Icon::Code, "File types", String::new()),
        ];
        if issues > 0 {
            views.push((
                Tab::Issues,
                Icon::Warning,
                "Unread folders",
                style::short_count(issues),
            ));
        }
        for (tab, icon, label, value) in views {
            if nav_row(ui, icon, th.dim, label, &value, self.tab == tab).clicked() {
                actions.push(Action::Tab(tab));
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(
                RichText::new(concat!(
                    "v",
                    env!("CARGO_PKG_VERSION"),
                    " · AGPL-3.0 · read-only"
                ))
                .font(style::regular(11.0))
                .color(th.faint),
            );
            ui.add_space(8.0);
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
            ui.add_space(6.0);
            if let Some(identity) = identity_picker(ui, self.identity) {
                actions.push(Action::Identity(identity));
            }
            ui.add_space(6.0);
            if let Some(shell) = shell_picker(ui, self.shell) {
                actions.push(Action::Shell(shell));
            }
            ui.add_space(10.0);
            // Scan target: drives and places live behind one button.
            let place = w::button(ui, Kind::Soft, Some(Icon::Drive), "Scan another place");
            egui::Popup::menu(&place).width(260.0).show(|ui| {
                for volume in &self.volumes {
                    let letter = volume.root.display().to_string();
                    if ui
                        .button(format!(
                            "{}  ·  {} free",
                            letter.trim_end_matches(['\\', '/']),
                            style::bytes(volume.free_bytes)
                        ))
                        .clicked()
                    {
                        actions.push(Action::Scan(letter));
                        ui.close();
                    }
                }
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
        });
        actions
    }

    /// Hero capacity bar, the suggestion list and the inspector.
    fn inbox(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let report = &data.reclaim;
        if report.candidates.is_empty() {
            empty_state(
                ui,
                Icon::Sparkle,
                "Nothing to clean up here",
                "No Git worktrees, build output, dependency folders or known caches were found in this scan.",
            );
            return actions;
        }
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        let hero = Rect::from_min_size(full.min, vec2(full.width(), 112.0));
        self.hero(ui, data, hero);
        let body = Rect::from_min_max(pos2(full.left(), hero.bottom() + 14.0), full.max);
        let inspector_w = (body.width() * 0.36).clamp(340.0, 440.0);
        let list = Rect::from_min_max(
            body.min,
            pos2(body.right() - inspector_w - 14.0, body.bottom()),
        );
        let inspector = Rect::from_min_max(pos2(list.right() + 14.0, body.top()), body.max);

        // Suggestions of the chosen category, biggest saving first.
        let mut items: Vec<usize> = (0..report.candidates.len())
            .filter(|&i| {
                let c = &report.candidates[i];
                self.category_filter.is_none_or(|f| f == c.category)
                    && self.safety_filter.is_none_or(|s| s == c.safety)
            })
            .collect();
        items.sort_by_key(|&i| std::cmp::Reverse(frees(&report.candidates[i], self.allocated)));
        if self.inspect.is_none_or(|i| !items.contains(&i)) {
            self.inspect = items.first().copied();
        }
        let mut head = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_size(list.min, vec2(list.width(), 34.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let title = self
            .category_filter
            .map_or("All suggestions", category_title);
        head.label(RichText::new(title).font(style::semibold(17.0)));
        head.label(RichText::new(style::count(items.len() as u64)).color(th.faint));
        head.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut safety = self.safety_filter;
            if w::segmented(
                ui,
                &mut safety,
                &[
                    (None, None, "All"),
                    (Some(Safety::Regenerable), None, "Rebuildable"),
                    (Some(Safety::Cache), None, "Caches"),
                    (Some(Safety::Review), None, "Review"),
                ],
                "Rebuildable: project tools recreate it. Caches: re-downloaded on demand. Review: may hold unique work.",
            ) {
                self.safety_filter = safety;
            }
        });
        let rows = Rect::from_min_max(pos2(list.left(), list.top() + 44.0), list.max);
        let mut child = ui.new_child(UiBuilder::new().max_rect(rows));
        egui::ScrollArea::vertical()
            .id_salt(("inbox", self.category_filter.map(|c| c as u8)))
            .auto_shrink(false)
            .show_rows(&mut child, 64.0, items.len() + 1, |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for index in range {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::click());
                    if let Some(&i) = items.get(index) {
                        actions.extend(self.candidate_card(ui, data, i, rect, response, true));
                    }
                }
            });
        actions.extend(self.plan_bar(ui, data, rows));
        if let Some(i) = self.inspect {
            actions.extend(self.inspector(ui, data, i, inspector));
        }
        actions
    }

    /// Disk capacity with the reclaimable share carved out of the used part.
    fn hero(&self, ui: &mut Ui, data: &Data, rect: Rect) {
        let th = t();
        let p = ui.painter().clone();
        p.rect_filled(rect, 16, th.card);
        let inner = rect.shrink2(vec2(22.0, 18.0));
        let frees = frees_total(&data.reclaim, self.allocated);
        let root = data.snapshot.summary.root.display().to_string();
        p.text(
            inner.left_top(),
            Align2::LEFT_TOP,
            format!("Reclaimable in {root}"),
            style::regular(12.5),
            th.dim,
        );
        w::big_bytes(
            &p,
            inner.left_top() + vec2(0.0, 20.0),
            Align2::LEFT_TOP,
            frees,
            34.0,
            th.text,
        );
        let review = data
            .reclaim
            .candidates
            .iter()
            .filter(|c| c.safety == Safety::Review && c.nested_in.is_none())
            .count();
        p.text(
            inner.left_bottom(),
            Align2::LEFT_BOTTOM,
            format!(
                "{} suggestions · {} to review first",
                style::count(data.reclaim.candidates.len() as u64),
                review
            ),
            style::regular(12.0),
            th.faint,
        );
        let bar_left = inner.left() + inner.width() * 0.42;
        let bar = Rect::from_min_max(
            pos2(bar_left, inner.top() + 22.0),
            pos2(inner.right(), inner.top() + 36.0),
        );
        let (total, used, label) = match self.volume_of(&root) {
            Some(v) => (
                v.total_bytes,
                v.total_bytes.saturating_sub(v.free_bytes),
                format!(
                    "{} ({})",
                    if v.label.is_empty() {
                        "Local Disk"
                    } else {
                        &v.label
                    },
                    v.root.display().to_string().trim_end_matches(['\\', '/'])
                ),
            ),
            None => {
                let size = self.size(&data.snapshot, 0);
                (size, size, "This scan".to_string())
            }
        };
        p.text(
            bar.left_top() - vec2(0.0, 6.0),
            Align2::LEFT_BOTTOM,
            label,
            style::semibold(12.5),
            th.text,
        );
        let reclaim = frees.min(used);
        let total = total.max(1) as f32;
        p.rect_filled(bar, 7, th.field);
        let used_w = bar.width() * used as f32 / total;
        let reclaim_w = bar.width() * reclaim as f32 / total;
        let kept = Rect::from_min_size(bar.min, vec2((used_w - reclaim_w).max(0.0), bar.height()));
        p.rect_filled(
            kept,
            7,
            style::alpha(th.dim, if th.dark { 120 } else { 90 }),
        );
        let carved = Rect::from_min_size(
            pos2(kept.right(), bar.top()),
            vec2(reclaim_w.max(3.0), bar.height()),
        );
        p.rect_filled(carved, 7, th.accent);
        let free = total as u64 - used.min(total as u64);
        let legend_y = bar.bottom() + 12.0;
        let mut x = bar.left();
        for (color, text) in [
            (
                style::alpha(th.dim, 160),
                format!("Used {}", style::bytes(used)),
            ),
            (th.accent, format!("Reclaimable {}", style::bytes(reclaim))),
            (
                th.field,
                format!(
                    "Free {}  →  {} after cleanup",
                    style::bytes(free),
                    style::bytes(free + reclaim)
                ),
            ),
        ] {
            p.circle_filled(pos2(x + 4.0, legend_y + 7.0), 4.0, color);
            let g = p.layout_no_wrap(text, style::regular(12.0), th.dim);
            let width = g.size().x;
            p.galley(pos2(x + 13.0, legend_y), g, th.dim);
            x += width + 30.0;
        }
    }

    fn inspector(&mut self, ui: &mut Ui, data: &Data, index: usize, rect: Rect) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let c = &data.reclaim.candidates[index];
        let snapshot = &data.snapshot;
        ui.painter().rect_filled(rect, 16, th.card);
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect.shrink(20.0)));
        egui::ScrollArea::vertical()
            .id_salt("inspector")
            .auto_shrink(false)
            .show(&mut child, |ui| {
                let color = style::safety_color(c.safety);
                ui.horizontal(|ui| {
                    let (tile, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                    ui.painter().rect_filled(
                        tile,
                        12,
                        style::alpha(color, if th.dark { 46 } else { 34 }),
                    );
                    icons::paint(ui.painter(), tile.shrink(11.0), category_icon(c), color);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(c.label).font(style::semibold(16.5)));
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            w::chip(ui, c.safety.label(), color);
                            if let Some(agent) = c.agent {
                                w::chip(ui, agent, agent_color(agent));
                            }
                        });
                    });
                });
                ui.add_space(14.0);
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
                let size = w::big_bytes(
                    ui.painter(),
                    rect.left_top(),
                    Align2::LEFT_TOP,
                    frees(c, self.allocated),
                    30.0,
                    th.text,
                );
                ui.painter().text(
                    pos2(size.right() + 10.0, size.bottom() - 6.0),
                    Align2::LEFT_BOTTOM,
                    "freed if removed",
                    style::regular(12.5),
                    th.dim,
                );
                ui.add_space(6.0);
                ui.label(RichText::new(safety_help(c.safety)).color(th.dim));
                ui.add_space(12.0);
                let idle = match c.idle_days {
                    Some(0) => "today".to_string(),
                    Some(d) => format!("{d} days ago"),
                    None => "unknown".to_string(),
                };
                let mut facts = vec![
                    ("Size", style::bytes(c.logical_bytes)),
                    (
                        "On disk",
                        c.allocated_bytes
                            .map(style::bytes)
                            .unwrap_or("unknown".into()),
                    ),
                    ("Files", style::count(c.file_count)),
                    ("Last change", idle),
                ];
                if c.shared_bytes > 0 {
                    facts.push((
                        "Hard-linked",
                        format!("{} stays (pnpm, Cargo)", style::bytes(c.shared_bytes)),
                    ));
                }
                for (label, value) in facts {
                    ui.horizontal(|ui| {
                        let (cell, _) = ui.allocate_exact_size(vec2(96.0, 20.0), Sense::hover());
                        ui.painter().text(
                            cell.left_center(),
                            Align2::LEFT_CENTER,
                            label,
                            style::regular(13.0),
                            th.faint,
                        );
                        ui.label(RichText::new(value).font(style::semibold(13.0)));
                    });
                }
                ui.add_space(12.0);
                w::section_label(ui, "Why");
                ui.label(RichText::new(&c.reason).color(th.text));
                ui.add_space(12.0);
                w::section_label(ui, "Inside");
                let (map, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 150.0), Sense::click());
                let params = Params {
                    root: c.entry_id,
                    rect: map,
                    allocated: self.allocated,
                    generation: self.generation,
                    color_by: ColorBy::Kind,
                    pixels_per_point: ui.ctx().pixels_per_point(),
                    compact: true,
                };
                if self.inspect_map.key != Some(treemap::key(&params)) {
                    self.inspect_map = treemap::build(snapshot, &params);
                }
                let painter = ui.painter_at(map);
                painter.rect_filled(map, 12, th.field);
                painter.add(Shape::mesh(self.inspect_map.mesh.clone()));
                if let Some(pos) = response.hover_pos()
                    && let Some(tile) = self.inspect_map.hit(pos).copied()
                {
                    treemap::outline(&painter, &tile, style::alpha(th.ink, 180), 1.5);
                    let id = tile.id as usize;
                    let total = self.size(snapshot, c.entry_id);
                    response.on_hover_ui_at_pointer(|ui| {
                        entry_tooltip(ui, data, id, self.allocated, total)
                    });
                }
                ui.add_space(12.0);
                w::section_label(ui, "Suggested");
                Frame::new()
                    .fill(th.field)
                    .corner_radius(10)
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&c.suggested_action).font(style::mono(12.0)));
                    });
                ui.add_space(6.0);
                let path = w::fit(
                    ui.painter(),
                    c.path.display().to_string(),
                    style::mono(11.5),
                    th.faint,
                    ui.available_width(),
                );
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
                ui.painter().galley(r.left_top(), path, th.faint);
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    let picked = self.picked.contains(&index);
                    let (kind, icon, label) = if picked {
                        (Kind::Soft, Icon::Check, "In plan")
                    } else {
                        (Kind::Primary, Icon::Sparkle, "Add to plan")
                    };
                    if w::button(ui, kind, Some(icon), label).clicked() {
                        actions.push(Action::Pick(index));
                    }
                    if w::button(ui, Kind::Soft, Some(Icon::Terminal), "Copy command").clicked() {
                        actions.push(Action::Copy(c.suggested_action.clone(), "command"));
                    }
                    if w::button(ui, Kind::Ghost, Some(Icon::Eye), "Show in map").clicked() {
                        actions.push(Action::Explore(c.entry_id));
                        self.highlight = true;
                    }
                });
            });
        actions
    }
}

fn nav_row(
    ui: &mut Ui,
    icon: Icon,
    color: Color32,
    label: &str,
    value: &str,
    active: bool,
) -> egui::Response {
    let th = t();
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let p = ui.painter();
    w::row_fill(p, rect, response.hovered(), active);
    let tile = Rect::from_min_size(rect.left_top() + vec2(8.0, 6.0), vec2(24.0, 24.0));
    p.rect_filled(tile, 7, style::alpha(color, if th.dark { 46 } else { 34 }));
    icons::paint(p, tile.shrink(5.5), icon, color);
    let value_w = if value.is_empty() {
        0.0
    } else {
        p.layout_no_wrap(value.to_string(), style::regular(12.0), th.dim)
            .size()
            .x
            + 10.0
    };
    let name = w::fit(
        p,
        label.to_string(),
        if active {
            style::semibold(13.0)
        } else {
            style::regular(13.0)
        },
        th.text,
        rect.width() - 52.0 - value_w,
    );
    p.galley(
        pos2(tile.right() + 10.0, rect.center().y - name.size().y / 2.0),
        name,
        th.text,
    );
    if !value.is_empty() {
        p.text(
            pos2(rect.right() - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            value,
            style::regular(12.0),
            if active { th.text } else { th.dim },
        );
    }
    response
}

fn category_look(category: Category) -> (Icon, Color32) {
    let th = t();
    match category {
        Category::GitWorktree => (Icon::Agent, th.warn),
        Category::BuildOutput => (Icon::Grid, th.good),
        Category::Dependencies => (Icon::Folder, th.good),
        Category::PackageCache => (Icon::Download, th.info),
        Category::AppCache => (Icon::Download, th.info),
        Category::Temporary => (Icon::Refresh, th.info),
        Category::RecycleBin => (Icon::Document, th.warn),
    }
}
