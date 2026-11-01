// SPDX-License-Identifier: AGPL-3.0-only
use crate::capture::Tour;
use crate::style::{self, FileClass};
use crate::treemap::{self, TileKind};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Pos2, Rect,
    RichText, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};
use rilldisk_core::{
    EntryKind, ReclaimReport, Safety, ScanControl, ScanOptions, Snapshot, TypeStat, Volume,
    file_types, find_reclaimable, largest_files, scan, volumes,
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, atomic::Ordering, mpsc},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const ROW_H: f32 = 22.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Map,
    Reclaim,
    Largest,
    Types,
    Issues,
    Search,
}

pub struct Data {
    pub snapshot: Arc<Snapshot>,
    pub reclaim: ReclaimReport,
    /// Entry ID -> index into `reclaim.candidates`.
    pub candidate_of: HashMap<usize, usize>,
    pub types: Vec<TypeStat>,
    pub classes: Vec<(FileClass, u64, u64)>,
    pub largest: [Vec<usize>; 2],
    pub analysis_ms: u128,
}

impl Data {
    fn analyze(snapshot: Snapshot) -> Self {
        let start = Instant::now();
        let reclaim = find_reclaimable(&snapshot, now_ms());
        let candidate_of = reclaim
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (c.entry_id, i))
            .collect();
        let types = file_types(&snapshot);
        let mut classes: HashMap<FileClass, (u64, u64)> = HashMap::new();
        for t in &types {
            let e = classes.entry(FileClass::of(&t.extension)).or_default();
            e.0 += t.logical_bytes;
            e.1 += t.allocated_bytes.unwrap_or(0);
        }
        let mut classes: Vec<_> = classes.into_iter().map(|(c, (l, a))| (c, l, a)).collect();
        classes.sort_by_key(|c| std::cmp::Reverse(c.1));
        let largest = [
            largest_files(&snapshot, 1000, false),
            largest_files(&snapshot, 1000, true),
        ];
        Self {
            snapshot: Arc::new(snapshot),
            reclaim,
            candidate_of,
            types,
            classes,
            largest,
            analysis_ms: start.elapsed().as_millis(),
        }
    }
}

struct Running {
    control: Arc<ScanControl>,
    root: String,
    started: Instant,
    rx: mpsc::Receiver<Result<Data, String>>,
}

enum Action {
    Select(usize),
    Zoom(usize),
    Toggle(usize),
    Reveal(usize),
    Copy(String, &'static str),
    Tab(Tab),
    Scan(String),
}

pub struct RillApp {
    pub path: String,
    volumes: Vec<Volume>,
    running: Option<Running>,
    pub data: Option<Data>,
    error: Option<String>,
    pub allocated: bool,
    pub tab: Tab,
    expanded: HashSet<usize>,
    rows: Vec<(u32, u16)>,
    rows_dirty: bool,
    pub selected: Option<usize>,
    pub zoom: usize,
    pub layout: treemap::Layout,
    generation: usize,
    scroll_to_selected: bool,
    pub search: String,
    search_results: Option<(String, Vec<usize>)>,
    search_rx: Option<mpsc::Receiver<(String, Vec<usize>)>>,
    reclaim_filter: Option<Safety>,
    pub highlight_reclaim: bool,
    toast: Option<(String, f64)>,
    pub tour: Option<Tour>,
    pub injected: Vec<egui::Event>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl RillApp {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<String>, tour: Option<Tour>) -> Self {
        style::apply(&cc.egui_ctx);
        let volumes = volumes();
        let default_path = path.clone().unwrap_or_else(|| {
            std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| "C:\\".into())
        });
        let mut app = Self {
            path: default_path,
            volumes,
            running: None,
            data: None,
            error: None,
            allocated: false,
            tab: Tab::Map,
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_dirty: true,
            selected: None,
            zoom: 0,
            layout: Default::default(),
            generation: 0,
            scroll_to_selected: false,
            search: String::new(),
            search_results: None,
            search_rx: None,
            reclaim_filter: None,
            highlight_reclaim: false,
            toast: None,
            tour,
            injected: Vec::new(),
        };
        if path.is_some() {
            app.start_scan(&cc.egui_ctx);
        }
        app
    }

    pub fn start_scan(&mut self, ctx: &egui::Context) {
        if self.running.is_some() {
            return;
        }
        let root = self.path.trim().trim_matches('"').to_string();
        if root.is_empty() {
            return;
        }
        let control = Arc::new(ScanControl::default());
        let (tx, rx) = mpsc::channel();
        let (thread_control, thread_root, repaint) =
            (control.clone(), PathBuf::from(&root), ctx.clone());
        std::thread::Builder::new()
            .name("rilldisk-scan".into())
            .spawn(move || {
                let result = scan(&thread_root, ScanOptions::default(), thread_control)
                    .map(Data::analyze)
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                repaint.request_repaint();
            })
            .expect("spawn scan thread");
        self.error = None;
        self.running = Some(Running {
            control,
            root,
            started: Instant::now(),
            rx,
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(running) = &self.running {
            match running.rx.try_recv() {
                Ok(Ok(data)) => {
                    self.running = None;
                    self.install(data);
                }
                Ok(Err(error)) => {
                    self.running = None;
                    self.error = Some(error);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(33));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.running = None;
                    self.error = Some("scan worker stopped unexpectedly".into());
                }
            }
        }
        if let Some(rx) = &self.search_rx
            && let Ok(result) = rx.try_recv()
        {
            self.search_results = Some(result);
            self.search_rx = None;
        }
    }

    fn install(&mut self, data: Data) {
        self.expanded.clear();
        self.expanded.insert(0);
        // Open the largest top-level folder so the tree is useful immediately.
        if let Some(&first) = data.snapshot.children(0).first()
            && data.snapshot.entries[first as usize].kind == EntryKind::Directory
        {
            self.expanded.insert(first as usize);
        }
        self.selected = None;
        self.zoom = 0;
        self.generation += 1;
        self.rows_dirty = true;
        self.search_results = None;
        self.data = Some(data);
    }

    fn size(&self, snapshot: &Snapshot, id: usize) -> u64 {
        treemap::size_of(snapshot, id, self.allocated)
    }

    fn sorted_children(&self, snapshot: &Snapshot, id: usize) -> Vec<u32> {
        let mut children = snapshot.children(id).to_vec();
        if self.allocated {
            children.sort_by_key(|&c| std::cmp::Reverse(self.size(snapshot, c as usize)));
        }
        children
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(data) = &self.data else { return };
        let snapshot = data.snapshot.clone();
        let mut stack = vec![(0_u32, 0_u16)];
        while let Some((id, depth)) = stack.pop() {
            self.rows.push((id, depth));
            if self.expanded.contains(&(id as usize)) {
                for child in self
                    .sorted_children(&snapshot, id as usize)
                    .into_iter()
                    .rev()
                {
                    stack.push((child, depth + 1));
                }
            }
        }
        self.rows_dirty = false;
    }

    fn apply(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Select(id) => self.selected = Some(id),
                Action::Toggle(id) => {
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                    self.rows_dirty = true;
                }
                Action::Zoom(id) => {
                    let Some(data) = &self.data else { continue };
                    let entry = &data.snapshot.entries[id];
                    self.zoom = if entry.kind == EntryKind::Directory {
                        id
                    } else {
                        entry.parent_id.unwrap_or(0)
                    };
                    self.tab = Tab::Map;
                }
                Action::Reveal(id) => {
                    let Some(data) = &self.data else { continue };
                    let mut cursor = data.snapshot.entries[id].parent_id;
                    while let Some(parent) = cursor {
                        self.expanded.insert(parent);
                        cursor = data.snapshot.entries[parent].parent_id;
                    }
                    self.selected = Some(id);
                    self.rows_dirty = true;
                    self.scroll_to_selected = true;
                }
                Action::Copy(text, what) => {
                    ctx.copy_text(text);
                    self.toast = Some((format!("Copied {what}"), ctx.input(|i| i.time)));
                }
                Action::Tab(tab) => self.tab = tab,
                Action::Scan(path) => {
                    self.path = path;
                    self.start_scan(ctx);
                }
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let mut actions = Vec::new();
        if ctx.input(|i| i.key_pressed(Key::Escape))
            && let Some(running) = &self.running
        {
            running.control.cancel();
        }
        if ctx.input(|i| i.key_pressed(Key::F5)) {
            self.start_scan(ctx);
        }
        let typing = ctx.egui_wants_keyboard_input();
        let Some(data) = &self.data else {
            return actions;
        };
        if typing {
            return actions;
        }
        let snapshot = &data.snapshot;
        let back = ctx.input(|i| {
            i.key_pressed(Key::Backspace) || i.pointer.button_pressed(egui::PointerButton::Extra1)
        });
        if back && let Some(parent) = snapshot.entries[self.zoom].parent_id {
            actions.push(Action::Select(self.zoom));
            self.zoom = parent;
        }
        let (up, down, left, right, enter) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Enter),
            )
        });
        let position = self
            .selected
            .and_then(|s| self.rows.iter().position(|r| r.0 as usize == s));
        if up || down {
            let next = match position {
                None => 0,
                Some(p) if up => p.saturating_sub(1),
                Some(p) => (p + 1).min(self.rows.len().saturating_sub(1)),
            };
            if let Some(row) = self.rows.get(next) {
                actions.push(Action::Select(row.0 as usize));
                self.scroll_to_selected = true;
            }
        }
        if let Some(id) = self.selected {
            let is_dir = snapshot.entries[id].kind == EntryKind::Directory;
            if right && is_dir && !self.expanded.contains(&id) {
                actions.push(Action::Toggle(id));
            }
            if left {
                if is_dir && self.expanded.contains(&id) && id != 0 {
                    actions.push(Action::Toggle(id));
                } else if let Some(parent) = snapshot.entries[id].parent_id {
                    actions.push(Action::Select(parent));
                    self.scroll_to_selected = true;
                }
            }
            if enter {
                actions.push(Action::Zoom(id));
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::C)) {
                actions.push(Action::Copy(
                    snapshot.path(id).display().to_string(),
                    "path",
                ));
            }
        }
        for (key, tab) in [
            (Key::Num1, Tab::Map),
            (Key::Num2, Tab::Reclaim),
            (Key::Num3, Tab::Largest),
            (Key::Num4, Tab::Types),
        ] {
            if ctx.input(|i| i.key_pressed(key)) {
                actions.push(Action::Tab(tab));
            }
        }
        actions
    }

    // ---------------------------------------------------------------- top bar

    fn top_bar(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            brand(ui);
            ui.add_space(6.0);
            for volume in &self.volumes {
                let used = volume.total_bytes.saturating_sub(volume.free_bytes);
                let share = used as f32 / volume.total_bytes.max(1) as f32;
                let letter = volume.root.display().to_string();
                let letter = letter.trim_end_matches('\\').to_string();
                let (rect, response) = ui.allocate_exact_size(vec2(84.0, 30.0), Sense::click());
                let hovered = response.hovered();
                let p = ui.painter();
                p.rect_filled(rect, 6, if hovered { style::SURFACE_HI } else { style::SURFACE });
                p.rect_stroke(rect, 6, Stroke::new(1.0, if hovered { style::alpha(style::ACCENT, 160) } else { style::BORDER }), StrokeKind::Inside);
                p.text(rect.left_top() + vec2(9.0, 5.0), Align2::LEFT_TOP, &letter, FontId::proportional(13.0), style::TEXT);
                p.text(rect.right_top() + vec2(-8.0, 6.0), Align2::RIGHT_TOP, format!("{:.0}%", share * 100.0), FontId::proportional(11.0), style::DIM);
                let bar = Rect::from_min_size(rect.left_bottom() + vec2(9.0, -8.0), vec2(rect.width() - 18.0, 3.0));
                p.rect_filled(bar, 2, style::BORDER);
                let fill = if share > 0.9 { style::DANGER } else if share > 0.75 { style::WARN } else { style::ACCENT };
                p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * share, 3.0)), 2, fill);
                let label = if volume.label.is_empty() { "Local disk".to_string() } else { volume.label.clone() };
                if response
                    .on_hover_text(format!(
                        "{label} ({letter}) · {}\n{} used of {} · {} free\nClick to scan",
                        volume.filesystem,
                        style::bytes(used),
                        style::bytes(volume.total_bytes),
                        style::bytes(volume.free_bytes)
                    ))
                    .clicked()
                {
                    actions.push(Action::Scan(volume.root.display().to_string()));
                }
            }
            ui.separator();
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .hint_text("Folder or drive to scan")
                    .desired_width(320.0)
                    .margin(Margin::symmetric(8, 5)),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                actions.push(Action::Scan(self.path.clone()));
            }
            if (ui.button("Browse…").on_hover_text("Pick a folder (Ctrl+O)").clicked()
                || ui.input(|i| i.modifiers.command && i.key_pressed(Key::O)))
                && let Some(folder) = rfd::FileDialog::new().set_directory(&self.path).pick_folder()
            {
                actions.push(Action::Scan(folder.display().to_string()));
            }
            if let Some(running) = &self.running {
                if ui
                    .add(egui::Button::new(RichText::new("■  Cancel").color(Color32::WHITE)).fill(style::scale(style::DANGER, 0.7)))
                    .on_hover_text("Stop and keep what was found (Esc)")
                    .clicked()
                {
                    running.control.cancel();
                }
            } else {
                let label = if self.data.is_some() { "⟳  Rescan" } else { "Scan" };
                if ui
                    .add(egui::Button::new(RichText::new(label).color(Color32::WHITE).strong()).fill(style::ACCENT_DEEP))
                    .on_hover_text("F5")
                    .clicked()
                {
                    actions.push(Action::Scan(self.path.clone()));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                segmented(ui, &mut self.allocated, [("On disk", true), ("Size", false)], "Size: logical file length.\nOn disk: allocated clusters; hard links counted once.");
                if self.data.is_some() {
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Find by name  (Ctrl+F)")
                            .desired_width(180.0)
                            .margin(Margin::symmetric(8, 5)),
                    );
                    if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        self.run_search();
                        actions.push(Action::Tab(Tab::Search));
                    }
                    if ui.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
                        search.request_focus();
                    }
                }
            });
        });
        actions
    }

    pub fn run_search(&mut self) {
        let Some(data) = &self.data else { return };
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        let snapshot = data.snapshot.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut hits: Vec<usize> = (1..snapshot.entries.len())
                .filter(|&id| snapshot.name(id).to_lowercase().contains(&query))
                .collect();
            hits.sort_by_key(|&id| std::cmp::Reverse(snapshot.entries[id].logical_bytes));
            hits.truncate(1000);
            let _ = tx.send((query, hits));
        });
        self.search_rx = Some(rx);
    }

    // ------------------------------------------------------------- status bar

    fn status_bar(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            if let Some(running) = &self.running {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(RichText::new(format!("Scanning {}", running.root)).color(style::DIM));
            } else if let Some(data) = &self.data {
                let s = &data.snapshot.summary;
                let (text, color) = if s.complete {
                    ("● Complete".to_string(), style::GOOD)
                } else if s.cancelled {
                    ("● Cancelled · partial".to_string(), style::WARN)
                } else {
                    (
                        format!("● Partial · {} issues", style::count(s.issue_count)),
                        style::WARN,
                    )
                };
                let chip = ui.label(RichText::new(text).color(color).strong());
                if !s.complete
                    && chip
                        .on_hover_text("Some folders could not be read. See Issues.")
                        .clicked()
                {
                    actions.push(Action::Tab(Tab::Issues));
                }
                ui.label(format!("{} files", style::count(s.files)));
                ui.label(format!("{} folders", style::count(s.directories)));
                ui.label(
                    RichText::new(format!("{:.2} s", s.elapsed_ms as f64 / 1000.0))
                        .color(style::DIM),
                )
                .on_hover_text(format!(
                    "Scan {} ms · analysis {} ms · backend {}",
                    s.elapsed_ms, data.analysis_ms, s.backend
                ));
                ui.separator();
                ui.label(
                    RichText::new(format!("Size {}", style::bytes(s.logical_bytes)))
                        .color(style::DIM),
                );
                ui.label(
                    RichText::new(format!("Unique {}", style::bytes(s.unique_logical_bytes)))
                        .color(style::DIM),
                )
                .on_hover_text("Hard-linked files counted once");
                ui.label(
                    RichText::new(format!(
                        "On disk {}",
                        s.allocated_bytes
                            .map(style::bytes)
                            .unwrap_or("unknown".into())
                    ))
                    .color(style::DIM),
                );
            } else {
                ui.label(
                    RichText::new("Read-only · metadata only · nothing leaves this PC")
                        .color(style::FAINT),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(concat!(
                        "RillDisk ",
                        env!("CARGO_PKG_VERSION"),
                        " · AGPL-3.0"
                    ))
                    .color(style::FAINT)
                    .small(),
                );
                if let (Some(id), Some(data)) = (self.selected, &self.data) {
                    let path = data.snapshot.path(id).display().to_string();
                    if ui.small_button("Copy path").clicked() {
                        actions.push(Action::Copy(path.clone(), "path"));
                    }
                    ui.add(
                        egui::Label::new(RichText::new(path).color(style::DIM).monospace())
                            .truncate(),
                    );
                }
            });
        });
        actions
    }

    // ------------------------------------------------------------------- tree

    fn tree(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.rows_dirty {
            self.rebuild_rows();
        }
        let Some(data) = &self.data else {
            return actions;
        };
        let snapshot = data.snapshot.clone();
        let now = now_ms();
        let columns = [
            ("Size", 78.0),
            ("%", 92.0),
            ("Files", 66.0),
            ("Changed", 62.0),
        ];
        let columns_width: f32 = columns.iter().map(|c| c.1).sum();
        // Header
        let (header, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
        let p = ui.painter();
        p.text(
            header.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            "Name",
            FontId::proportional(12.0),
            style::FAINT,
        );
        let mut x = header.right() - 8.0;
        for (title, width) in columns.iter().rev() {
            p.text(
                pos2(x, header.center().y),
                Align2::RIGHT_CENTER,
                *title,
                FontId::proportional(12.0),
                style::FAINT,
            );
            x -= width;
        }
        p.line_segment(
            [header.left_bottom(), header.right_bottom()],
            Stroke::new(1.0, style::BORDER),
        );

        let selected_row = self
            .selected
            .and_then(|s| self.rows.iter().position(|r| r.0 as usize == s));
        let mut area = egui::ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("tree");
        if self.scroll_to_selected
            && let Some(row) = selected_row
        {
            let row_h = ROW_H + ui.spacing().item_spacing.y * 0.0;
            let viewport = ui.available_height();
            area = area.vertical_scroll_offset((row as f32 * row_h - viewport * 0.4).max(0.0));
            self.scroll_to_selected = false;
        }
        let rows = &self.rows;
        let expanded = &self.expanded;
        let allocated = self.allocated;
        let selected = self.selected;
        let candidate_of = &data.candidate_of;
        area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for &(id, depth) in &rows[range] {
                let id = id as usize;
                let entry = &snapshot.entries[id];
                let size = treemap::size_of(&snapshot, id, allocated);
                let parent_size = entry
                    .parent_id
                    .map_or(size, |p| treemap::size_of(&snapshot, p, allocated))
                    .max(1);
                let share = size as f32 / parent_size as f32;
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                let p = ui.painter();
                let is_selected = selected == Some(id);
                if is_selected {
                    p.rect_filled(rect, 0, style::alpha(style::ACCENT, 46));
                } else if response.hovered() {
                    p.rect_filled(rect, 0, style::SURFACE_HI);
                }
                let indent = rect.left() + 8.0 + depth as f32 * 14.0;
                let name_right = rect.right() - columns_width - 8.0;
                let is_dir = entry.kind == EntryKind::Directory;
                let has_children = !snapshot.children(id).is_empty();
                let color = if is_dir {
                    Color32::from_rgb(125, 211, 252)
                } else {
                    FileClass::of(&style::extension_of(&snapshot.name(id))).color()
                };
                if is_dir && has_children {
                    let arrow = if expanded.contains(&id) { "▾" } else { "▸" };
                    p.text(
                        pos2(indent + 4.0, rect.center().y),
                        Align2::CENTER_CENTER,
                        arrow,
                        FontId::proportional(12.0),
                        style::DIM,
                    );
                }
                let icon =
                    Rect::from_center_size(pos2(indent + 21.0, rect.center().y), vec2(9.0, 9.0));
                if is_dir {
                    p.rect_stroke(icon, 2, Stroke::new(1.5, color), StrokeKind::Inside);
                } else if entry.kind == EntryKind::Link {
                    p.circle_stroke(icon.center(), 4.0, Stroke::new(1.2, style::FAINT));
                } else {
                    p.rect_filled(icon, 2, color);
                }
                let name = snapshot.name(id);
                let mut name_rect = Rect::from_min_max(
                    pos2(indent + 31.0, rect.top()),
                    pos2(name_right, rect.bottom()),
                );
                if let Some(&c) = candidate_of.get(&id) {
                    let candidate = &data.reclaim.candidates[c];
                    let badge_color = style::safety_color(candidate.safety);
                    let galley = p.layout_no_wrap(
                        candidate.label.to_string(),
                        FontId::proportional(10.5),
                        badge_color,
                    );
                    let badge = Rect::from_min_size(
                        pos2(name_right - galley.size().x - 10.0, rect.center().y - 8.0),
                        vec2(galley.size().x + 10.0, 16.0),
                    );
                    if badge.left() > name_rect.left() + 60.0 {
                        p.rect_filled(badge, 8, style::alpha(badge_color, 34));
                        p.galley(
                            badge.left_top() + vec2(5.0, (16.0 - galley.size().y) / 2.0),
                            galley,
                            badge_color,
                        );
                        name_rect.max.x = badge.left() - 6.0;
                    }
                }
                let text_color = if entry.hard_link_duplicate || entry.kind == EntryKind::Link {
                    style::FAINT
                } else {
                    style::TEXT
                };
                p.with_clip_rect(name_rect).text(
                    name_rect.left_center(),
                    Align2::LEFT_CENTER,
                    name,
                    FontId::proportional(13.0),
                    text_color,
                );
                let mut x = rect.right() - 8.0;
                let cells = [
                    style::bytes(size),
                    if entry.parent_id.is_some() {
                        format!("{:.1}", share * 100.0)
                    } else {
                        "100".into()
                    },
                    if is_dir {
                        style::count(entry.file_count)
                    } else {
                        String::new()
                    },
                    style::age(now, entry.modified_unix_ms),
                ];
                // Share of parent as a compact bar in the % column.
                let track = Rect::from_min_size(
                    pos2(
                        rect.right() - 8.0 - 62.0 - 66.0 - 88.0,
                        rect.center().y - 3.0,
                    ),
                    vec2(44.0, 6.0),
                );
                p.rect_filled(track, 3, style::SURFACE_HI);
                p.rect_filled(
                    Rect::from_min_size(
                        track.min,
                        vec2(track.width() * share.clamp(0.0, 1.0), 6.0),
                    ),
                    3,
                    style::alpha(color, 200),
                );
                for ((_, width), text) in columns.iter().zip(cells.iter()).rev() {
                    let color = if *width == 78.0 {
                        style::TEXT
                    } else {
                        style::DIM
                    };
                    p.text(
                        pos2(x, rect.center().y),
                        Align2::RIGHT_CENTER,
                        text,
                        FontId::proportional(12.5),
                        color,
                    );
                    x -= width;
                }
                let pointer_x = response.interact_pointer_pos().map(|p| p.x);
                if response.clicked() {
                    if is_dir && has_children && pointer_x.is_some_and(|x| x < indent + 14.0) {
                        actions.push(Action::Toggle(id));
                    } else {
                        actions.push(Action::Select(id));
                    }
                }
                if response.double_clicked() {
                    if is_dir {
                        actions.push(Action::Toggle(id));
                    }
                    actions.push(Action::Zoom(id));
                }
                response.context_menu(|ui| {
                    entry_menu(ui, &snapshot, id, &mut actions);
                });
            }
        });
        actions
    }

    // ---------------------------------------------------------------- central

    fn central(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(error) = &self.error {
            Frame::new()
                .fill(style::alpha(style::DANGER, 30))
                .stroke(Stroke::new(1.0, style::alpha(style::DANGER, 120)))
                .corner_radius(8)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("Scan failed: {error}")).color(style::DANGER));
                });
            ui.add_space(8.0);
        }
        if let Some(running) = &self.running {
            scanning_card(ui, running);
            return actions;
        }
        if self.data.is_none() {
            return self.welcome(ui);
        }
        let data = self.data.as_ref().unwrap();
        let reclaim_count = data.reclaim.candidates.len();
        let issues = data.snapshot.summary.issue_count;
        ui.horizontal(|ui| {
            let tabs = [
                (Tab::Map, "Map".to_string()),
                (
                    Tab::Reclaim,
                    format!(
                        "Reclaim  {}",
                        style::bytes(frees_total(&data.reclaim, self.allocated))
                    ),
                ),
                (Tab::Largest, "Largest files".to_string()),
                (Tab::Types, "File types".to_string()),
            ];
            for (tab, label) in tabs {
                if tab_button(
                    ui,
                    self.tab == tab,
                    &label,
                    tab == Tab::Reclaim && reclaim_count > 0,
                ) {
                    actions.push(Action::Tab(tab));
                }
            }
            if issues > 0
                && tab_button(
                    ui,
                    self.tab == Tab::Issues,
                    &format!("Issues  {}", style::count(issues)),
                    false,
                )
            {
                actions.push(Action::Tab(Tab::Issues));
            }
            if self.search_results.is_some()
                && tab_button(ui, self.tab == Tab::Search, "Search", false)
            {
                actions.push(Action::Tab(Tab::Search));
            }
        });
        ui.add_space(6.0);
        match self.tab {
            Tab::Map => actions.extend(self.map(ui)),
            Tab::Reclaim => actions.extend(self.reclaim(ui)),
            Tab::Largest => actions.extend(self.largest(ui)),
            Tab::Types => self.types(ui),
            Tab::Issues => self.issues(ui),
            Tab::Search => actions.extend(self.search_tab(ui)),
        }
        actions
    }

    fn map(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let data = self.data.as_ref().unwrap();
        let snapshot = data.snapshot.clone();
        // Breadcrumb
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let mut chain = vec![self.zoom];
            let mut cursor = snapshot.entries[self.zoom].parent_id;
            while let Some(p) = cursor {
                chain.push(p);
                cursor = snapshot.entries[p].parent_id;
            }
            if ui
                .add_enabled(self.zoom != 0, egui::Button::new("⬆").small())
                .on_hover_text("Up one level (Backspace)")
                .clicked()
            {
                actions.push(Action::Zoom(
                    snapshot.entries[self.zoom].parent_id.unwrap_or(0),
                ));
            }
            for (i, &id) in chain.iter().rev().enumerate() {
                if i > 0 {
                    ui.label(RichText::new("›").color(style::FAINT));
                }
                let last = id == self.zoom;
                let text = RichText::new(snapshot.name(id)).color(if last {
                    style::TEXT
                } else {
                    style::DIM
                });
                if ui
                    .add(
                        egui::Label::new(if last { text.strong() } else { text })
                            .sense(Sense::click()),
                    )
                    .clicked()
                {
                    actions.push(Action::Zoom(id));
                }
            }
            ui.label(
                RichText::new(format!(
                    "  {}",
                    style::bytes(treemap::size_of(&snapshot, self.zoom, self.allocated))
                ))
                .color(style::FAINT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.toggle_value(&mut self.highlight_reclaim, "Highlight reclaimable")
                    .on_hover_text("Outline worktrees, build output, dependencies and caches");
            });
        });
        let legend_h = 26.0;
        let mut rect = ui.available_rect_before_wrap();
        rect.max.y -= legend_h;
        let response = ui.allocate_rect(rect, Sense::click());
        let key = (
            self.zoom,
            self.allocated,
            [
                rect.min.x as i32,
                rect.min.y as i32,
                rect.max.x as i32,
                rect.max.y as i32,
            ],
            self.generation,
        );
        if self.layout.key != Some(key) {
            self.layout =
                treemap::build(&snapshot, self.zoom, rect, self.allocated, self.generation);
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6, style::BG);
        painter.add(Shape::mesh(self.layout.mesh.clone()));
        // Labels
        let label_font = FontId::proportional(11.5);
        for tile in &self.layout.tiles {
            let r = tile.rect;
            match tile.kind {
                TileKind::Dir if r.height() >= 44.0 && r.width() >= 64.0 => {
                    let header =
                        Rect::from_min_size(r.min + vec2(4.0, 1.0), vec2(r.width() - 8.0, 15.0));
                    let (top, end) = (tile.id as usize, tile.chain as usize);
                    let name = if top == end {
                        snapshot.name(top).into_owned()
                    } else if snapshot.entries[end].parent_id == Some(top) {
                        format!("{} › {}", snapshot.name(top), snapshot.name(end))
                    } else {
                        format!("{} › … › {}", snapshot.name(top), snapshot.name(end))
                    };
                    let text = format!(
                        "{}  {}",
                        name,
                        style::bytes(treemap::size_of(
                            &snapshot,
                            tile.id as usize,
                            self.allocated
                        ))
                    );
                    painter.with_clip_rect(header.intersect(rect)).text(
                        header.left_center(),
                        Align2::LEFT_CENTER,
                        text,
                        label_font.clone(),
                        style::DIM,
                    );
                }
                TileKind::File(_) if r.width() >= 72.0 && r.height() >= 30.0 => {
                    let inner = r.shrink(4.0);
                    let clip = painter.with_clip_rect(inner.intersect(rect));
                    let ink = Color32::from_rgba_unmultiplied(8, 10, 14, 225);
                    clip.text(
                        inner.left_top(),
                        Align2::LEFT_TOP,
                        snapshot.name(tile.id as usize),
                        label_font.clone(),
                        ink,
                    );
                    clip.text(
                        inner.left_top() + vec2(0.0, 14.0),
                        Align2::LEFT_TOP,
                        style::bytes(treemap::size_of(
                            &snapshot,
                            tile.id as usize,
                            self.allocated,
                        )),
                        FontId::proportional(10.5),
                        Color32::from_rgba_unmultiplied(8, 10, 14, 170),
                    );
                }
                _ => {}
            }
        }
        if self.highlight_reclaim {
            for tile in &self.layout.tiles {
                if let Some(&c) = data.candidate_of.get(&(tile.id as usize)) {
                    let color = style::safety_color(data.reclaim.candidates[c].safety);
                    painter.rect_filled(tile.rect, 2, style::alpha(color, 28));
                    painter.rect_stroke(tile.rect, 2, Stroke::new(2.0, color), StrokeKind::Inside);
                }
            }
        }
        if let Some(selected) = self.selected
            && let Some(tile) = self.layout.tile_of(selected)
        {
            painter.rect_stroke(
                tile.rect,
                2,
                Stroke::new(2.5, Color32::WHITE),
                StrokeKind::Inside,
            );
        }
        if self.layout.tiles.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Nothing with a size to show here",
                FontId::proportional(14.0),
                style::FAINT,
            );
        }
        if let Some(pos) = response.hover_pos()
            && let Some(tile) = self.layout.hit(pos).copied()
        {
            let id = tile.id as usize;
            painter.rect_stroke(
                tile.rect,
                2,
                Stroke::new(1.5, style::alpha(Color32::WHITE, 200)),
                StrokeKind::Inside,
            );
            let entry = &snapshot.entries[id];
            let size = treemap::size_of(&snapshot, id, self.allocated);
            let root_size = treemap::size_of(&snapshot, self.zoom, self.allocated).max(1);
            let candidate = data
                .candidate_of
                .get(&id)
                .map(|&c| &data.reclaim.candidates[c]);
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(420.0);
                ui.label(RichText::new(snapshot.name(id)).strong().size(14.0));
                ui.label(
                    RichText::new(snapshot.path(id).display().to_string())
                        .color(style::DIM)
                        .small(),
                );
                ui.add_space(4.0);
                ui.label(format!(
                    "{}  ·  {:.1}% of view{}",
                    style::bytes(size),
                    size as f64 * 100.0 / root_size as f64,
                    if entry.kind == EntryKind::Directory {
                        format!("  ·  {} files", style::count(entry.file_count))
                    } else {
                        String::new()
                    }
                ));
                ui.label(
                    RichText::new(format!(
                        "Changed {}",
                        style::age(now_ms(), entry.modified_unix_ms)
                    ))
                    .color(style::DIM),
                );
                if let Some(c) = candidate {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("{} · {}", c.label, c.safety.label()))
                            .color(style::safety_color(c.safety))
                            .strong(),
                    );
                }
                ui.label(
                    RichText::new("Double-click to zoom · right-click for actions")
                        .color(style::FAINT)
                        .small(),
                );
            });
            if response.clicked() {
                actions.push(Action::Reveal(id));
            }
            if response.double_clicked() {
                actions.push(Action::Zoom(id));
            }
            response.context_menu(|ui| entry_menu(ui, &snapshot, id, &mut actions));
        }
        // Legend
        let legend = Rect::from_min_size(
            pos2(rect.left(), rect.bottom()),
            vec2(rect.width(), legend_h),
        );
        let mut ui_legend = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(legend)
                .layout(Layout::left_to_right(Align::Center)),
        );
        for (class, logical, allocated) in data.classes.iter().take(9) {
            let size = if self.allocated { *allocated } else { *logical };
            let (dot, _) = ui_legend.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui_legend.painter().rect_filled(dot, 2, class.color());
            ui_legend.label(
                RichText::new(format!("{} {}", class.label(), style::bytes(size)))
                    .color(style::DIM)
                    .small(),
            );
            ui_legend.add_space(8.0);
        }
        if self.layout.truncated {
            ui_legend.label(
                RichText::new("· detail limited for speed; zoom in for more")
                    .color(style::FAINT)
                    .small(),
            );
        }
        actions
    }

    fn reclaim(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let data = self.data.as_ref().unwrap();
        let report = &data.reclaim;
        let size_of = |c: &rilldisk_core::Candidate| {
            if self.allocated {
                c.allocated_bytes.unwrap_or(0)
            } else {
                c.logical_bytes
            }
        };
        Frame::new()
            .fill(style::SURFACE)
            .stroke(Stroke::new(1.0, style::BORDER))
            .corner_radius(10)
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let outer = report.candidates.iter().filter(|c| c.nested_in.is_none());
                        let total: u64 = outer.clone().map(size_of).sum();
                        let shared: u64 = outer.map(|c| c.shared_bytes).sum();
                        ui.label(
                            RichText::new(style::bytes(frees_total(report, self.allocated)))
                                .size(30.0)
                                .strong()
                                .color(style::TEXT),
                        );
                        ui.label(
                            RichText::new(format!(
                                "can be freed, at least, across {} folders",
                                report.candidates.len()
                            ))
                            .color(style::DIM),
                        );
                        let mut detail = format!("{} total size", style::bytes(total));
                        if shared > 0 {
                            detail.push_str(&format!(
                                " · {} shared through hard links",
                                style::bytes(shared)
                            ));
                        }
                        ui.label(RichText::new(detail).color(style::FAINT).small())
                            .on_hover_text("Hard-linked files (for example from a pnpm store) also live elsewhere. Removing one folder does not free them.");
                    });
                    ui.add_space(24.0);
                    for safety in [Safety::Regenerable, Safety::Cache, Safety::Review] {
                        let (n, bytes) = report
                            .candidates
                            .iter()
                            .filter(|c| c.safety == safety && c.nested_in.is_none())
                            .fold((0, 0), |(n, b), c| (n + 1, b + frees(c, self.allocated)));
                        let active = self.reclaim_filter == Some(safety);
                        let color = style::safety_color(safety);
                        let text = RichText::new(format!(
                            "{}\n{} · {}",
                            safety.label(),
                            style::bytes(bytes),
                            n
                        ))
                        .color(color);
                        let button = egui::Button::new(text)
                            .fill(if active {
                                style::alpha(color, 40)
                            } else {
                                style::SURFACE_HI
                            })
                            .stroke(Stroke::new(1.0, if active { color } else { style::BORDER }))
                            .min_size(vec2(128.0, 44.0));
                        if ui.add(button).on_hover_text(safety_help(safety)).clicked() {
                            self.reclaim_filter = if active { None } else { Some(safety) };
                        }
                    }
                });
            });
        ui.add_space(4.0);
        ui.label(RichText::new("RillDisk never deletes. Each suggestion is based on folder names and marker files only. Use the suggested command; Git refuses to remove a worktree with uncommitted changes.").color(style::FAINT).small());
        ui.add_space(6.0);
        let now = now_ms();
        let snapshot = data.snapshot.clone();
        egui::ScrollArea::vertical().auto_shrink(false).id_salt("reclaim").show(ui, |ui| {
            for candidate in report.candidates.iter().filter(|c| self.reclaim_filter.is_none_or(|s| s == c.safety)) {
                let color = style::safety_color(candidate.safety);
                let response = Frame::new()
                    .fill(style::SURFACE)
                    .stroke(Stroke::new(1.0, if self.selected == Some(candidate.entry_id) { style::alpha(style::ACCENT, 180) } else { style::BORDER }))
                    .corner_radius(8)
                    .inner_margin(Margin { left: 14, right: 12, top: 9, bottom: 9 })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(candidate.label).strong().size(14.0));
                                    pill(ui, candidate.safety.label(), color);
                                    pill(ui, candidate.category.label(), style::DIM);
                                    if candidate.nested_in.is_some() {
                                        pill(ui, "inside another candidate", style::FAINT);
                                    }
                                });
                                ui.add(egui::Label::new(RichText::new(candidate.path.display().to_string()).monospace().color(style::DIM)).truncate());
                                ui.label(RichText::new(&candidate.reason).color(style::FAINT).small());
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.button("Copy command").on_hover_text(&candidate.suggested_action).clicked() {
                                    actions.push(Action::Copy(candidate.suggested_action.clone(), "command"));
                                }
                                if ui.button("Copy path").clicked() {
                                    actions.push(Action::Copy(candidate.path.display().to_string(), "path"));
                                }
                                if ui.button("Show").on_hover_text("Reveal in tree and map").clicked() {
                                    actions.push(Action::Reveal(candidate.entry_id));
                                    actions.push(Action::Zoom(snapshot.entries[candidate.entry_id].parent_id.unwrap_or(0)));
                                    self.highlight_reclaim = true;
                                }
                                ui.add_space(8.0);
                                ui.vertical(|ui| {
                                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                                        ui.label(RichText::new(style::bytes(size_of(candidate))).strong().size(16.0));
                                        let idle = candidate.idle_days.map_or("—".into(), |d| if d == 0 { "active today".into() } else { format!("idle {d} d") });
                                        let _ = now;
                                        ui.label(RichText::new(format!("{} · {} files", idle, style::count(candidate.file_count))).color(style::FAINT).small());
                                        if candidate.shared_bytes > 0 {
                                            ui.label(RichText::new(format!("frees ≥ {} · {} hard-linked", style::bytes(frees(candidate, self.allocated)), style::bytes(candidate.shared_bytes))).color(style::WARN).small())
                                                .on_hover_text("These files also exist under another name (hard link). Removing this folder frees only the rest.");
                                        }
                                    });
                                });
                            });
                        });
                    })
                    .response;
                let stripe = Rect::from_min_size(response.rect.min + vec2(0.0, 6.0), vec2(3.0, response.rect.height() - 12.0));
                ui.painter().rect_filled(stripe, CornerRadius::same(2), color);
                ui.add_space(4.0);
            }
            if report.candidates.is_empty() {
                ui.add_space(30.0);
                ui.label(RichText::new("No worktrees, build output, dependency folders or known caches found here.").color(style::DIM));
            }
        });
        actions
    }

    fn largest(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let data = self.data.as_ref().unwrap();
        let snapshot = data.snapshot.clone();
        let ids = &data.largest[usize::from(self.allocated)];
        let top = ids
            .first()
            .map_or(1, |&id| treemap::size_of(&snapshot, id, self.allocated))
            .max(1);
        let now = now_ms();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("largest")
            .show_rows(ui, 34.0, ids.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for &id in &ids[range] {
                    let size = treemap::size_of(&snapshot, id, self.allocated);
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                    let p = ui.painter();
                    if self.selected == Some(id) {
                        p.rect_filled(rect, 4, style::alpha(style::ACCENT, 46));
                    } else if response.hovered() {
                        p.rect_filled(rect, 4, style::SURFACE_HI);
                    }
                    let class = FileClass::of(&style::extension_of(&snapshot.name(id)));
                    let bar = Rect::from_min_size(
                        rect.left_bottom() + vec2(10.0, -5.0),
                        vec2((rect.width() - 200.0) * size as f32 / top as f32, 2.0),
                    );
                    p.rect_filled(bar, 1, style::alpha(class.color(), 160));
                    p.rect_filled(
                        Rect::from_center_size(
                            pos2(rect.left() + 14.0, rect.top() + 12.0),
                            vec2(9.0, 9.0),
                        ),
                        2,
                        class.color(),
                    );
                    let text = Rect::from_min_max(
                        rect.min + vec2(26.0, 2.0),
                        pos2(rect.right() - 190.0, rect.bottom()),
                    );
                    let clip = p.with_clip_rect(text);
                    clip.text(
                        text.left_top(),
                        Align2::LEFT_TOP,
                        snapshot.name(id),
                        FontId::proportional(13.0),
                        style::TEXT,
                    );
                    let parent = snapshot.entries[id]
                        .parent_id
                        .map(|p| snapshot.path(p).display().to_string())
                        .unwrap_or_default();
                    clip.text(
                        text.left_top() + vec2(0.0, 16.0),
                        Align2::LEFT_TOP,
                        parent,
                        FontId::proportional(11.0),
                        style::FAINT,
                    );
                    p.text(
                        rect.right_center() + vec2(-80.0, 0.0),
                        Align2::RIGHT_CENTER,
                        style::bytes(size),
                        FontId::proportional(13.0),
                        style::TEXT,
                    );
                    p.text(
                        rect.right_center() + vec2(-10.0, 0.0),
                        Align2::RIGHT_CENTER,
                        style::age(now, snapshot.entries[id].modified_unix_ms),
                        FontId::proportional(12.0),
                        style::DIM,
                    );
                    if response.clicked() {
                        actions.push(Action::Reveal(id));
                    }
                    if response.double_clicked() {
                        actions.push(Action::Zoom(id));
                    }
                    response.context_menu(|ui| entry_menu(ui, &snapshot, id, &mut actions));
                }
            });
        actions
    }

    fn types(&mut self, ui: &mut Ui) {
        let data = self.data.as_ref().unwrap();
        let total: u64 = data
            .types
            .iter()
            .map(|t| {
                if self.allocated {
                    t.allocated_bytes.unwrap_or(0)
                } else {
                    t.logical_bytes
                }
            })
            .sum::<u64>()
            .max(1);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("types")
            .show(ui, |ui| {
                for t in data.types.iter().take(300) {
                    let size = if self.allocated {
                        t.allocated_bytes.unwrap_or(0)
                    } else {
                        t.logical_bytes
                    };
                    let class = FileClass::of(&t.extension);
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                    let p = ui.painter();
                    let share = size as f32 / total as f32;
                    p.rect_filled(
                        Rect::from_center_size(
                            pos2(rect.left() + 12.0, rect.center().y),
                            vec2(10.0, 10.0),
                        ),
                        2,
                        class.color(),
                    );
                    let name = if t.extension.is_empty() {
                        "(no extension)".to_string()
                    } else {
                        format!(".{}", t.extension)
                    };
                    p.text(
                        pos2(rect.left() + 26.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        name,
                        FontId::proportional(13.0),
                        style::TEXT,
                    );
                    p.text(
                        pos2(rect.left() + 150.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        class.label(),
                        FontId::proportional(12.0),
                        style::FAINT,
                    );
                    let bar_area = Rect::from_min_max(
                        pos2(rect.left() + 260.0, rect.top() + 8.0),
                        pos2(rect.right() - 230.0, rect.bottom() - 8.0),
                    );
                    p.rect_filled(bar_area, 3, style::SURFACE);
                    p.rect_filled(
                        Rect::from_min_size(
                            bar_area.min,
                            vec2(bar_area.width() * share, bar_area.height()),
                        ),
                        3,
                        style::alpha(class.color(), 200),
                    );
                    p.text(
                        pos2(rect.right() - 150.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        plural(t.files, "file"),
                        FontId::proportional(12.0),
                        style::DIM,
                    );
                    p.text(
                        pos2(rect.right() - 60.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        style::bytes(size),
                        FontId::proportional(13.0),
                        style::TEXT,
                    );
                    p.text(
                        pos2(rect.right() - 8.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        format!("{:.1}%", share * 100.0),
                        FontId::proportional(12.0),
                        style::DIM,
                    );
                }
            });
    }

    fn issues(&mut self, ui: &mut Ui) {
        let data = self.data.as_ref().unwrap();
        let summary = &data.snapshot.summary;
        ui.label(RichText::new(format!(
            "{} folders or entries could not be read. Totals exclude them. The first {} are listed. Running without elevation is expected to hit protected system folders.",
            style::count(summary.issue_count),
            summary.issues.len()
        )).color(style::DIM));
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("issues")
            .show(ui, |ui| {
                for issue in &summary.issues {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⚠").color(style::WARN));
                        ui.add(
                            egui::Label::new(
                                RichText::new(issue.path.display().to_string()).monospace(),
                            )
                            .truncate(),
                        );
                    });
                    ui.label(RichText::new(&issue.message).color(style::FAINT).small());
                    ui.add_space(4.0);
                }
            });
    }

    fn search_tab(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let data = self.data.as_ref().unwrap();
        let snapshot = data.snapshot.clone();
        let Some((query, hits)) = &self.search_results else {
            ui.add(egui::Spinner::new());
            return actions;
        };
        ui.label(
            RichText::new(format!(
                "{} matches for “{}”{}",
                style::count(hits.len() as u64),
                query,
                if hits.len() >= 1000 {
                    " (first 1,000 by size)"
                } else {
                    ""
                }
            ))
            .color(style::DIM),
        );
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("search")
            .show_rows(ui, 24.0, hits.len(), |ui, range| {
                for &id in &hits[range] {
                    let response = ui
                        .horizontal(|ui| {
                            ui.set_height(22.0);
                            let selected = self.selected == Some(id);
                            ui.selectable_label(
                                selected,
                                RichText::new(format!(
                                    "{:>10}   {}",
                                    style::bytes(snapshot.entries[id].logical_bytes),
                                    snapshot.path(id).display()
                                ))
                                .monospace(),
                            )
                        })
                        .inner;
                    if response.clicked() {
                        actions.push(Action::Reveal(id));
                    }
                    if response.double_clicked() {
                        actions.push(Action::Zoom(id));
                    }
                }
            });
        actions
    }

    fn welcome(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.12).max(20.0));
            ui.label(RichText::new("See where your space went.").size(34.0).strong());
            ui.add_space(6.0);
            ui.label(RichText::new("Scan a drive or folder in seconds. RillDisk reads file metadata only — it never opens, uploads or deletes your files.").size(15.0).color(style::DIM));
            ui.add_space(28.0);
            let card_w = 230.0;
            let columns = ((ui.available_width() / (card_w + 16.0)) as usize).clamp(1, 4);
            let rows = self.volumes.chunks(columns);
            for row in rows {
                let width = row.len() as f32 * (card_w + 16.0);
                ui.allocate_ui_with_layout(vec2(width, 112.0), Layout::left_to_right(Align::Min), |ui| {
                    for volume in row {
                        let (rect, response) = ui.allocate_exact_size(vec2(card_w, 104.0), Sense::click());
                        let hovered = response.hovered();
                        let p = ui.painter();
                        p.rect_filled(rect, 12, if hovered { style::SURFACE_HI } else { style::SURFACE });
                        p.rect_stroke(rect, 12, Stroke::new(1.0, if hovered { style::ACCENT } else { style::BORDER }), StrokeKind::Inside);
                        let used = volume.total_bytes.saturating_sub(volume.free_bytes);
                        let share = used as f32 / volume.total_bytes.max(1) as f32;
                        let letter = volume.root.display().to_string();
                        p.text(rect.left_top() + vec2(16.0, 14.0), Align2::LEFT_TOP, letter.trim_end_matches('\\'), FontId::proportional(22.0), style::TEXT);
                        let label = if volume.label.is_empty() { "Local disk" } else { &volume.label };
                        p.text(rect.left_top() + vec2(56.0, 20.0), Align2::LEFT_TOP, format!("{label} · {}", volume.filesystem), FontId::proportional(12.5), style::DIM);
                        let bar = Rect::from_min_size(rect.left_top() + vec2(16.0, 56.0), vec2(card_w - 32.0, 8.0));
                        p.rect_filled(bar, 4, style::BG);
                        let fill = if share > 0.9 { style::DANGER } else if share > 0.75 { style::WARN } else { style::ACCENT };
                        p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * share, 8.0)), 4, fill);
                        p.text(rect.left_top() + vec2(16.0, 74.0), Align2::LEFT_TOP, format!("{} free of {}", style::bytes(volume.free_bytes), style::bytes(volume.total_bytes)), FontId::proportional(12.5), style::DIM);
                        if response.on_hover_text("Scan this drive").clicked() {
                            actions.push(Action::Scan(letter.clone()));
                        }
                        ui.add_space(16.0);
                    }
                });
            }
            ui.add_space(16.0);
            if ui.add(egui::Button::new(RichText::new("Scan a folder…").size(15.0)).min_size(vec2(180.0, 36.0))).clicked()
                && let Some(folder) = rfd::FileDialog::new().pick_folder()
            {
                actions.push(Action::Scan(folder.display().to_string()));
            }
            ui.add_space(36.0);
            ui.label(RichText::new("F5 rescan  ·  Esc cancel  ·  Enter zoom  ·  Backspace up  ·  Ctrl+F find  ·  1–4 switch views").color(style::FAINT).small());
            ui.label(RichText::new("Also available as a CLI and an MCP server for AI assistants: rilldisk reclaim · rilldisk mcp").color(style::FAINT).small());
        });
        actions
    }
}

/// Space removing a candidate frees at least: hard-linked files are excluded.
fn frees(candidate: &rilldisk_core::Candidate, allocated: bool) -> u64 {
    if allocated {
        candidate.exclusive_bytes
    } else {
        candidate
            .logical_bytes
            .saturating_sub(candidate.shared_bytes)
    }
}

fn frees_total(report: &ReclaimReport, allocated: bool) -> u64 {
    report
        .candidates
        .iter()
        .filter(|c| c.nested_in.is_none())
        .map(|c| frees(c, allocated))
        .sum()
}

fn plural(n: u64, word: &str) -> String {
    format!(
        "{} {word}{}",
        style::count(n),
        if n == 1 { "" } else { "s" }
    )
}

fn safety_help(safety: Safety) -> &'static str {
    match safety {
        Safety::Regenerable => {
            "Rebuilt or reinstalled by project tools from source (node_modules, target, .next, __pycache__)."
        }
        Safety::Cache => {
            "Re-downloaded or recreated on demand (package caches, temp, shader caches)."
        }
        Safety::Review => {
            "May hold unique work: Git worktrees, Recycle Bin, model downloads. Check first."
        }
    }
}

fn entry_menu(ui: &mut Ui, snapshot: &Snapshot, id: usize, actions: &mut Vec<Action>) {
    let entry = &snapshot.entries[id];
    if ui.button("Zoom map here").clicked() {
        actions.push(Action::Zoom(id));
        ui.close();
    }
    if ui.button("Show in tree").clicked() {
        actions.push(Action::Reveal(id));
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        actions.push(Action::Copy(
            snapshot.path(id).display().to_string(),
            "path",
        ));
        ui.close();
    }
    if ui.button("Copy name").clicked() {
        actions.push(Action::Copy(snapshot.name(id).into_owned(), "name"));
        ui.close();
    }
    if entry.kind == EntryKind::Directory && ui.button("Scan only this folder").clicked() {
        actions.push(Action::Scan(snapshot.path(id).display().to_string()));
        ui.close();
    }
}

fn brand(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 5, Color32::from_rgb(15, 23, 36));
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| {
        Rect::from_min_max(
            rect.min + vec2(x0, y0) * 22.0,
            rect.min + vec2(x1, y1) * 22.0,
        )
    };
    p.rect_filled(
        r(0.16, 0.16, 0.58, 0.40),
        1,
        Color32::from_rgb(125, 211, 252),
    );
    p.rect_filled(r(0.16, 0.44, 0.58, 0.84), 1, style::ACCENT);
    p.rect_filled(r(0.62, 0.16, 0.84, 0.50), 1, FileClass::Video.color());
    p.rect_filled(r(0.62, 0.54, 0.84, 0.84), 1, style::GOOD);
    ui.label(RichText::new("RillDisk").strong().size(16.0));
}

fn segmented(ui: &mut Ui, value: &mut bool, options: [(&str, bool); 2], help: &str) {
    Frame::new()
        .fill(style::SURFACE)
        .stroke(Stroke::new(1.0, style::BORDER))
        .corner_radius(7)
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (label, option) in options {
                let active = *value == option;
                let button = egui::Button::new(RichText::new(label).color(if active {
                    Color32::WHITE
                } else {
                    style::DIM
                }))
                .fill(if active {
                    style::ACCENT_DEEP
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(Stroke::NONE);
                if ui.add(button).on_hover_text(help).clicked() {
                    *value = option;
                }
            }
        });
}

fn tab_button(ui: &mut Ui, active: bool, label: &str, accent: bool) -> bool {
    let color = if active {
        style::TEXT
    } else if accent {
        style::GOOD
    } else {
        style::DIM
    };
    let button = egui::Button::new(RichText::new(label).color(color).strong())
        .fill(if active {
            style::SURFACE_HI
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::new(
            1.0,
            if active {
                style::BORDER
            } else {
                Color32::TRANSPARENT
            },
        ));
    ui.add(button).clicked()
}

fn pill(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(style::alpha(color, 30))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(11.0));
        });
}

fn scanning_card(ui: &mut Ui, running: &Running) {
    let elapsed = running.started.elapsed().as_secs_f64();
    let files = running.control.files.load(Ordering::Relaxed);
    let dirs = running.control.directories.load(Ordering::Relaxed);
    let bytes = running.control.bytes.load(Ordering::Relaxed);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.22).max(20.0));
        ui.add(egui::Spinner::new().size(44.0).color(style::ACCENT));
        ui.add_space(14.0);
        ui.label(RichText::new("Scanning").size(26.0).strong());
        ui.label(RichText::new(&running.root).color(style::DIM).monospace());
        ui.add_space(22.0);
        ui.allocate_ui_with_layout(
            vec2(660.0, 70.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                for (value, label) in [
                    (style::count(files), "files"),
                    (style::count(dirs), "folders"),
                    (style::bytes(bytes), "found"),
                    (
                        format!(
                            "{}/s",
                            style::count((files as f64 / elapsed.max(0.001)) as u64)
                        ),
                        "files per second",
                    ),
                ] {
                    ui.allocate_ui_with_layout(
                        vec2(160.0, 64.0),
                        Layout::top_down(Align::Center),
                        |ui| {
                            ui.label(RichText::new(value).size(24.0).strong().color(style::TEXT));
                            ui.label(RichText::new(label).color(style::FAINT));
                        },
                    );
                }
            },
        );
        ui.add_space(14.0);
        ui.label(
            RichText::new(format!(
                "{elapsed:.1} s · Esc to stop and keep partial results"
            ))
            .color(style::FAINT),
        );
    });
}

impl eframe::App for RillApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        let mut actions = self.shortcuts(&ctx);
        let top = egui::Panel::top("top")
            .frame(
                Frame::new()
                    .fill(style::PANEL)
                    .inner_margin(Margin::symmetric(12, 9)),
            )
            .show(ui, |ui| self.top_bar(ui));
        actions.extend(top.inner);
        let status = egui::Panel::bottom("status")
            .frame(
                Frame::new()
                    .fill(style::PANEL)
                    .inner_margin(Margin::symmetric(12, 5)),
            )
            .show(ui, |ui| self.status_bar(ui));
        actions.extend(status.inner);
        if self.data.is_some() && self.running.is_none() {
            let tree = egui::Panel::left("tree")
                .resizable(true)
                .default_size(560.0)
                .size_range(320.0..=1100.0)
                .frame(Frame::new().fill(style::PANEL).inner_margin(Margin {
                    left: 6,
                    right: 4,
                    top: 6,
                    bottom: 0,
                }))
                .show(ui, |ui| self.tree(ui));
            actions.extend(tree.inner);
        }
        let central = egui::CentralPanel::default()
            .frame(Frame::new().fill(style::BG).inner_margin(Margin::same(12)))
            .show(ui, |ui| self.central(ui));
        actions.extend(central.inner);
        self.apply(&ctx, actions);
        if let Some((text, since)) = &self.toast {
            let age = ctx.input(|i| i.time) - since;
            if age > 1.8 {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::unique("toast"))
                    .anchor(Align2::RIGHT_BOTTOM, vec2(-18.0, -44.0))
                    .show(&ctx, |ui| {
                        Frame::new()
                            .fill(style::SURFACE_HI)
                            .stroke(Stroke::new(1.0, style::alpha(style::GOOD, 160)))
                            .corner_radius(8)
                            .inner_margin(Margin::symmetric(14, 8))
                            .show(ui, |ui| {
                                ui.label(RichText::new(format!("✓ {text}")).color(style::GOOD))
                            });
                    });
                ctx.request_repaint();
            }
        }
        if self.tour.is_some() {
            crate::capture::advance(self, &ctx);
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        raw_input.events.append(&mut self.injected);
    }
}

pub fn pointer_for(app: &RillApp, id: usize) -> Option<Pos2> {
    app.layout.tile_of(id).map(|t| t.rect.center())
}
