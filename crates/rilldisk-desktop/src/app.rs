// SPDX-License-Identifier: AGPL-3.0-only
use crate::capture::Tour;
use crate::icons::{self, Icon};
use crate::settings;
use crate::style::{self, FileClass, Identity, t};
use crate::sunburst::{self, Hit, Rings};
use crate::treemap::{self, ColorBy, Params};
use crate::update;
use crate::widgets::{self as w, Kind};
use eframe::egui::{
    self, Align, Align2, Color32, Frame, Key, Layout, Margin, Mesh, Pos2, Rect, RichText, Sense,
    Shape, Stroke, Ui, UiBuilder, pos2, vec2,
};
use rilldisk_core::{
    Candidate, Category, EntryKind, ReclaimReport, Safety, ScanControl, ScanOptions, Snapshot,
    TypeStat, Volume, file_types, find_reclaimable, largest_files, scan, volumes,
};
use std::{
    collections::{HashMap, HashSet},
    f32::consts::TAU,
    path::PathBuf,
    sync::{Arc, atomic::Ordering, mpsc},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Overview,
    Explore,
    Cleanup,
    Largest,
    Types,
    Issues,
    Search,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Chart {
    #[default]
    Map,
    Rings,
}

pub struct Data {
    pub snapshot: Arc<Snapshot>,
    pub reclaim: ReclaimReport,
    /// Entry ID -> index into `reclaim.candidates`.
    pub candidate_of: HashMap<usize, usize>,
    pub types: Vec<TypeStat>,
    /// Class, logical bytes, allocated bytes, files.
    pub classes: Vec<(FileClass, u64, u64, u64)>,
    pub largest: [Vec<usize>; 2],
    /// Agent name and indices of its worktree candidates.
    pub agents: Vec<(&'static str, Vec<usize>)>,
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
        let mut classes: HashMap<FileClass, (u64, u64, u64)> = HashMap::new();
        for t in &types {
            let e = classes.entry(FileClass::of(&t.extension)).or_default();
            e.0 += t.logical_bytes;
            e.1 += t.allocated_bytes.unwrap_or(0);
            e.2 += t.files;
        }
        let mut classes: Vec<_> = classes
            .into_iter()
            .map(|(c, (l, a, f))| (c, l, a, f))
            .collect();
        classes.sort_by_key(|c| std::cmp::Reverse(c.1));
        let mut agents: Vec<(&'static str, Vec<usize>)> = Vec::new();
        for (i, c) in reclaim.candidates.iter().enumerate() {
            if let Some(agent) = c.agent {
                match agents.iter_mut().find(|a| a.0 == agent) {
                    Some(a) => a.1.push(i),
                    None => agents.push((agent, vec![i])),
                }
            }
        }
        agents.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
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
            agents,
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
    /// Show a folder (or a file's folder) in the current view.
    Zoom(usize),
    /// Show a folder in Explore, selecting `id`.
    Explore(usize),
    Up,
    Copy(String, &'static str),
    Tab(Tab),
    Scan(String),
    ScanThen(String, Tab, Option<Category>),
    Pick(usize),
    Theme(bool),
    Identity(Identity),
    /// The user agreed to install this update.
    Install(Box<update::Release>),
    CheckUpdates,
}

pub struct RillApp {
    pub path: String,
    volumes: Vec<Volume>,
    places: Vec<(&'static str, Icon, String)>,
    running: Option<Running>,
    pub data: Option<Arc<Data>>,
    error: Option<String>,
    pub allocated: bool,
    pub tab: Tab,
    pub chart: Chart,
    pub color_by: ColorBy,
    pub selected: Option<usize>,
    pub zoom: usize,
    pub map: treemap::Layout,
    pub mini: treemap::Layout,
    pub rings: Rings,
    generation: usize,
    hovered: Option<usize>,
    list_hover: Option<usize>,
    pub search: String,
    search_edited: Option<f64>,
    focus_search: bool,
    search_results: Option<(String, Vec<usize>)>,
    search_rx: Option<mpsc::Receiver<(String, Vec<usize>)>>,
    pub safety_filter: Option<Safety>,
    pub category_filter: Option<Category>,
    pub picked: HashSet<usize>,
    pub highlight: bool,
    toast: Option<(String, f64)>,
    pub dark: bool,
    pub identity: Identity,
    applied: Option<(Identity, bool)>,
    /// Background update check, its result and a running install.
    update_check: Option<update::Pending<Result<Option<update::Release>, String>>>,
    update_manual: bool,
    pub available: Option<update::Release>,
    installing: Option<update::Install>,
    update_error: Option<String>,
    after_scan: Option<(Tab, Option<Category>)>,
    pub tour: Option<Tour>,
    pub injected: Vec<egui::Event>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn home_dir() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "C:\\".into())
}

impl RillApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        path: Option<String>,
        dark: Option<bool>,
        identity: Option<Identity>,
        tour: Option<Tour>,
    ) -> Self {
        style::install_fonts(&cc.egui_ctx);
        let saved = settings::load();
        let identity = identity.or(saved.identity).unwrap_or(Identity::Tide);
        let dark = dark
            .or(saved.dark)
            .unwrap_or(cc.egui_ctx.system_theme() != Some(egui::Theme::Light));
        style::apply(&cc.egui_ctx, identity, dark);
        let home = home_dir();
        // The platform's standard folders. Linux reads the XDG user-dirs file, so
        // localized or relocated folders are found; missing ones are left out.
        let places = {
            let base = PathBuf::from(&home);
            let mut list = vec![("Home", Icon::Home, base.clone())];
            if cfg!(windows) || cfg!(target_os = "macos") {
                list.push(("Desktop", Icon::Desktop, base.join("Desktop")));
                list.push(("Documents", Icon::Document, base.join("Documents")));
                list.push(("Downloads", Icon::Download, base.join("Downloads")));
                list.push(if cfg!(windows) {
                    ("App data", Icon::Code, base.join("AppData"))
                } else {
                    ("Library", Icon::Code, base.join("Library"))
                });
            } else {
                let env_dir = |key: &str| std::env::var_os(key).map(PathBuf::from);
                let config = env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| base.join(".config"));
                let user_dirs =
                    std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
                // Lines look like `XDG_DESKTOP_DIR="$HOME/Desktop"`.
                let xdg = |key: &str, default: &str| {
                    user_dirs
                        .lines()
                        .filter_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
                        .map(|value| value.trim().trim_matches('"'))
                        .find_map(|value| match value.strip_prefix("$HOME") {
                            Some(rest) => Some(base.join(rest.trim_start_matches('/'))),
                            None => value.starts_with('/').then(|| PathBuf::from(value)),
                        })
                        .unwrap_or_else(|| base.join(default))
                };
                list.push(("Desktop", Icon::Desktop, xdg("XDG_DESKTOP_DIR", "Desktop")));
                list.push((
                    "Documents",
                    Icon::Document,
                    xdg("XDG_DOCUMENTS_DIR", "Documents"),
                ));
                list.push((
                    "Downloads",
                    Icon::Download,
                    xdg("XDG_DOWNLOAD_DIR", "Downloads"),
                ));
                let cache = env_dir("XDG_CACHE_HOME").unwrap_or_else(|| base.join(".cache"));
                list.push(("Cache", Icon::Code, cache));
            }
            let mut seen = HashSet::new();
            list.into_iter()
                // A user-dirs entry set to `$HOME/` disables that folder.
                .filter(|(_, _, path)| path.is_dir() && seen.insert(path.clone()))
                .map(|(name, icon, path)| (name, icon, path.display().to_string()))
                .collect()
        };
        let mut app = Self {
            path: path.clone().unwrap_or_else(home_dir),
            // Public captures show invented drives, never this machine's.
            volumes: if tour.is_some() {
                crate::capture::demo_volumes()
            } else {
                volumes()
            },
            places,
            running: None,
            data: None,
            error: None,
            allocated: false,
            tab: Tab::Overview,
            chart: Chart::Map,
            color_by: ColorBy::Folder,
            selected: None,
            zoom: 0,
            map: Default::default(),
            mini: Default::default(),
            rings: Default::default(),
            generation: 0,
            hovered: None,
            list_hover: None,
            search: String::new(),
            search_edited: None,
            focus_search: false,
            search_results: None,
            search_rx: None,
            safety_filter: None,
            category_filter: None,
            picked: HashSet::new(),
            highlight: false,
            toast: None,
            dark,
            identity,
            applied: Some((identity, dark)),
            update_check: None,
            update_manual: false,
            available: None,
            installing: None,
            update_error: None,
            after_scan: None,
            tour,
            injected: Vec::new(),
        };
        if path.is_some() {
            app.start_scan(&cc.egui_ctx);
        }
        // Scripted captures never touch the network.
        if app.tour.is_none() {
            app.update_check = Some(update::check(&cc.egui_ctx, update::Trigger::Automatic));
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
                    self.after_scan = None;
                    self.error = Some(error);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(16));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.running = None;
                    self.error = Some("scan worker stopped unexpectedly".into());
                }
            }
        }
        if let Some(result) = self.update_check.as_ref().and_then(|c| c.poll()) {
            self.update_check = None;
            match result {
                Ok(Some(release)) => self.available = Some(release),
                Ok(None) if self.update_manual => {
                    self.toast = Some(("RillDisk is up to date".into(), ctx.input(|i| i.time)));
                }
                Ok(None) => {}
                Err(error) if self.update_manual => self.update_error = Some(error),
                Err(_) => {}
            }
            self.update_manual = false;
        }
        if let Some(install) = &self.installing {
            if let Some(Err(error)) = install.poll() {
                self.update_error = Some(error);
                self.installing = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        if let Some(rx) = &self.search_rx
            && let Ok(result) = rx.try_recv()
        {
            self.search_results = Some(result);
            self.search_rx = None;
        }
        // Live search: run shortly after typing stops.
        if let Some(edited) = self.search_edited {
            let now = ctx.input(|i| i.time);
            if now - edited > 0.3 {
                self.search_edited = None;
                if self.search.trim().chars().count() >= 2 {
                    self.run_search();
                    self.tab = Tab::Search;
                } else if self.search.trim().is_empty() {
                    self.search_results = None;
                    if self.tab == Tab::Search {
                        self.tab = Tab::Overview;
                    }
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(60));
            }
        }
    }

    fn install(&mut self, data: Data) {
        self.selected = None;
        self.zoom = 0;
        self.generation += 1;
        self.search_results = None;
        self.picked.clear();
        self.data = Some(Arc::new(data));
        match self.after_scan.take() {
            Some((tab, category)) => {
                self.tab = tab;
                self.category_filter = category;
                self.safety_filter = None;
            }
            None if self.tab == Tab::Search || self.tab == Tab::Issues => self.tab = Tab::Overview,
            None => {}
        }
    }

    fn size(&self, snapshot: &Snapshot, id: usize) -> u64 {
        treemap::size_of(snapshot, id, self.allocated)
    }

    fn volume_of(&self, root: &str) -> Option<&Volume> {
        let root = root.to_lowercase();
        self.volumes
            .iter()
            .find(|v| root.starts_with(&v.root.display().to_string().to_lowercase()))
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

    fn apply(&mut self, ctx: &egui::Context, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Select(id) => self.selected = Some(id),
                Action::Zoom(id) | Action::Explore(id) => {
                    let Some(data) = &self.data else { continue };
                    let entry = &data.snapshot.entries[id];
                    let folder = if entry.kind == EntryKind::Directory {
                        id
                    } else {
                        entry.parent_id.unwrap_or(0)
                    };
                    if matches!(action, Action::Explore(_)) {
                        self.tab = Tab::Explore;
                        self.zoom = entry.parent_id.unwrap_or(0);
                        self.selected = Some(id);
                        if entry.kind != EntryKind::Directory {
                            self.zoom = folder;
                        }
                    } else {
                        self.zoom = folder;
                        self.selected = (folder != id).then_some(id);
                    }
                }
                Action::Up => {
                    if let Some(data) = &self.data
                        && let Some(parent) = data.snapshot.entries[self.zoom].parent_id
                    {
                        self.selected = Some(self.zoom);
                        self.zoom = parent;
                    }
                }
                Action::Copy(text, what) => {
                    ctx.copy_text(text);
                    self.toast = Some((format!("Copied {what}"), ctx.input(|i| i.time)));
                }
                Action::Tab(tab) => self.tab = tab,
                Action::Scan(_) | Action::ScanThen(..) if self.running.is_some() => {}
                Action::Scan(path) => {
                    self.path = path;
                    self.start_scan(ctx);
                }
                Action::ScanThen(path, tab, category) => {
                    self.path = path;
                    self.after_scan = Some((tab, category));
                    self.start_scan(ctx);
                }
                Action::Pick(index) => {
                    if !self.picked.remove(&index) {
                        self.picked.insert(index);
                    }
                }
                Action::Theme(dark) => {
                    self.dark = dark;
                    settings::save(self.identity, self.dark);
                }
                Action::Identity(identity) => {
                    self.identity = identity;
                    settings::save(self.identity, self.dark);
                }
                Action::Install(release) => {
                    self.update_error = None;
                    self.installing = Some(update::install_and_restart(ctx, *release));
                }
                Action::CheckUpdates => {
                    self.update_error = None;
                    self.update_manual = true;
                    self.update_check = Some(update::check(ctx, update::Trigger::Manual));
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
        if ctx.input(|i| i.key_pressed(Key::F5)) && self.data.is_some() {
            {
                let root = self
                    .running
                    .as_ref()
                    .map(|r| r.root.clone())
                    .unwrap_or_else(|| self.path.clone());
                actions.push(Action::Scan(root));
            }
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::O))
            && let Some(folder) = rfd::FileDialog::new().pick_folder()
        {
            actions.push(Action::Scan(folder.display().to_string()));
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
            self.focus_search = true;
        }
        let Some(data) = self.data.clone() else {
            return actions;
        };
        if ctx.egui_wants_keyboard_input() || self.running.is_some() {
            return actions;
        }
        let snapshot = &data.snapshot;
        if ctx.input(|i| {
            i.key_pressed(Key::Backspace) || i.pointer.button_pressed(egui::PointerButton::Extra1)
        }) {
            actions.push(Action::Up);
        }
        for (key, tab) in [
            (Key::Num1, Tab::Overview),
            (Key::Num2, Tab::Explore),
            (Key::Num3, Tab::Cleanup),
            (Key::Num4, Tab::Largest),
            (Key::Num5, Tab::Types),
        ] {
            if ctx.input(|i| i.key_pressed(key)) {
                actions.push(Action::Tab(tab));
            }
        }
        if self.tab == Tab::Explore {
            let rows = treemap::sized_children(snapshot, self.zoom, self.allocated);
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::Enter) || i.key_pressed(Key::ArrowRight),
                )
            });
            let position = self
                .selected
                .and_then(|s| rows.iter().position(|r| r.0 as usize == s));
            if (up || down) && !rows.is_empty() {
                let next = match position {
                    None => 0,
                    Some(p) if up => p.saturating_sub(1),
                    Some(p) => (p + 1).min(rows.len() - 1),
                };
                actions.push(Action::Select(rows[next].0 as usize));
            }
            if ctx.input(|i| i.key_pressed(Key::ArrowLeft)) {
                actions.push(Action::Up);
            }
            if enter
                && let Some(id) = self.selected
                && snapshot.entries[id].kind == EntryKind::Directory
                && snapshot.entries[id].parent_id == Some(self.zoom)
            {
                actions.push(Action::Zoom(id));
            }
        }
        if let Some(id) = self.selected
            && ctx.input(|i| i.modifiers.command && i.key_pressed(Key::C))
        {
            actions.push(Action::Copy(
                snapshot.path(id).display().to_string(),
                "path",
            ));
        }
        actions
    }

    // ------------------------------------------------------------- sidebar

    fn sidebar(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let root = self
            .running
            .as_ref()
            .map(|r| r.root.clone())
            .or_else(|| {
                self.data
                    .as_ref()
                    .map(|d| d.snapshot.summary.root.display().to_string())
            })
            .unwrap_or_default()
            .to_lowercase();
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
            logo(ui.painter(), rect);
            ui.add_space(2.0);
            ui.label(RichText::new("RillDisk").font(style::semibold(17.0)));
        });
        ui.add_space(20.0);
        w::section_label(ui, "Drives");
        ui.add_space(2.0);
        for volume in &self.volumes {
            let letter = volume.root.display().to_string();
            let active =
                root.trim_end_matches('\\') == letter.to_lowercase().trim_end_matches('\\');
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click());
            let p = ui.painter();
            w::row_fill(p, rect, response.hovered(), active);
            let tile = Rect::from_min_size(rect.left_top() + vec2(8.0, 10.0), vec2(30.0, 30.0));
            p.rect_filled(
                tile,
                9,
                style::alpha(th.accent, if th.dark { 50 } else { 34 }),
            );
            icons::paint(p, tile.shrink(7.0), Icon::Drive, th.accent);
            let label = if volume.label.is_empty() {
                "Local Disk"
            } else {
                &volume.label
            };
            let text_left = tile.right() + 10.0;
            let width = rect.right() - text_left - 8.0;
            let name = w::fit(
                p,
                format!("{label} ({})", letter.trim_end_matches('\\')),
                style::semibold(13.0),
                th.text,
                width,
            );
            p.galley(pos2(text_left, rect.top() + 8.0), name, th.text);
            let used = volume.total_bytes.saturating_sub(volume.free_bytes);
            let share = used as f32 / volume.total_bytes.max(1) as f32;
            p.text(
                pos2(text_left, rect.top() + 26.0),
                Align2::LEFT_TOP,
                format!("{} free", style::bytes(volume.free_bytes)),
                style::regular(11.0),
                th.faint,
            );
            let bar = Rect::from_min_size(pos2(text_left, rect.bottom() - 7.0), vec2(width, 3.0));
            w::progress(p, bar, share, usage_color(share));
            if response
                .on_hover_text(format!(
                    "{} used of {} · {}\nClick to scan",
                    style::bytes(used),
                    style::bytes(volume.total_bytes),
                    volume.filesystem
                ))
                .clicked()
            {
                actions.push(Action::Scan(letter));
            }
        }
        ui.add_space(16.0);
        w::section_label(ui, "Places");
        ui.add_space(2.0);
        for (i, (name, icon, path)) in self.places.iter().enumerate() {
            let active = root == path.to_lowercase();
            if place_row(
                ui,
                *icon,
                name,
                active,
                Some(style::chart(i as f32 / 5.0, 0)),
            )
            .on_hover_text(format!("Scan {path}"))
            .clicked()
            {
                actions.push(Action::Scan(path.clone()));
            }
        }
        if place_row(ui, Icon::Folder, "Choose folder…", false, None)
            .on_hover_text("Ctrl+O")
            .clicked()
            && let Some(folder) = rfd::FileDialog::new().pick_folder()
        {
            actions.push(Action::Scan(folder.display().to_string()));
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    RichText::new(concat!("v", env!("CARGO_PKG_VERSION"), " · AGPL-3.0 ·"))
                        .font(style::regular(11.0))
                        .color(th.faint),
                );
                let busy = self.update_check.is_some();
                let link = ui.add(
                    egui::Label::new(
                        RichText::new(if busy {
                            "checking…"
                        } else {
                            "check for updates"
                        })
                        .font(style::regular(11.0))
                        .color(th.dim),
                    )
                    .sense(Sense::click()),
                );
                if link.clicked() && !busy {
                    actions.push(Action::CheckUpdates);
                }
                link.on_hover_text("Right-click for update settings")
                    .context_menu(|ui| {
                        let mut automatic = update::automatic_checks();
                        if ui
                            .checkbox(&mut automatic, "Check once a day on startup")
                            .changed()
                        {
                            let _ = update::set_automatic_checks(automatic);
                        }
                        ui.label(
                            RichText::new("Only asks GitHub for the latest version. Set RILLDISK_NO_UPDATE_CHECK=1 to turn checks off.")
                                .font(style::regular(11.0))
                                .color(th.faint),
                        );
                    });
            });
            if let Some(action) = self.update_card(ui) {
                actions.push(action);
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, th.good);
                ui.label(
                    RichText::new("Read-only · nothing leaves this PC")
                        .font(style::regular(11.5))
                        .color(th.dim),
                );
            });
            ui.add_space(10.0);
            if let Some(identity) = identity_picker(ui, self.identity) {
                actions.push(Action::Identity(identity));
            }
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
        });
        actions
    }

    // -------------------------------------------------------------- header

    fn header(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let snapshot = &data.snapshot;
        ui.horizontal(|ui| {
            ui.set_height(34.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.add_enabled_ui(self.zoom != 0, |ui| {
                if w::icon_button(ui, Icon::Back, "Up one level (Backspace)").clicked() {
                    actions.push(Action::Up);
                }
            });
            ui.add_space(4.0);
            let chain = ancestors(snapshot, self.zoom);
            let n = chain.len();
            for (i, &id) in chain.iter().enumerate() {
                if n > 5 && i > 0 && i < n - 3 {
                    if i == 1 {
                        crumb_separator(ui);
                        ui.label(RichText::new("…").color(th.faint));
                    }
                    continue;
                }
                if i > 0 {
                    crumb_separator(ui);
                }
                let name = if id == 0 {
                    snapshot.summary.root.display().to_string()
                } else {
                    snapshot.name(id).into_owned()
                };
                if crumb(ui, &name, i == n - 1).clicked() {
                    actions.push(Action::Zoom(id));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let root = snapshot.summary.root.display().to_string();
                if w::icon_button(ui, Icon::Refresh, "Rescan (F5)").clicked() {
                    actions.push(Action::Scan(root));
                }
                w::segmented(
                    ui,
                    &mut self.allocated,
                    &[(false, None, "Size"), (true, None, "On disk")],
                    "Size: file length. On disk: allocated clusters, hard links counted once.",
                );
                let search = w::search_field(ui, &mut self.search, "Find files   Ctrl+F", 230.0);
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
                let (text, color) = if s.complete && s.skipped_mounts > 0 {
                    // Other mounted filesystems were listed but not scanned.
                    (
                        format!("{} mounts not entered", style::count(s.skipped_mounts)),
                        th.info,
                    )
                } else if s.complete {
                    ("Complete".to_string(), th.good)
                } else if s.cancelled {
                    ("Stopped early · partial".to_string(), th.warn)
                } else {
                    (
                        format!("Partial · {} issues", style::count(s.issue_count)),
                        th.warn,
                    )
                };
                let mounts = if s.skipped_mounts > 0 {
                    format!(
                        "\n{} mount points here belong to other or virtual filesystems and were not scanned; their sizes are not included. The CLI's --cross-filesystems includes other disks.",
                        style::count(s.skipped_mounts)
                    )
                } else {
                    String::new()
                };
                let chip = w::chip(ui, &text, color).interact(Sense::click());
                let chip = chip.on_hover_text(format!(
                    "{} files · {} folders · scanned in {:.2} s (analysis {} ms)\nSize {} · unique {} · on disk {}{}{mounts}",
                    style::count(s.files),
                    style::count(s.directories),
                    s.elapsed_ms as f64 / 1000.0,
                    data.analysis_ms,
                    style::bytes(s.logical_bytes),
                    style::bytes(s.unique_logical_bytes),
                    s.allocated_bytes.map(style::bytes).unwrap_or("unknown".into()),
                    if s.complete { "" } else { "\nSome folders could not be read (left out of the totals) or something could not be verified. See Issues." }
                ));
                if chip.clicked() && !s.complete {
                    actions.push(Action::Tab(Tab::Issues));
                }
            });
        });
        actions
    }

    fn tab_bar(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let th = t();
        let mut tabs = vec![
            (Tab::Overview, "Overview".to_string(), None),
            (Tab::Explore, "Explore".to_string(), None),
            (
                Tab::Cleanup,
                "Clean up".to_string(),
                (!data.reclaim.candidates.is_empty()).then(|| {
                    (
                        style::bytes(frees_total(&data.reclaim, self.allocated)),
                        th.accent,
                    )
                }),
            ),
            (Tab::Largest, "Largest files".to_string(), None),
            (Tab::Types, "File types".to_string(), None),
        ];
        let issues = data.snapshot.summary.issue_count;
        if issues > 0 {
            tabs.push((
                Tab::Issues,
                "Issues".to_string(),
                Some((style::short_count(issues), th.warn)),
            ));
        }
        if let Some((_, hits)) = &self.search_results {
            tabs.push((
                Tab::Search,
                "Search".to_string(),
                Some((style::short_count(hits.len() as u64), th.dim)),
            ));
        }
        let active = tabs.iter().position(|t| t.0 == self.tab).unwrap_or(0);
        let items: Vec<_> = tabs
            .iter()
            .map(|(_, l, b)| (l.clone(), b.clone()))
            .collect();
        match w::tabs(ui, active, &items) {
            Some(i) => vec![Action::Tab(tabs[i].0)],
            None => Vec::new(),
        }
    }

    fn main(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let data = self.data.clone().expect("main view needs data");
        actions.extend(self.header(ui, &data));
        ui.add_space(12.0);
        if let Some(error) = &self.error {
            error_banner(ui, error, Some(&data));
            ui.add_space(10.0);
        }
        if let Some(running) = &self.running {
            actions.extend(scanning(ui, running));
            return actions;
        }
        actions.extend(self.tab_bar(ui, &data));
        ui.add_space(14.0);
        match self.tab {
            Tab::Overview => actions.extend(self.overview(ui, &data)),
            Tab::Explore => actions.extend(self.explore(ui, &data)),
            Tab::Cleanup => actions.extend(self.cleanup(ui, &data)),
            Tab::Largest => actions.extend(self.largest(ui, &data)),
            Tab::Types => self.types(ui, &data),
            Tab::Issues => self.issues(ui, &data),
            Tab::Search => actions.extend(self.search_tab(ui, &data)),
        }
        actions
    }

    // ------------------------------------------------------------ overview

    fn overview(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let snapshot = &data.snapshot;
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        let gap = 14.0;
        let top_h = 158.0;
        let cw = (full.width() - gap * 3.0) / 4.0;
        let card = |i: usize| {
            Rect::from_min_size(full.min + vec2(i as f32 * (cw + gap), 0.0), vec2(cw, top_h))
        };
        let p = ui.painter().clone();
        let total = self.size(snapshot, 0);

        // 1. Drive or folder
        {
            let rect = card(0);
            p.rect_filled(rect, 16, th.card);
            let root = snapshot.summary.root.display().to_string();
            let inner = rect.shrink(18.0);
            let center = pos2(inner.left() + 40.0, inner.center().y + 8.0);
            let (title, fraction, big, caption, foot) = match self.volume_of(&root) {
                Some(v) => {
                    let used = v.total_bytes.saturating_sub(v.free_bytes);
                    let share = used as f32 / v.total_bytes.max(1) as f32;
                    (
                        format!(
                            "{} ({})",
                            if v.label.is_empty() {
                                "Local Disk"
                            } else {
                                &v.label
                            },
                            v.root.display().to_string().trim_end_matches('\\')
                        ),
                        share,
                        v.free_bytes,
                        format!("free of {}", style::bytes(v.total_bytes)),
                        format!("This scan {}", style::bytes(total)),
                    )
                }
                None => (
                    "Scanned".to_string(),
                    1.0,
                    total,
                    "in this folder".to_string(),
                    String::new(),
                ),
            };
            p.text(
                inner.left_top(),
                Align2::LEFT_TOP,
                title,
                style::semibold(12.5),
                th.dim,
            );
            w::donut(&p, center, 34.0, 9.0, fraction, usage_color(fraction));
            p.text(
                center,
                Align2::CENTER_CENTER,
                format!("{:.0}%", fraction * 100.0),
                style::semibold(12.5),
                th.text,
            );
            let x = center.x + 52.0;
            w::big_bytes(
                &p,
                pos2(x, center.y - 22.0),
                Align2::LEFT_TOP,
                big,
                26.0,
                th.text,
            );
            p.text(
                pos2(x, center.y + 10.0),
                Align2::LEFT_TOP,
                caption,
                style::regular(12.0),
                th.dim,
            );
            p.text(
                pos2(x, center.y + 27.0),
                Align2::LEFT_TOP,
                foot,
                style::regular(11.5),
                th.faint,
            );
        }

        // 2. Clean up
        {
            let rect = card(1);
            let frees = frees_total(&data.reclaim, self.allocated);
            let has = !data.reclaim.candidates.is_empty();
            let (fill, fg) = if has {
                (th.accent, th.on_accent)
            } else {
                (th.card, th.text)
            };
            p.rect_filled(rect, 16, fill);
            let inner = rect.shrink(18.0);
            p.text(
                inner.left_top(),
                Align2::LEFT_TOP,
                "Clean up",
                style::semibold(12.5),
                style::alpha(fg, 210),
            );
            if has {
                w::big_bytes(
                    &p,
                    inner.left_top() + vec2(0.0, 20.0),
                    Align2::LEFT_TOP,
                    frees,
                    30.0,
                    fg,
                );
                let caption = w::fit(
                    &p,
                    format!(
                        "{} folders: worktrees, build output, caches",
                        style::count(
                            data.reclaim
                                .candidates
                                .iter()
                                .filter(|c| c.nested_in.is_none())
                                .count() as u64
                        )
                    ),
                    style::regular(12.0),
                    style::alpha(fg, 220),
                    inner.width(),
                );
                p.galley(inner.left_top() + vec2(0.0, 60.0), caption, fg);
                let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(
                    pos2(inner.left(), inner.bottom() - 32.0),
                    inner.max,
                )));
                if w::button(&mut child, Kind::Inverse, Some(Icon::Sparkle), "Review").clicked() {
                    actions.push(Action::Tab(Tab::Cleanup));
                }
            } else {
                p.text(
                    inner.left_top() + vec2(0.0, 26.0),
                    Align2::LEFT_TOP,
                    "Nothing to clean here",
                    style::semibold(18.0),
                    th.text,
                );
                p.text(
                    inner.left_top() + vec2(0.0, 52.0),
                    Align2::LEFT_TOP,
                    "No worktrees, build output or caches found.",
                    style::regular(12.0),
                    th.dim,
                );
            }
        }

        // 3. AI agent worktrees
        {
            let rect = card(2);
            let response = ui.interact(rect, ui.scope_id().with("agents-card"), Sense::click());
            p.rect_filled(
                rect,
                16,
                if response.hovered() {
                    th.hover
                } else {
                    th.card
                },
            );
            let inner = rect.shrink(18.0);
            icons::paint(
                &p,
                Rect::from_min_size(inner.left_top(), vec2(16.0, 16.0)),
                Icon::Agent,
                th.dim,
            );
            p.text(
                inner.left_top() + vec2(22.0, 0.0),
                Align2::LEFT_TOP,
                "AI agent worktrees",
                style::semibold(12.5),
                th.dim,
            );
            let worktrees: Vec<&Candidate> = data
                .reclaim
                .candidates
                .iter()
                .filter(|c| c.category == Category::GitWorktree)
                .collect();
            let agent_count: usize = data.agents.iter().map(|a| a.1.len()).sum();
            if worktrees.is_empty() {
                p.text(
                    inner.left_top() + vec2(0.0, 26.0),
                    Align2::LEFT_TOP,
                    "None found",
                    style::semibold(18.0),
                    th.text,
                );
                p.text(
                    inner.left_top() + vec2(0.0, 52.0),
                    Align2::LEFT_TOP,
                    "Scan your home folder to find Claude Code,\nCodex, T3 Code and Cursor checkouts.",
                    style::regular(12.0),
                    th.dim,
                );
            } else {
                let freed: u64 = worktrees
                    .iter()
                    .filter(|c| c.nested_in.is_none())
                    .map(|c| frees(c, self.allocated))
                    .sum();
                let (count, unit) = match (agent_count, worktrees.len()) {
                    (1, _) => (1, "agent worktree"),
                    (0, 1) => (1, "Git worktree"),
                    (0, n) => (n, "Git worktrees"),
                    (n, _) => (n, "agent worktrees"),
                };
                w::big_value(
                    &p,
                    inner.left_top() + vec2(0.0, 20.0),
                    Align2::LEFT_TOP,
                    &style::count(count as u64),
                    unit,
                    30.0,
                    th.text,
                );
                p.text(
                    inner.left_top() + vec2(0.0, 60.0),
                    Align2::LEFT_TOP,
                    format!("frees ≥ {} if removed", style::bytes(freed)),
                    style::regular(12.0),
                    th.dim,
                );
                let mut y = inner.top() + 84.0;
                for (agent, indices) in data.agents.iter().take(2) {
                    let bytes: u64 = indices
                        .iter()
                        .map(|&i| &data.reclaim.candidates[i])
                        .filter(|c| c.nested_in.is_none())
                        .map(|c| frees(c, self.allocated))
                        .sum();
                    p.circle_filled(pos2(inner.left() + 4.0, y + 8.0), 3.5, agent_color(agent));
                    p.text(
                        pos2(inner.left() + 14.0, y),
                        Align2::LEFT_TOP,
                        *agent,
                        style::semibold(12.0),
                        th.text,
                    );
                    p.text(
                        pos2(inner.right(), y),
                        Align2::RIGHT_TOP,
                        format!("{} · {}", indices.len(), style::bytes(bytes)),
                        style::regular(12.0),
                        th.dim,
                    );
                    y += 19.0;
                }
            }
            if response
                .on_hover_text("Show worktrees in Clean up")
                .clicked()
            {
                actions.push(Action::Tab(Tab::Cleanup));
                self.category_filter = Some(Category::GitWorktree);
                self.safety_filter = None;
            }
        }

        // 4. Biggest file and scan facts
        {
            let rect = card(3);
            let response = ui.interact(rect, ui.scope_id().with("biggest-card"), Sense::click());
            p.rect_filled(
                rect,
                16,
                if response.hovered() {
                    th.hover
                } else {
                    th.card
                },
            );
            let inner = rect.shrink(18.0);
            p.text(
                inner.left_top(),
                Align2::LEFT_TOP,
                "Biggest file",
                style::semibold(12.5),
                th.dim,
            );
            let biggest = data.largest[usize::from(self.allocated)].first().copied();
            if let Some(id) = biggest {
                let name = w::fit(
                    &p,
                    snapshot.name(id).into_owned(),
                    style::semibold(14.5),
                    th.text,
                    inner.width(),
                );
                p.galley(inner.left_top() + vec2(0.0, 22.0), name, th.text);
                p.text(
                    inner.left_top() + vec2(0.0, 42.0),
                    Align2::LEFT_TOP,
                    style::bytes(self.size(snapshot, id)),
                    style::regular(12.5),
                    th.dim,
                );
                if response.clicked() {
                    actions.push(Action::Explore(id));
                }
            }
            let s = &snapshot.summary;
            p.text(
                pos2(inner.left(), inner.bottom() - 34.0),
                Align2::LEFT_TOP,
                "This scan",
                style::semibold(12.5),
                th.dim,
            );
            p.text(
                pos2(inner.left(), inner.bottom() - 14.0),
                Align2::LEFT_TOP,
                format!(
                    "{} files · {} folders · {}",
                    style::short_count(s.files),
                    style::short_count(s.directories),
                    style::duration(s.elapsed_ms as u64)
                ),
                style::regular(12.0),
                th.text,
            );
        }

        // Bottom area: types + folders on the left, map on the right.
        let bottom = Rect::from_min_max(pos2(full.left(), full.top() + top_h + gap), full.max);
        let left_w = (bottom.width() * 0.36).clamp(320.0, 470.0);
        let types_rect = Rect::from_min_size(bottom.min, vec2(left_w, 176.0));
        let folders_rect = Rect::from_min_max(
            pos2(bottom.left(), types_rect.bottom() + gap),
            pos2(bottom.left() + left_w, bottom.bottom()),
        );
        let map_rect =
            Rect::from_min_max(pos2(bottom.left() + left_w + gap, bottom.top()), bottom.max);

        // Types
        {
            p.rect_filled(types_rect, 16, th.card);
            let inner = types_rect.shrink(18.0);
            w::big_bytes(&p, inner.left_top(), Align2::LEFT_TOP, total, 28.0, th.text);
            p.text(
                inner.right_top() + vec2(0.0, 10.0),
                Align2::RIGHT_TOP,
                style::plural(snapshot.summary.files, "file"),
                style::regular(12.0),
                th.dim,
            );
            let bar =
                Rect::from_min_size(inner.left_top() + vec2(0.0, 46.0), vec2(inner.width(), 8.0));
            class_bar(&p, bar, &data.classes, self.allocated);
            for (i, (class, logical, allocated, _)) in data.classes.iter().take(6).enumerate() {
                let size = if self.allocated { *allocated } else { *logical };
                let col = i % 2;
                let row = i / 2;
                let x = inner.left() + col as f32 * (inner.width() / 2.0 + 8.0);
                let y = bar.bottom() + 16.0 + row as f32 * 21.0;
                let cell_w = inner.width() / 2.0 - 8.0;
                p.circle_filled(pos2(x + 4.0, y + 8.0), 4.0, class.color());
                p.text(
                    pos2(x + 14.0, y),
                    Align2::LEFT_TOP,
                    class.label(),
                    style::regular(12.0),
                    th.dim,
                );
                p.text(
                    pos2(x + cell_w, y),
                    Align2::RIGHT_TOP,
                    style::bytes(size),
                    style::semibold(12.0),
                    th.text,
                );
            }
        }

        // Largest folders
        {
            p.rect_filled(folders_rect, 16, th.card);
            let inner = folders_rect.shrink(14.0);
            let items = treemap::sized_children(snapshot, 0, self.allocated);
            let sum: f64 = items.iter().map(|i| i.1).sum::<f64>().max(1.0);
            let top_size = items.first().map_or(1.0, |i| i.1).max(1.0);
            let row_h = 46.0;
            let rows = ((inner.height() / row_h).floor() as usize).max(1);
            let mut cumulative = 0.0;
            for (i, (id, size)) in items.iter().enumerate() {
                let color = style::chart(((cumulative + size / 2.0) / sum) as f32, 0);
                cumulative += size;
                if i >= rows {
                    break;
                }
                let id = *id as usize;
                let rect = Rect::from_min_size(
                    inner.left_top() + vec2(0.0, i as f32 * row_h),
                    vec2(inner.width(), row_h - 4.0),
                );
                let response =
                    ui.interact(rect, ui.scope_id().with(("folder-row", id)), Sense::click());
                w::row_fill(&p, rect, response.hovered(), false);
                let entry = &snapshot.entries[id];
                let is_dir = entry.kind == EntryKind::Directory;
                let tile = Rect::from_min_size(rect.left_top() + vec2(8.0, 7.0), vec2(28.0, 28.0));
                p.rect_filled(tile, 8, color);
                icons::paint(
                    &p,
                    tile.shrink(7.0),
                    if is_dir { Icon::Folder } else { Icon::Document },
                    th.ink,
                );
                let text_x = tile.right() + 11.0;
                let size_w = 70.0;
                let bar_w = (rect.width() * 0.22).min(90.0);
                let name_w = rect.right() - text_x - size_w - bar_w - 18.0;
                let name = w::fit(
                    &p,
                    snapshot.name(id).into_owned(),
                    style::semibold(13.0),
                    th.text,
                    name_w,
                );
                p.galley(pos2(text_x, rect.top() + 6.0), name, th.text);
                p.text(
                    pos2(text_x, rect.top() + 23.0),
                    Align2::LEFT_TOP,
                    if is_dir {
                        style::plural(entry.file_count, "file")
                    } else {
                        "file".into()
                    },
                    style::regular(11.0),
                    th.faint,
                );
                let bar = Rect::from_min_size(
                    pos2(rect.right() - size_w - bar_w - 8.0, rect.center().y - 2.0),
                    vec2(bar_w, 4.0),
                );
                w::progress(
                    &p,
                    bar,
                    (*size / top_size) as f32,
                    style::alpha(th.dim, 150),
                );
                p.text(
                    pos2(rect.right() - 8.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    style::bytes(*size as u64),
                    style::semibold(13.0),
                    th.text,
                );
                let response = response.on_hover_text(format!(
                    "{:.1}% of this scan · click to explore",
                    *size * 100.0 / sum
                ));
                if response.clicked() {
                    actions.push(Action::Tab(Tab::Explore));
                    actions.push(Action::Zoom(id));
                }
                response.context_menu(|ui| entry_menu(ui, snapshot, id, &mut actions));
            }
        }

        // Map
        {
            p.rect_filled(map_rect, 16, th.card);
            let inner = map_rect.shrink(8.0);
            let params = Params {
                root: 0,
                rect: inner,
                allocated: self.allocated,
                generation: self.generation,
                color_by: ColorBy::Folder,
                pixels_per_point: ui.ctx().pixels_per_point(),
                compact: false,
            };
            if self.mini.key != Some(treemap::key(&params)) {
                self.mini = treemap::build(snapshot, &params);
            }
            let painter = ui.painter_at(map_rect);
            painter.add(Shape::mesh(self.mini.mesh.clone()));
            treemap::paint_labels(&painter, &self.mini, snapshot, self.allocated, false);
            if self.mini.truncated {
                detail_note(&painter, inner);
            }
            let response = ui.interact(inner, ui.scope_id().with("mini-map"), Sense::click());
            if let Some(pos) = response.hover_pos()
                && let Some(tile) = self.mini.hit(pos).copied()
            {
                treemap::wash(
                    &painter,
                    &tile,
                    style::alpha(Color32::WHITE, if th.dark { 30 } else { 70 }),
                );
                treemap::outline(&painter, &tile, style::alpha(th.ink, 160), 1.5);
                let id = tile.id as usize;
                let response = response.on_hover_ui_at_pointer(|ui| {
                    entry_tooltip(ui, data, id, self.allocated, total)
                });
                if response.clicked() {
                    actions.push(Action::Explore(id));
                }
                response.context_menu(|ui| entry_menu(ui, snapshot, id, &mut actions));
            }
        }
        actions
    }

    // ------------------------------------------------------------- explore

    fn explore(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            self.chart_controls(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let snapshot = &data.snapshot;
                let entry = &snapshot.entries[self.zoom];
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        style::bytes(self.size(snapshot, self.zoom)),
                        style::plural(entry.file_count, "file")
                    ))
                    .color(th.dim),
                );
            });
        });
        ui.add_space(12.0);
        let full = ui.available_rect_before_wrap();
        ui.allocate_rect(full, Sense::hover());
        let panel_w = (full.width() * 0.3).clamp(300.0, 380.0);
        let chart_rect =
            Rect::from_min_max(full.min, pos2(full.right() - panel_w - 14.0, full.bottom()));
        let panel_rect = Rect::from_min_max(pos2(chart_rect.right() + 14.0, full.top()), full.max);
        ui.painter().rect_filled(chart_rect, 16, th.card);
        actions.extend(self.chart_area(ui, data, chart_rect.shrink(10.0), chart_rect));
        actions.extend(self.side_panel(ui, data, panel_rect, th.card));
        actions
    }

    /// Map/Rings, color mode and the cleanable highlight toggle.
    fn chart_controls(&mut self, ui: &mut Ui) {
        {
            w::segmented(
                ui,
                &mut self.chart,
                &[
                    (Chart::Map, Some(Icon::Grid), "Map"),
                    (Chart::Rings, Some(Icon::Rings), "Rings"),
                ],
                "Map: nested rectangles. Rings: a sunburst, one ring per level.",
            );
            w::segmented(
                ui,
                &mut self.color_by,
                &[
                    (ColorBy::Folder, None, "By folder"),
                    (ColorBy::Kind, None, "By type"),
                ],
                "Color tiles by top-level folder or by file type",
            );
            let label = if self.highlight {
                "Cleanable lit up"
            } else {
                "Light up cleanable"
            };
            if w::button(
                ui,
                if self.highlight {
                    Kind::Primary
                } else {
                    Kind::Soft
                },
                Some(Icon::Sparkle),
                label,
            )
            .on_hover_text("Show worktrees, build output, dependencies and caches")
            .clicked()
            {
                self.highlight = !self.highlight;
            }
        }
    }

    /// The interactive chart inside `inner`; `clip` bounds its painting.
    fn chart_area(&mut self, ui: &mut Ui, data: &Data, inner: Rect, clip: Rect) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let snapshot = &data.snapshot;
        let chart_rect = clip;
        let params = Params {
            root: self.zoom,
            rect: inner,
            allocated: self.allocated,
            generation: self.generation,
            color_by: self.color_by,
            pixels_per_point: ui.ctx().pixels_per_point(),
            compact: false,
        };
        self.hovered = None;
        let response = ui.interact(inner, ui.scope_id().with("chart"), Sense::click());
        let painter = ui.painter_at(chart_rect);
        let view_total = self.size(snapshot, self.zoom);
        match self.chart {
            Chart::Map => {
                if self.map.key != Some(treemap::key(&params)) {
                    self.map = treemap::build(snapshot, &params);
                }
                painter.add(Shape::mesh(self.map.mesh.clone()));
                treemap::paint_labels(&painter, &self.map, snapshot, self.allocated, false);
                if self.highlight {
                    painter.rect_filled(inner, 10, style::alpha(th.card, 200));
                    for tile in &self.map.tiles {
                        if let Some(&c) = data.candidate_of.get(&(tile.id as usize)) {
                            let color = style::safety_color(data.reclaim.candidates[c].safety);
                            treemap::wash(
                                &painter,
                                tile,
                                style::alpha(color, if th.dark { 120 } else { 150 }),
                            );
                            treemap::outline(&painter, tile, color, 1.5);
                            if tile.rect.width() > 70.0 && tile.rect.height() > 34.0 {
                                let name = w::fit(
                                    &painter,
                                    snapshot.name(tile.id as usize).into_owned(),
                                    style::semibold(11.5),
                                    th.text,
                                    tile.rect.width() - 12.0,
                                );
                                painter.galley(tile.rect.min + vec2(6.0, 5.0), name, th.text);
                                painter.text(
                                    tile.rect.min + vec2(6.0, 20.0),
                                    Align2::LEFT_TOP,
                                    style::bytes(self.size(snapshot, tile.id as usize)),
                                    style::regular(10.5),
                                    th.dim,
                                );
                            }
                        }
                    }
                }
                for (id, color, width) in [
                    (self.list_hover, style::alpha(th.ink, 200), 2.0),
                    (self.selected, th.accent, 2.5),
                ] {
                    if let Some(id) = id
                        && let Some(tile) = self.map.tile_of(id)
                    {
                        treemap::outline(&painter, tile, color, width);
                    }
                }
                if self.map.truncated {
                    detail_note(&painter, inner);
                }
                if self.map.tiles.is_empty() {
                    painter.text(
                        inner.center(),
                        Align2::CENTER_CENTER,
                        "Nothing with a size here",
                        style::regular(14.0),
                        th.faint,
                    );
                }
                if let Some(pos) = response.hover_pos()
                    && let Some(tile) = self.map.hit(pos).copied()
                {
                    treemap::wash(
                        &painter,
                        &tile,
                        style::alpha(Color32::WHITE, if th.dark { 26 } else { 60 }),
                    );
                    treemap::outline(&painter, &tile, style::alpha(th.ink, 180), 1.5);
                    self.hovered = Some(tile.id as usize);
                }
            }
            Chart::Rings => {
                if self.rings.key != Some(treemap::key(&params)) {
                    self.rings = sunburst::build(snapshot, &params);
                }
                let rings = &self.rings;
                painter.circle_filled(
                    rings.center,
                    rings.outer() + 4.0,
                    style::alpha(th.bg, if th.dark { 90 } else { 140 }),
                );
                painter.add(Shape::mesh(rings.mesh.clone()));
                let mut extra = Mesh::default();
                if self.highlight {
                    painter.circle_filled(
                        rings.center,
                        rings.outer() + 2.0,
                        style::alpha(th.card, 200),
                    );
                    for segment in &rings.segments {
                        if let Some(&c) = data.candidate_of.get(&(segment.id as usize)) {
                            let color = style::safety_color(data.reclaim.candidates[c].safety);
                            rings.highlight(&mut extra, segment, color);
                        }
                    }
                }
                let hit = response.hover_pos().and_then(|pos| rings.hit(pos));
                if let Some(Hit::Segment(segment)) = &hit {
                    self.hovered = Some(segment.id as usize);
                }
                for id in [self.list_hover, self.hovered, self.selected]
                    .into_iter()
                    .flatten()
                {
                    if let Some(segment) = rings.segment_of(id) {
                        let color = if Some(id) == self.selected && Some(id) != self.hovered {
                            th.accent
                        } else {
                            style::mix(segment.color, Color32::WHITE, 0.35)
                        };
                        rings.highlight(&mut extra, segment, color);
                    }
                }
                painter.add(Shape::mesh(Arc::new(extra)));
                // Center: what the pointer is on, or the folder being shown.
                let focus = self.hovered.or(self.list_hover).unwrap_or(self.zoom);
                let hover_center = matches!(hit, Some(Hit::Center));
                painter.circle_filled(
                    rings.center,
                    rings.inner - 3.0,
                    if hover_center && self.zoom != 0 {
                        th.hover
                    } else {
                        th.card
                    },
                );
                let focus_size = self.size(snapshot, focus);
                let share = focus_size as f32 / view_total.max(1) as f32;
                w::arc(
                    &painter,
                    rings.center,
                    rings.inner - 12.0,
                    4.0,
                    -TAU / 4.0,
                    -TAU / 4.0 + TAU * share,
                    th.accent,
                );
                w::big_bytes(
                    &painter,
                    rings.center + vec2(0.0, -6.0),
                    Align2::CENTER_CENTER,
                    focus_size,
                    (rings.inner * 0.3).clamp(16.0, 28.0),
                    th.text,
                );
                let label = if focus == 0 {
                    snapshot.summary.root.display().to_string()
                } else {
                    snapshot.name(focus).into_owned()
                };
                let name = w::fit(
                    &painter,
                    label,
                    style::regular(12.0),
                    th.dim,
                    rings.inner * 1.5,
                );
                painter.galley(
                    rings.center + vec2(-name.size().x / 2.0, 14.0),
                    name,
                    th.dim,
                );
                if hover_center && self.zoom != 0 {
                    painter.text(
                        rings.center + vec2(0.0, 32.0),
                        Align2::CENTER_TOP,
                        "click to go up",
                        style::regular(10.5),
                        th.faint,
                    );
                }
                if rings.segments.is_empty() {
                    painter.text(
                        inner.center() + vec2(0.0, rings.outer() * 0.6),
                        Align2::CENTER_CENTER,
                        "Nothing with a size here",
                        style::regular(14.0),
                        th.faint,
                    );
                }
            }
        }
        if let Some(id) = self.hovered {
            let response = response.on_hover_ui_at_pointer(|ui| {
                entry_tooltip(ui, data, id, self.allocated, view_total)
            });
            if response.clicked() {
                if snapshot.entries[id].kind == EntryKind::Directory {
                    actions.push(Action::Zoom(id));
                } else {
                    actions.push(Action::Select(id));
                }
            }
            response.context_menu(|ui| entry_menu(ui, snapshot, id, &mut actions));
        } else if response.clicked() && self.chart == Chart::Rings {
            actions.push(Action::Up);
        }
        actions
    }

    /// Contents of the hovered folder (or the shown one), like a Finder column.
    fn side_panel(&mut self, ui: &mut Ui, data: &Data, rect: Rect, fill: Color32) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let snapshot = &data.snapshot;
        ui.painter().rect_filled(rect, 16, fill);
        let focus = match self.hovered {
            Some(id) if snapshot.entries[id].kind == EntryKind::Directory => id,
            Some(id) => snapshot.entries[id].parent_id.unwrap_or(self.zoom),
            None => self.zoom,
        };
        let inner = rect.shrink(18.0);
        let p = ui.painter().clone();
        let size = self.size(snapshot, focus);
        let size_rect = w::big_bytes(
            &p,
            inner.right_top(),
            Align2::RIGHT_TOP,
            size,
            22.0,
            th.text,
        );
        let title = if focus == 0 {
            snapshot.summary.root.display().to_string()
        } else {
            snapshot.name(focus).into_owned()
        };
        let galley = w::fit(
            &p,
            title,
            style::semibold(19.0),
            th.text,
            inner.width() - size_rect.width() - 12.0,
        );
        p.galley(inner.left_top(), galley, th.text);
        let children = treemap::sized_children(snapshot, focus, self.allocated);
        let entry = &snapshot.entries[focus];
        p.text(
            inner.left_top() + vec2(0.0, 30.0),
            Align2::LEFT_TOP,
            format!(
                "{} · {}",
                style::plural(entry.file_count, "file"),
                style::plural(snapshot.children(focus).len() as u64, "item")
            ),
            style::regular(12.0),
            th.faint,
        );
        let footer_h = 58.0;
        let list = Rect::from_min_max(
            pos2(rect.left() + 8.0, inner.top() + 56.0),
            pos2(rect.right() - 8.0, rect.bottom() - footer_h),
        );
        let mut child = ui.new_child(UiBuilder::new().max_rect(list));
        let colors: HashMap<u32, Color32> = match self.chart {
            Chart::Map => self.map.tiles.iter().map(|t| (t.id, t.color)).collect(),
            Chart::Rings => self
                .rings
                .segments
                .iter()
                .map(|s| (s.id, s.color))
                .collect(),
        };
        let mut row_hover = None;
        let candidate_of = &data.candidate_of;
        egui::ScrollArea::vertical()
            .id_salt(("side", focus))
            .auto_shrink(false)
            .show_rows(&mut child, 36.0, children.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for &(id, bytes) in &children[range] {
                    let id = id as usize;
                    let (row, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
                    let p = ui.painter();
                    let selected = self.selected == Some(id);
                    w::row_fill(
                        p,
                        row.shrink2(vec2(2.0, 1.0)),
                        response.hovered() || self.hovered == Some(id),
                        selected,
                    );
                    if response.hovered() {
                        row_hover = Some(id);
                    }
                    let is_dir = snapshot.entries[id].kind == EntryKind::Directory;
                    let color = colors.get(&(id as u32)).copied().unwrap_or(th.faint);
                    let dot = pos2(row.left() + 18.0, row.center().y);
                    if is_dir {
                        p.circle_filled(dot, 5.0, color);
                    } else {
                        p.circle_stroke(dot, 4.0, Stroke::new(2.0, color));
                    }
                    let size_text = style::bytes(bytes as u64);
                    let name_w = row.width() - 120.0;
                    let mut name_x = row.left() + 34.0;
                    let name = w::fit(
                        p,
                        snapshot.name(id).into_owned(),
                        style::regular(13.0),
                        th.text,
                        name_w,
                    );
                    let name_size = name.size().x;
                    p.galley(
                        pos2(name_x, row.center().y - name.size().y / 2.0),
                        name,
                        th.text,
                    );
                    name_x += name_size + 8.0;
                    if let Some(&c) = candidate_of.get(&id) {
                        let color = style::safety_color(data.reclaim.candidates[c].safety);
                        if name_x + 10.0 < row.right() - 90.0 {
                            p.circle_filled(pos2(name_x, row.center().y), 3.0, color);
                        }
                    }
                    p.text(
                        pos2(row.right() - 12.0, row.center().y),
                        Align2::RIGHT_CENTER,
                        size_text,
                        style::semibold(12.5),
                        th.text,
                    );
                    if response.clicked() {
                        if is_dir {
                            actions.push(Action::Zoom(id));
                        } else {
                            actions.push(Action::Select(id));
                        }
                    }
                    response.context_menu(|ui| entry_menu(ui, snapshot, id, &mut actions));
                }
            });
        if children.is_empty() {
            p.text(
                list.center_top() + vec2(0.0, 30.0),
                Align2::CENTER_TOP,
                "Empty",
                style::regular(13.0),
                th.faint,
            );
        }
        self.list_hover = row_hover;
        // Footer: path and a copy button.
        let footer = Rect::from_min_max(
            pos2(inner.left(), rect.bottom() - footer_h + 8.0),
            pos2(inner.right(), rect.bottom() - 14.0),
        );
        p.line_segment(
            [
                footer.left_top() - vec2(0.0, 6.0),
                footer.right_top() - vec2(0.0, 6.0),
            ],
            Stroke::new(1.0, style::alpha(th.faint, 50)),
        );
        let path = snapshot
            .path(self.selected.unwrap_or(focus))
            .display()
            .to_string();
        let mut row = ui.new_child(
            UiBuilder::new()
                .max_rect(footer)
                .layout(Layout::right_to_left(Align::Center)),
        );
        if w::icon_button(&mut row, Icon::Copy, "Copy path (Ctrl+C)").clicked() {
            actions.push(Action::Copy(path.clone(), "path"));
        }
        let galley = w::fit(&p, path, style::mono(11.5), th.faint, footer.width() - 40.0);
        p.galley(
            pos2(footer.left(), footer.center().y - galley.size().y / 2.0),
            galley,
            th.faint,
        );
        actions
    }

    // ------------------------------------------------------------- clean up

    fn cleanup(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
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
        // Summary
        let head = ui.available_rect_before_wrap();
        let narrow = head.width() < 820.0;
        let head = Rect::from_min_size(
            head.min,
            vec2(head.width(), if narrow { 132.0 } else { 96.0 }),
        );
        ui.allocate_rect(head, Sense::hover());
        let p = ui.painter().clone();
        p.rect_filled(head, 16, th.card);
        let inner = head.shrink2(vec2(20.0, 16.0));
        let total = frees_total(report, self.allocated);
        let big = w::big_bytes(&p, inner.left_top(), Align2::LEFT_TOP, total, 32.0, th.text);
        p.text(
            pos2(big.right() + 12.0, big.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            "can be freed, at least",
            style::regular(13.0),
            th.dim,
        );
        let outer = report.candidates.iter().filter(|c| c.nested_in.is_none());
        let shared: u64 = outer.clone().map(|c| c.shared_bytes).sum();
        let mut caption = format!(
            "{} across {} folders.",
            style::bytes(outer.clone().map(|c| size_metric(c, self.allocated)).sum()),
            style::count(outer.count() as u64)
        );
        if shared > 0 {
            caption.push_str(&format!(
                " {} is hard-linked elsewhere (pnpm, Cargo) and stays until every link is gone.",
                style::bytes(shared)
            ));
        }
        let galley = w::fit(
            &p,
            caption,
            style::regular(12.0),
            th.faint,
            if narrow {
                inner.width()
            } else {
                inner.width() * 0.55
            },
        );
        p.galley(inner.left_top() + vec2(0.0, 44.0), galley, th.faint);
        let filter_rect = if narrow {
            Rect::from_min_max(pos2(inner.left(), inner.bottom() - 32.0), inner.max)
        } else {
            Rect::from_min_max(pos2(inner.center().x, inner.top()), inner.max)
        };
        let mut filters = ui.new_child(UiBuilder::new().max_rect(filter_rect).layout(if narrow {
            Layout::left_to_right(Align::Min)
        } else {
            Layout::right_to_left(Align::Min)
        }));
        let mut safety = self.safety_filter;
        let count = |s: Safety| report.candidates.iter().filter(|c| c.safety == s).count();
        let labels = [
            (None, "All".to_string()),
            (
                Some(Safety::Regenerable),
                format!("Rebuildable {}", count(Safety::Regenerable)),
            ),
            (
                Some(Safety::Cache),
                format!("Caches {}", count(Safety::Cache)),
            ),
            (
                Some(Safety::Review),
                format!("Review first {}", count(Safety::Review)),
            ),
        ];
        let options: Vec<(Option<Safety>, Option<Icon>, &str)> =
            labels.iter().map(|(s, l)| (*s, None, l.as_str())).collect();
        if w::segmented(
            &mut filters,
            &mut safety,
            &options,
            "Rebuildable: project tools recreate it. Caches: re-downloaded on demand. Review first: may hold unique work.",
        ) {
            self.safety_filter = safety;
        }
        ui.add_space(10.0);
        // Category chips
        let mut categories: Vec<(Category, usize, u64)> = Category::ALL
            .iter()
            .map(|&cat| {
                let items = report.candidates.iter().filter(|c| {
                    c.category == cat && self.safety_filter.is_none_or(|s| s == c.safety)
                });
                (
                    cat,
                    items.clone().count(),
                    items
                        .filter(|c| c.nested_in.is_none())
                        .map(|c| frees(c, self.allocated))
                        .sum(),
                )
            })
            .filter(|c| c.1 > 0)
            .collect();
        categories.sort_by_key(|c| std::cmp::Reverse(c.2));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if filter_chip(ui, "Everything", None, self.category_filter.is_none()).clicked() {
                self.category_filter = None;
            }
            for (cat, n, bytes) in &categories {
                let active = self.category_filter == Some(*cat);
                if filter_chip(
                    ui,
                    &format!("{}  {}", category_title(*cat), style::bytes(*bytes)),
                    Some(*n),
                    active,
                )
                .clicked()
                {
                    self.category_filter = if active { None } else { Some(*cat) };
                }
            }
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("RillDisk never deletes. Tick folders to build a cleanup plan you can review and run yourself; Git refuses to remove a worktree with uncommitted changes.")
                .font(style::regular(12.0))
                .color(th.faint),
        );
        ui.add_space(8.0);
        // Rows grouped by category, biggest group first.
        enum Row {
            Group(Category, usize, u64),
            Item(usize),
        }
        let mut rows = Vec::new();
        for (cat, n, bytes) in &categories {
            if self.category_filter.is_some_and(|f| f != *cat) {
                continue;
            }
            rows.push(Row::Group(*cat, *n, *bytes));
            for (i, c) in report.candidates.iter().enumerate() {
                if c.category == *cat && self.safety_filter.is_none_or(|s| s == c.safety) {
                    rows.push(Row::Item(i));
                }
            }
        }
        let now = now_ms();
        let _ = now;
        let list_rect = ui.available_rect_before_wrap();
        egui::ScrollArea::vertical()
            .id_salt("cleanup")
            .auto_shrink(false)
            .show_rows(ui, 64.0, rows.len() + 1, |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for index in range {
                    let Some(row) = rows.get(index) else {
                        // Room for the floating plan bar.
                        ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
                        continue;
                    };
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::click());
                    let p = ui.painter();
                    match row {
                        Row::Group(cat, n, bytes) => {
                            let y = rect.bottom() - 16.0;
                            p.text(
                                pos2(rect.left() + 4.0, y),
                                Align2::LEFT_CENTER,
                                category_title(*cat),
                                style::semibold(15.0),
                                th.text,
                            );
                            let w = p
                                .layout_no_wrap(
                                    category_title(*cat).into(),
                                    style::semibold(15.0),
                                    th.text,
                                )
                                .size()
                                .x;
                            p.text(
                                pos2(rect.left() + 12.0 + w, y),
                                Align2::LEFT_CENTER,
                                style::count(*n as u64),
                                style::regular(12.5),
                                th.faint,
                            );
                            p.text(
                                pos2(rect.right() - 8.0, y),
                                Align2::RIGHT_CENTER,
                                format!("frees ≥ {}", style::bytes(*bytes)),
                                style::regular(12.5),
                                th.dim,
                            );
                        }
                        Row::Item(i) => {
                            actions.extend(self.candidate_card(ui, data, *i, rect, response));
                        }
                    }
                }
            });
        actions.extend(self.plan_bar(ui, data, list_rect));
        actions
    }

    /// One reclaim suggestion as a card; a click toggles it in the cleanup plan.
    fn candidate_card(
        &mut self,
        ui: &mut Ui,
        data: &Data,
        i: usize,
        rect: Rect,
        response: egui::Response,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let report = &data.reclaim;
        let snapshot = &data.snapshot;
        let p = ui.painter().clone();
        let p = &p;
        let c = &report.candidates[i];
        let card = rect.shrink2(vec2(0.0, 3.0));
        let picked = self.picked.contains(&i);
        p.rect_filled(
            card,
            12,
            if response.hovered() {
                th.hover
            } else {
                th.card
            },
        );
        let check = w::check(
            p,
            pos2(card.left() + 22.0, card.center().y),
            picked,
            response.hovered(),
        );
        let color = style::safety_color(c.safety);
        let tile = Rect::from_min_size(
            pos2(check.right() + 12.0, card.center().y - 17.0),
            vec2(34.0, 34.0),
        );
        p.rect_filled(tile, 10, style::alpha(color, if th.dark { 46 } else { 34 }));
        icons::paint(p, tile.shrink(9.0), category_icon(c), color);
        let text_x = tile.right() + 12.0;
        let right_w = 150.0;
        let text_w = card.right() - text_x - right_w;
        let mut title = c.label.to_string();
        if let Some(agent) = c.agent {
            title = format!("{title} · {agent}");
        }
        let galley = w::fit(p, title, style::semibold(13.5), th.text, text_w - 110.0);
        let title_w = galley.size().x;
        p.galley(pos2(text_x, card.top() + 11.0), galley, th.text);
        let mut chip_x = text_x + title_w + 8.0;
        let chip = w::chip_at(p, pos2(chip_x, card.top() + 20.0), c.safety.label(), color);
        chip_x = chip.right() + 6.0;
        if c.nested_in.is_some() && chip_x + 60.0 < text_x + text_w {
            w::chip_at(p, pos2(chip_x, card.top() + 20.0), "nested", th.faint);
        }
        let path = w::fit(
            p,
            c.path.display().to_string(),
            style::mono(11.5),
            th.faint,
            text_w,
        );
        p.galley(pos2(text_x, card.top() + 34.0), path, th.faint);
        let size = frees(c, self.allocated);
        p.text(
            pos2(card.right() - 16.0, card.top() + 12.0),
            Align2::RIGHT_TOP,
            style::bytes(size),
            style::semibold(15.0),
            th.text,
        );
        let idle = match c.idle_days {
            Some(0) => "active today".to_string(),
            Some(1) => "idle 1 day".to_string(),
            Some(d) => format!("idle {d} days"),
            None => String::new(),
        };
        let detail = if c.shared_bytes > 0 {
            format!(
                "of {} · {}",
                style::bytes(size_metric(c, self.allocated)),
                idle
            )
        } else {
            idle
        };
        p.text(
            pos2(card.right() - 16.0, card.top() + 34.0),
            Align2::RIGHT_TOP,
            detail,
            style::regular(11.5),
            th.faint,
        );
        // Row actions appear on hover.
        let mut action_rect = None;
        if response.hovered() || response.contains_pointer() {
            let r = Rect::from_min_size(
                pos2(card.right() - right_w - 108.0, card.center().y - 16.0),
                vec2(104.0, 32.0),
            );
            p.rect_filled(r.expand(4.0), 10, th.hover);
            action_rect = Some(r);
        }
        let mut clicked_action = false;
        if let Some(r) = action_rect {
            let mut bar = ui.new_child(
                UiBuilder::new()
                    .max_rect(r)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            bar.spacing_mut().item_spacing.x = 4.0;
            if w::icon_button(
                &mut bar,
                Icon::Terminal,
                &format!("Copy command:\n{}", c.suggested_action),
            )
            .clicked()
            {
                actions.push(Action::Copy(c.suggested_action.clone(), "command"));
                clicked_action = true;
            }
            if w::icon_button(&mut bar, Icon::Copy, "Copy path").clicked() {
                actions.push(Action::Copy(c.path.display().to_string(), "path"));
                clicked_action = true;
            }
            if w::icon_button(&mut bar, Icon::Eye, "Show in Explore").clicked() {
                actions.push(Action::Explore(c.entry_id));
                self.highlight = true;
                clicked_action = true;
            }
        }
        let response =
            response.on_hover_text(format!("{}\n\nSuggested: {}", c.reason, c.suggested_action));
        if response.clicked() && !clicked_action {
            actions.push(Action::Pick(i));
        }
        response.context_menu(|ui| entry_menu(ui, snapshot, c.entry_id, &mut actions));
        actions
    }

    /// Floating bar summarising ticked folders, with a copyable plan.
    fn plan_bar(&mut self, ui: &mut Ui, data: &Data, area: Rect) -> Vec<Action> {
        let mut actions = Vec::new();
        if self.picked.is_empty() {
            return actions;
        }
        let th = t();
        let report = &data.reclaim;
        let picked_ids: HashSet<usize> = self
            .picked
            .iter()
            .map(|&i| report.candidates[i].entry_id)
            .collect();
        let mut chosen: Vec<&Candidate> = self
            .picked
            .iter()
            .map(|&i| &report.candidates[i])
            .filter(|c| c.nested_in.is_none_or(|p| !picked_ids.contains(&p)))
            .collect();
        chosen.sort_by_key(|c| std::cmp::Reverse(c.logical_bytes));
        let total: u64 = chosen.iter().map(|c| frees(c, self.allocated)).sum();
        let width = area.width().min(640.0);
        let rect = Rect::from_center_size(
            pos2(area.center().x, area.bottom() - 36.0),
            vec2(width, 60.0),
        );
        let p = ui.painter().clone();
        w::shadow(&p, rect, 18);
        p.rect_filled(rect, 18, th.popup);
        let inner = rect.shrink2(vec2(20.0, 10.0));
        p.text(
            pos2(inner.left(), inner.center().y - 9.0),
            Align2::LEFT_CENTER,
            format!("{} selected", style::count(chosen.len() as u64)),
            style::semibold(14.0),
            th.text,
        );
        p.text(
            pos2(inner.left(), inner.center().y + 10.0),
            Align2::LEFT_CENTER,
            format!("frees ≥ {}", style::bytes(total)),
            style::regular(12.5),
            th.accent,
        );
        let mut bar = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::right_to_left(Align::Center)),
        );
        bar.spacing_mut().item_spacing.x = 8.0;
        if w::button(&mut bar, Kind::Primary, Some(Icon::Copy), "Copy cleanup plan")
            .on_hover_text("A commented list of the suggested command for each folder. Review it, then run what you agree with.")
            .clicked()
        {
            let mut plan = format!(
                "# RillDisk cleanup plan: {} folders, frees at least {}\n# Review every line before running it. RillDisk itself never deletes anything.\n",
                chosen.len(),
                style::bytes(total)
            );
            for c in &chosen {
                plan.push_str(&format!(
                    "\n# {} · {} · {}\n",
                    c.label,
                    style::bytes(frees(c, self.allocated)),
                    c.path.display()
                ));
                if is_command(&c.suggested_action) {
                    plan.push_str(&c.suggested_action);
                    plan.push('\n');
                } else {
                    plan.push_str(&format!("# {}\n", c.suggested_action));
                }
            }
            actions.push(Action::Copy(plan, "cleanup plan"));
        }
        if w::button(&mut bar, Kind::Ghost, None, "Clear").clicked() {
            self.picked.clear();
        }
        actions
    }

    // -------------------------------------------------------- lists & types

    fn largest(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let ids = data.largest[usize::from(self.allocated)].clone();
        self.file_list(ui, data, &ids, false, "largest")
    }

    fn file_list(
        &mut self,
        ui: &mut Ui,
        data: &Data,
        ids: &[usize],
        full_path: bool,
        salt: &str,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let snapshot = &data.snapshot;
        let now = now_ms();
        let top = ids.first().map_or(1, |&id| self.size(snapshot, id)).max(1);
        egui::ScrollArea::vertical()
            .id_salt(salt)
            .auto_shrink(false)
            .show_rows(ui, 56.0, ids.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for &id in &ids[range] {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
                    let p = ui.painter();
                    let card = rect.shrink2(vec2(0.0, 2.0));
                    w::row_fill(p, card, response.hovered(), self.selected == Some(id));
                    let entry = &snapshot.entries[id];
                    let name = snapshot.name(id);
                    let is_dir = entry.kind == EntryKind::Directory;
                    let ext = style::extension_of(&name);
                    let class = FileClass::of(&ext);
                    let badge = Rect::from_min_size(
                        pos2(card.left() + 10.0, card.center().y - 17.0),
                        vec2(44.0, 34.0),
                    );
                    p.rect_filled(
                        badge,
                        9,
                        if is_dir {
                            style::chart(0.6, 0)
                        } else {
                            class.color()
                        },
                    );
                    if is_dir {
                        icons::paint(p, badge.shrink(9.0), Icon::Folder, th.ink);
                    } else {
                        let label = if ext.is_empty() {
                            "FILE".into()
                        } else {
                            ext.to_uppercase()
                        };
                        let g =
                            w::fit(p, label, style::semibold(10.0), th.ink, badge.width() - 6.0);
                        p.galley(badge.center() - g.size() / 2.0, g, th.ink);
                    }
                    let text_x = badge.right() + 14.0;
                    let right = 230.0;
                    let text_w = card.right() - text_x - right;
                    let g = w::fit(p, name.into_owned(), style::semibold(13.5), th.text, text_w);
                    p.galley(pos2(text_x, card.top() + 9.0), g, th.text);
                    let location = if full_path {
                        snapshot.path(id).display().to_string()
                    } else {
                        entry
                            .parent_id
                            .map(|p| snapshot.path(p).display().to_string())
                            .unwrap_or_default()
                    };
                    let g = w::fit(p, location, style::regular(11.5), th.faint, text_w);
                    p.galley(pos2(text_x, card.top() + 28.0), g, th.faint);
                    let size = self.size(snapshot, id);
                    let bar = Rect::from_min_size(
                        pos2(card.right() - right + 10.0, card.center().y - 2.0),
                        vec2(80.0, 4.0),
                    );
                    w::progress(p, bar, size as f32 / top as f32, style::alpha(th.dim, 150));
                    p.text(
                        pos2(card.right() - 14.0, card.top() + 11.0),
                        Align2::RIGHT_TOP,
                        style::bytes(size),
                        style::semibold(13.5),
                        th.text,
                    );
                    p.text(
                        pos2(card.right() - 14.0, card.top() + 30.0),
                        Align2::RIGHT_TOP,
                        style::age(now, entry.modified_unix_ms),
                        style::regular(11.5),
                        th.faint,
                    );
                    if response.clicked() {
                        actions.push(Action::Explore(id));
                    }
                    response.context_menu(|ui| entry_menu(ui, snapshot, id, &mut actions));
                }
            });
        actions
    }

    fn types(&mut self, ui: &mut Ui, data: &Data) {
        let th = t();
        let metric = |t: &TypeStat| {
            if self.allocated {
                t.allocated_bytes.unwrap_or(0)
            } else {
                t.logical_bytes
            }
        };
        let total: u64 = data.types.iter().map(metric).sum::<u64>().max(1);
        let head = ui.available_rect_before_wrap();
        let rows_of_legend = data.classes.len().div_ceil(3) as f32;
        let head = Rect::from_min_size(head.min, vec2(head.width(), 96.0 + rows_of_legend * 22.0));
        ui.allocate_rect(head, Sense::hover());
        let p = ui.painter().clone();
        p.rect_filled(head, 16, th.card);
        let inner = head.shrink2(vec2(20.0, 16.0));
        w::big_bytes(&p, inner.left_top(), Align2::LEFT_TOP, total, 28.0, th.text);
        p.text(
            inner.right_top() + vec2(0.0, 10.0),
            Align2::RIGHT_TOP,
            format!("{} kinds of file", style::count(data.types.len() as u64)),
            style::regular(12.0),
            th.dim,
        );
        let bar = Rect::from_min_size(
            inner.left_top() + vec2(0.0, 48.0),
            vec2(inner.width(), 10.0),
        );
        class_bar(&p, bar, &data.classes, self.allocated);
        let col_w = inner.width() / 3.0;
        for (i, (class, logical, allocated, files)) in data.classes.iter().enumerate() {
            let size = if self.allocated { *allocated } else { *logical };
            let x = inner.left() + (i % 3) as f32 * col_w;
            let y = bar.bottom() + 16.0 + (i / 3) as f32 * 22.0;
            p.circle_filled(pos2(x + 4.0, y + 8.0), 4.0, class.color());
            p.text(
                pos2(x + 14.0, y),
                Align2::LEFT_TOP,
                class.label(),
                style::regular(12.5),
                th.dim,
            );
            p.text(
                pos2(x + col_w - 24.0, y),
                Align2::RIGHT_TOP,
                format!("{} · {}", style::bytes(size), style::short_count(*files)),
                style::semibold(12.5),
                th.text,
            );
        }
        ui.add_space(12.0);
        let mut rows: Vec<&TypeStat> = data.types.iter().collect();
        if self.allocated {
            rows.sort_by_key(|t| std::cmp::Reverse(t.allocated_bytes.unwrap_or(0)));
        }
        rows.truncate(500);
        let top = rows.first().map_or(1, |t| metric(t)).max(1);
        egui::ScrollArea::vertical()
            .id_salt("types")
            .auto_shrink(false)
            .show_rows(ui, 46.0, rows.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for t in &rows[range] {
                    let size = metric(t);
                    let class = FileClass::of(&t.extension);
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
                    let p = ui.painter();
                    let card = rect.shrink2(vec2(0.0, 2.0));
                    w::row_fill(p, card, response.hovered(), false);
                    let badge = Rect::from_min_size(
                        pos2(card.left() + 10.0, card.center().y - 14.0),
                        vec2(48.0, 28.0),
                    );
                    p.rect_filled(badge, 8, class.color());
                    let label = if t.extension.is_empty() {
                        "NONE".into()
                    } else {
                        t.extension.to_uppercase()
                    };
                    let g = w::fit(p, label, style::semibold(10.0), th.ink, 44.0);
                    p.galley(badge.center() - g.size() / 2.0, g, th.ink);
                    p.text(
                        pos2(badge.right() + 14.0, card.center().y),
                        Align2::LEFT_CENTER,
                        class.label(),
                        style::regular(12.5),
                        th.dim,
                    );
                    let bar = Rect::from_min_max(
                        pos2(card.left() + 240.0, card.center().y - 3.0),
                        pos2(card.right() - 260.0, card.center().y + 3.0),
                    );
                    if bar.width() > 20.0 {
                        w::progress(p, bar, size as f32 / top as f32, class.color());
                    }
                    p.text(
                        pos2(card.right() - 170.0, card.center().y),
                        Align2::RIGHT_CENTER,
                        style::plural(t.files, "file"),
                        style::regular(12.5),
                        th.dim,
                    );
                    p.text(
                        pos2(card.right() - 70.0, card.center().y),
                        Align2::RIGHT_CENTER,
                        style::bytes(size),
                        style::semibold(13.0),
                        th.text,
                    );
                    p.text(
                        pos2(card.right() - 14.0, card.center().y),
                        Align2::RIGHT_CENTER,
                        format!("{:.1}%", size as f64 * 100.0 / total as f64),
                        style::regular(12.0),
                        th.faint,
                    );
                }
            });
    }

    fn issues(&mut self, ui: &mut Ui, data: &Data) {
        let th = t();
        let summary = &data.snapshot.summary;
        ui.label(
            RichText::new(format!(
                "{} issues: folders that could not be read (left out of the totals) or checks that could not be made (each message says which). {} are listed. Protected system folders are expected without administrator rights.",
                style::count(summary.issue_count),
                if summary.issues.len() as u64 == summary.issue_count { "All".to_string() } else { format!("The first {}", summary.issues.len()) }
            ))
            .color(th.dim),
        );
        ui.add_space(10.0);
        let issues = &summary.issues;
        egui::ScrollArea::vertical()
            .id_salt("issues")
            .auto_shrink(false)
            .show_rows(ui, 54.0, issues.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for issue in &issues[range] {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
                    let p = ui.painter();
                    let card = rect.shrink2(vec2(0.0, 3.0));
                    p.rect_filled(card, 12, th.card);
                    let tile = Rect::from_min_size(
                        pos2(card.left() + 12.0, card.center().y - 15.0),
                        vec2(30.0, 30.0),
                    );
                    p.rect_filled(tile, 9, style::alpha(th.warn, 40));
                    icons::paint(p, tile.shrink(8.0), Icon::Warning, th.warn);
                    let x = tile.right() + 12.0;
                    let g = w::fit(
                        p,
                        issue.path.display().to_string(),
                        style::mono(12.0),
                        th.text,
                        card.right() - x - 12.0,
                    );
                    p.galley(pos2(x, card.top() + 9.0), g, th.text);
                    let g = w::fit(
                        p,
                        issue.message.clone(),
                        style::regular(11.5),
                        th.faint,
                        card.right() - x - 12.0,
                    );
                    p.galley(pos2(x, card.top() + 27.0), g, th.faint);
                }
            });
    }

    fn search_tab(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let th = t();
        let Some((query, hits)) = self.search_results.clone() else {
            ui.add(egui::Spinner::new().color(th.accent));
            return Vec::new();
        };
        ui.label(
            RichText::new(format!(
                "{} matches for “{}”{}",
                style::count(hits.len() as u64),
                query,
                if hits.len() >= 1000 {
                    ", the 1,000 largest shown"
                } else {
                    ""
                }
            ))
            .color(th.dim),
        );
        ui.add_space(8.0);
        if hits.is_empty() {
            empty_state(
                ui,
                Icon::Search,
                "No matches",
                "Try part of a file or folder name.",
            );
            return Vec::new();
        }
        self.file_list(ui, data, &hits, true, "search")
    }

    // --------------------------------------------------------------- home

    fn home(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let width = (ui.available_width() - 80.0).clamp(420.0, 820.0);
            let left = (ui.available_width() - width) / 2.0;
            let content = 470.0 + self.volumes.len() as f32 * 88.0;
            ui.add_space(((ui.available_height() - content) / 2.0).max(28.0));
            ui.horizontal(|ui| {
                ui.add_space(left);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    if let Some(error) = &self.error {
                        error_banner(ui, error, None);
                        ui.add_space(12.0);
                    }
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
                        logo(ui.painter(), rect);
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.label(RichText::new("RillDisk").font(style::semibold(28.0)));
                            ui.label(
                                RichText::new("See where your disk went, and what you can safely get back.")
                                    .font(style::regular(14.5))
                                    .color(th.dim),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            let (icon, tip) = if self.dark { (Icon::Sun, "Light theme") } else { (Icon::Moon, "Dark theme") };
                            if w::icon_button(ui, icon, tip).clicked() {
                                actions.push(Action::Theme(!self.dark));
                            }
                        });
                    });
                    ui.add_space(28.0);
                    w::section_label(ui, "Drives");
                    ui.add_space(4.0);
                    for volume in &self.volumes {
                        if let Some(action) = drive_row(ui, volume) {
                            actions.push(action);
                        }
                        ui.add_space(8.0);
                    }
                    if self.volumes.is_empty() {
                        ui.label(RichText::new("No drives listed on this platform yet. Pick a folder below.").color(th.dim));
                    }
                    ui.add_space(18.0);
                    w::section_label(ui, "Scan something else");
                    ui.add_space(4.0);
                    let home = home_dir();
                    let cards: [(Icon, &str, &str, usize); 3] = [
                        (Icon::Folder, "A folder", "Pick any folder, or press Ctrl+O", 0),
                        (Icon::Home, "Your home folder", "Projects, downloads and app caches", 1),
                        (Icon::Agent, "AI agent worktrees", "Stale checkouts left by coding agents", 2),
                    ];
                    let gap = 12.0;
                    let cw = (width - gap * 2.0) / 3.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for (icon, title, sub, which) in cards {
                            let (rect, response) = ui.allocate_exact_size(vec2(cw, 112.0), Sense::click());
                            let p = ui.painter();
                            p.rect_filled(rect, 16, if response.hovered() { th.hover } else { th.card });
                            let tile = Rect::from_min_size(rect.left_top() + vec2(16.0, 16.0), vec2(34.0, 34.0));
                            p.rect_filled(tile, 10, style::chart(which as f32 * 0.4 + 0.05, 0));
                            icons::paint(p, tile.shrink(9.0), icon, th.ink);
                            p.text(rect.left_top() + vec2(16.0, 62.0), Align2::LEFT_TOP, title, style::semibold(14.0), th.text);
                            let g = w::fit(p, sub.into(), style::regular(12.0), th.dim, cw - 32.0);
                            p.galley(rect.left_top() + vec2(16.0, 82.0), g, th.dim);
                            if response.clicked() {
                                match which {
                                    0 => {
                                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                            actions.push(Action::Scan(folder.display().to_string()));
                                        }
                                    }
                                    1 => actions.push(Action::Scan(home.clone())),
                                    _ => actions.push(Action::ScanThen(home.clone(), Tab::Cleanup, Some(Category::GitWorktree))),
                                }
                            }
                        }
                    });
                    ui.add_space(18.0);
                    w::card_frame().show(ui, |ui| {
                        ui.set_width(width - 32.0);
                        ui.horizontal(|ui| {
                            let (tile, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                            ui.painter().rect_filled(tile, 10, th.field);
                            icons::paint(ui.painter(), tile.shrink(9.0), Icon::Terminal, th.dim);
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Also a CLI and an MCP server").font(style::semibold(13.5)));
                                ui.label(
                                    RichText::new("The same scanner for scripts and AI agents: rilldisk reclaim ~   ·   rilldisk mcp")
                                        .font(style::regular(12.0))
                                        .color(th.dim),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if w::button(ui, Kind::Soft, Some(Icon::Copy), "Copy").on_hover_text("Copy an MCP config snippet").clicked() {
                                    actions.push(Action::Copy(
                                        "{\n  \"mcpServers\": {\n    \"rilldisk\": { \"command\": \"rilldisk\", \"args\": [\"mcp\"] }\n  }\n}".into(),
                                        "MCP config",
                                    ));
                                }
                            });
                        });
                    });
                    if let Some(action) = self.update_card(ui) {
                        actions.push(action);
                    }
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(concat!(
                                "Read-only · metadata only · nothing leaves this PC   ·   v",
                                env!("CARGO_PKG_VERSION"),
                                " · AGPL-3.0"
                            ))
                            .font(style::regular(11.5))
                            .color(th.faint),
                        );
                    });
                    ui.add_space(20.0);
                });
            });
        });
        actions
    }

    /// Offer for a newer release; nothing installs until the user clicks.
    fn update_card(&self, ui: &mut Ui) -> Option<Action> {
        let th = t();
        let mut action = None;
        if self.available.is_none() && self.update_error.is_none() {
            return None;
        }
        ui.add_space(10.0);
        Frame::new()
            .fill(style::alpha(th.accent, if th.dark { 34 } else { 26 }))
            .corner_radius(12)
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if let Some(error) = &self.update_error {
                    ui.label(
                        RichText::new("Update failed")
                            .font(style::semibold(13.0))
                            .color(th.danger),
                    );
                    ui.label(
                        RichText::new(error)
                            .font(style::regular(11.5))
                            .color(th.dim),
                    );
                    return;
                }
                let Some(release) = &self.available else {
                    return;
                };
                ui.label(
                    RichText::new(format!("RillDisk {} is available", release.version))
                        .font(style::semibold(13.0)),
                );
                if let Some(install) = &self.installing {
                    let (bar, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
                    w::progress(
                        ui.painter(),
                        bar,
                        install.progress().unwrap_or(0.05),
                        th.accent,
                    );
                    ui.label(
                        RichText::new("Downloading and verifying…")
                            .font(style::regular(11.5))
                            .color(th.dim),
                    );
                    return;
                }
                if let Some(reason) = &release.blocked {
                    ui.label(
                        RichText::new(reason)
                            .font(style::regular(11.5))
                            .color(th.dim),
                    );
                    if w::button(ui, Kind::Soft, None, "Open the release page").clicked() {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(&release.notes_url));
                    }
                    return;
                }
                if let Some(command) = &release.update_command {
                    let owner = release.managed_by.as_deref().unwrap_or("a package manager");
                    ui.label(
                        RichText::new(format!("Installed with {owner}. To update, {command}."))
                            .font(style::regular(11.5))
                            .color(th.dim),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if let Some(url) = &release.download_url {
                            let file = url.rsplit('/').next().unwrap_or(url);
                            if w::button(ui, Kind::Primary, None, "Download")
                                .on_hover_text(file)
                                .clicked()
                            {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                            }
                        }
                        if w::button(ui, Kind::Ghost, None, "What's new").clicked() {
                            ui.ctx()
                                .open_url(egui::OpenUrl::new_tab(&release.notes_url));
                        }
                    });
                    return;
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if w::button(ui, Kind::Primary, None, "Install and restart")
                        .on_hover_text(
                            "Downloads the release, checks its SHA-256 and replaces both programs",
                        )
                        .clicked()
                    {
                        action = Some(Action::Install(Box::new(release.clone())));
                    }
                    if w::button(ui, Kind::Ghost, None, "What's new").clicked() {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(&release.notes_url));
                    }
                });
            });
        action
    }

    fn paint_toast(&mut self, ctx: &egui::Context) {
        let Some((text, since)) = &self.toast else {
            return;
        };
        let age = ctx.input(|i| i.time) - since;
        if age > 1.8 {
            self.toast = None;
            return;
        }
        let th = t();
        let fade = ((1.8 - age) / 0.25).clamp(0.0, 1.0) as f32;
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -28.0))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                ui.set_opacity(fade);
                Frame::new()
                    .fill(th.popup)
                    .corner_radius(14)
                    .shadow(ctx.global_style().visuals.popup_shadow)
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 9.0, th.good);
                            icons::paint(ui.painter(), r.shrink(3.0), Icon::Check, Color32::WHITE);
                            ui.label(RichText::new(text).font(style::semibold(13.0)));
                        });
                    });
            });
        ctx.request_repaint();
    }
}

impl eframe::App for RillApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.applied != Some((self.identity, self.dark)) {
            style::apply(&ctx, self.identity, self.dark);
            self.applied = Some((self.identity, self.dark));
        }
        self.poll(&ctx);
        let mut actions = self.shortcuts(&ctx);
        let th = t();
        if self.data.is_none() {
            let central = egui::CentralPanel::default()
                .frame(Frame::new().fill(th.bg).inner_margin(Margin::same(24)))
                .show(ui, |ui| match &self.running {
                    Some(running) => scanning(ui, running),
                    None => self.home(ui),
                });
            actions.extend(central.inner);
        } else {
            let side = egui::Panel::left("sidebar")
                .exact_size(248.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(Frame::new().fill(th.sidebar).inner_margin(Margin {
                    left: 14,
                    right: 14,
                    top: 18,
                    bottom: 16,
                }))
                .show(ui, |ui| self.sidebar(ui));
            actions.extend(side.inner);
            let central = egui::CentralPanel::default()
                .frame(Frame::new().fill(th.bg).inner_margin(Margin {
                    left: 24,
                    right: 24,
                    top: 18,
                    bottom: 20,
                }))
                .show(ui, |ui| self.main(ui));
            actions.extend(central.inner);
        }
        self.apply(&ctx, actions);
        self.paint_toast(&ctx);
        if self.tour.is_some() {
            crate::capture::advance(self, &ctx);
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        raw_input.events.append(&mut self.injected);
    }
}

// ------------------------------------------------------------------ helpers

/// Space removing a candidate frees at least: hard-linked files are excluded.
fn frees(candidate: &Candidate, allocated: bool) -> u64 {
    if allocated {
        candidate.exclusive_bytes
    } else {
        candidate
            .logical_bytes
            .saturating_sub(candidate.shared_bytes)
    }
}

fn size_metric(candidate: &Candidate, allocated: bool) -> u64 {
    if allocated {
        candidate.allocated_bytes.unwrap_or(0)
    } else {
        candidate.logical_bytes
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

fn is_command(action: &str) -> bool {
    [
        "git ",
        "cargo ",
        "npm ",
        "pnpm ",
        "yarn ",
        "bun ",
        "dotnet ",
        "gradle ",
        "mvn ",
        "terraform ",
        "uv ",
        "pip ",
        "go ",
    ]
    .iter()
    .any(|prefix| action.starts_with(prefix))
}

fn ancestors(snapshot: &Snapshot, id: usize) -> Vec<usize> {
    let mut chain = vec![id];
    let mut cursor = snapshot.entries[id].parent_id;
    while let Some(parent) = cursor {
        chain.push(parent);
        cursor = snapshot.entries[parent].parent_id;
    }
    chain.reverse();
    chain
}

fn usage_color(share: f32) -> Color32 {
    let th = t();
    if share > 0.9 {
        th.danger
    } else if share > 0.8 {
        th.warn
    } else {
        th.accent
    }
}

fn agent_color(agent: &str) -> Color32 {
    match agent {
        "Claude Code" => Color32::from_rgb(217, 119, 87),
        "Codex" => Color32::from_rgb(110, 140, 250),
        "T3 Code" => Color32::from_rgb(168, 112, 240),
        "Cursor" => Color32::from_rgb(70, 190, 160),
        _ => t().dim,
    }
}

fn category_title(category: Category) -> &'static str {
    match category {
        Category::GitWorktree => "Git worktrees",
        Category::BuildOutput => "Build output",
        Category::Dependencies => "Dependencies",
        Category::PackageCache => "Package caches",
        Category::AppCache => "App caches",
        Category::Temporary => "Temporary files",
        Category::RecycleBin => "Recycle Bin",
    }
}

fn category_icon(candidate: &Candidate) -> Icon {
    match candidate.category {
        Category::GitWorktree if candidate.agent.is_some() => Icon::Agent,
        Category::GitWorktree => Icon::Code,
        Category::BuildOutput => Icon::Grid,
        Category::Dependencies => Icon::Folder,
        Category::PackageCache | Category::AppCache => Icon::Download,
        Category::Temporary => Icon::Refresh,
        Category::RecycleBin => Icon::Document,
    }
}

fn class_bar(
    p: &egui::Painter,
    bar: Rect,
    classes: &[(FileClass, u64, u64, u64)],
    allocated: bool,
) {
    let total: u64 = classes
        .iter()
        .map(|c| if allocated { c.2 } else { c.1 })
        .sum::<u64>()
        .max(1);
    p.rect_filled(bar, 4, t().field);
    let mut x = bar.left();
    for (class, logical, alloc, _) in classes {
        let size = if allocated { *alloc } else { *logical };
        let w = bar.width() * size as f32 / total as f32;
        if w < 1.0 {
            continue;
        }
        let seg = Rect::from_min_size(pos2(x, bar.top()), vec2((w - 2.0).max(1.0), bar.height()));
        p.rect_filled(seg, 3, class.color());
        x += w;
    }
}

/// The mark: a rounded tile holding a stream of treemap blocks in theme colors.
/// Small notice when a map hit its tile limit, so blank space is not mistaken
/// for unused disk space.
fn detail_note(painter: &egui::Painter, area: Rect) {
    let th = t();
    let text = "Largest items only · open a folder for more detail";
    let galley = painter.layout_no_wrap(text.into(), style::regular(11.5), th.text);
    let rect = Rect::from_min_size(
        pos2(area.left() + 10.0, area.bottom() - galley.size().y - 18.0),
        galley.size() + vec2(18.0, 10.0),
    );
    painter.rect_filled(rect, 9, style::alpha(th.popup, 235));
    painter.galley(rect.min + vec2(9.0, 5.0), galley, th.text);
}

fn logo(p: &egui::Painter, rect: Rect) {
    let th = t();
    let s = rect.width();
    let stops = style::stops();
    let tile = if th.dark { th.card } else { th.text };
    p.rect_filled(rect, (s * 0.26) as u8, tile);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| {
        Rect::from_min_max(rect.min + vec2(x0, y0) * s, rect.min + vec2(x1, y1) * s)
    };
    let radius = (s * 0.07).max(1.0) as u8;
    p.rect_filled(r(0.18, 0.18, 0.55, 0.82), radius, th.accent);
    p.rect_filled(r(0.60, 0.18, 0.82, 0.46), radius, stops[2]);
    p.rect_filled(r(0.60, 0.51, 0.82, 0.66), radius, stops[3]);
    p.rect_filled(r(0.60, 0.71, 0.82, 0.82), radius, stops[5]);
}

/// Three swatches; returns a newly chosen identity.
fn identity_picker(ui: &mut Ui, current: Identity) -> Option<Identity> {
    let th = t();
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for identity in Identity::ALL {
            let theme = identity.theme(th.dark);
            let (rect, response) = ui.allocate_exact_size(vec2(64.0, 30.0), Sense::click());
            let p = ui.painter();
            let active = identity == current;
            p.rect_filled(
                rect,
                9,
                if active {
                    th.hover
                } else if response.hovered() {
                    style::alpha(th.hover, 140)
                } else {
                    Color32::TRANSPARENT
                },
            );
            let dot = pos2(rect.left() + 14.0, rect.center().y);
            p.circle_filled(dot, 7.0, theme.accent);
            p.text(
                pos2(rect.left() + 26.0, rect.center().y),
                Align2::LEFT_CENTER,
                identity.name(),
                if active {
                    style::semibold(12.0)
                } else {
                    style::regular(12.0)
                },
                if active { th.text } else { th.dim },
            );
            if response
                .on_hover_text(format!("{} palette", identity.name()))
                .clicked()
                && !active
            {
                chosen = Some(identity);
            }
        }
    });
    chosen
}

fn crumb(ui: &mut Ui, text: &str, last: bool) -> egui::Response {
    let th = t();
    let font = if last {
        style::semibold(14.0)
    } else {
        style::regular(14.0)
    };
    let color = if last { th.text } else { th.dim };
    let galley = w::fit(ui.painter(), text.to_string(), font, color, 280.0);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 14.0, 30.0), Sense::click());
    if response.hovered() && !last {
        ui.painter().rect_filled(rect, 8, th.hover);
    }
    ui.painter().galley(
        pos2(rect.left() + 7.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response
}

fn crumb_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 30.0), Sense::hover());
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(10.0, 10.0)),
        Icon::Chevron,
        t().faint,
    );
}

fn place_row(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    active: bool,
    tint: Option<Color32>,
) -> egui::Response {
    let th = t();
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let p = ui.painter();
    w::row_fill(p, rect, response.hovered(), active);
    let tile = Rect::from_min_size(rect.left_top() + vec2(8.0, 6.0), vec2(24.0, 24.0));
    match tint {
        Some(color) => {
            p.rect_filled(tile, 7, color);
            icons::paint(p, tile.shrink(5.5), icon, th.ink);
        }
        None => icons::paint(p, tile.shrink(4.0), icon, th.dim),
    }
    p.text(
        pos2(tile.right() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        if active {
            style::semibold(13.0)
        } else {
            style::regular(13.0)
        },
        if tint.is_some() { th.text } else { th.dim },
    );
    response
}

fn filter_chip(ui: &mut Ui, label: &str, count: Option<usize>, active: bool) -> egui::Response {
    let th = t();
    let text = match count {
        Some(n) => format!("{label}  ·  {n}"),
        None => label.to_string(),
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text, style::semibold(12.0), Color32::PLACEHOLDER);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 22.0, 30.0), Sense::click());
    let (fill, fg) = if active {
        (th.text, th.bg)
    } else if response.hovered() {
        (th.hover, th.text)
    } else {
        (th.card, th.dim)
    };
    ui.painter().rect_filled(rect, 15, fill);
    ui.painter().galley(
        pos2(rect.left() + 11.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    response
}

fn drive_row(ui: &mut Ui, volume: &Volume) -> Option<Action> {
    let th = t();
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 80.0), Sense::click());
    let p = ui.painter().clone();
    p.rect_filled(
        rect,
        16,
        if response.hovered() {
            th.hover
        } else {
            th.card
        },
    );
    let tile = Rect::from_min_size(rect.left_top() + vec2(16.0, 17.0), vec2(46.0, 46.0));
    p.rect_filled(
        tile,
        13,
        style::alpha(th.accent, if th.dark { 50 } else { 36 }),
    );
    icons::paint(&p, tile.shrink(12.0), Icon::Drive, th.accent);
    let letter = volume.root.display().to_string();
    let label = if volume.label.is_empty() {
        "Local Disk"
    } else {
        &volume.label
    };
    p.text(
        pos2(tile.right() + 16.0, rect.top() + 20.0),
        Align2::LEFT_TOP,
        format!("{label} ({})", letter.trim_end_matches('\\')),
        style::semibold(15.0),
        th.text,
    );
    p.text(
        pos2(tile.right() + 16.0, rect.top() + 42.0),
        Align2::LEFT_TOP,
        format!(
            "{} · {}",
            style::bytes(volume.total_bytes),
            volume.filesystem
        ),
        style::regular(12.5),
        th.dim,
    );
    let used = volume.total_bytes.saturating_sub(volume.free_bytes);
    let share = used as f32 / volume.total_bytes.max(1) as f32;
    let bar_w = (width * 0.3).clamp(140.0, 240.0);
    let bar = Rect::from_min_size(
        pos2(rect.right() - 110.0 - bar_w, rect.center().y - 10.0),
        vec2(bar_w, 8.0),
    );
    w::progress(&p, bar, share, usage_color(share));
    p.text(
        bar.left_bottom() + vec2(0.0, 6.0),
        Align2::LEFT_TOP,
        format!("{} used", style::bytes(used)),
        style::regular(11.5),
        th.faint,
    );
    p.text(
        bar.right_bottom() + vec2(0.0, 6.0),
        Align2::RIGHT_TOP,
        format!("{} free", style::bytes(volume.free_bytes)),
        style::semibold(11.5),
        if share > 0.9 { th.danger } else { th.good },
    );
    let mut button = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(
        pos2(rect.right() - 92.0, rect.center().y - 16.0),
        pos2(rect.right() - 16.0, rect.center().y + 16.0),
    )));
    let clicked = w::button(&mut button, Kind::Primary, None, "Scan").clicked();
    (clicked || response.clicked()).then_some(Action::Scan(letter))
}

fn scanning(ui: &mut Ui, running: &Running) -> Vec<Action> {
    let th = t();
    let elapsed = running.started.elapsed().as_secs_f64();
    let files = running.control.files.load(Ordering::Relaxed);
    let dirs = running.control.directories.load(Ordering::Relaxed);
    let bytes = running.control.bytes.load(Ordering::Relaxed);
    let full = ui.available_rect_before_wrap();
    ui.allocate_rect(full, Sense::hover());
    let p = ui.painter().clone();
    let center = pos2(full.center().x, full.top() + full.height() * 0.36);
    let r = 74.0;
    w::arc(&p, center, r, 10.0, 0.0, TAU, th.field);
    let spin = (ui.input(|i| i.time) as f32 * 2.4) % TAU;
    w::arc(&p, center, r, 10.0, spin, spin + TAU * 0.28, th.accent);
    w::arc(
        &p,
        center,
        r,
        10.0,
        spin + TAU * 0.5,
        spin + TAU * 0.56,
        style::alpha(th.accent, 120),
    );
    p.text(
        center + vec2(0.0, -6.0),
        Align2::CENTER_CENTER,
        style::short_count(files),
        style::semibold(26.0),
        th.text,
    );
    p.text(
        center + vec2(0.0, 18.0),
        Align2::CENTER_CENTER,
        "files",
        style::regular(12.5),
        th.faint,
    );
    let mut y = center.y + r + 34.0;
    p.text(
        pos2(center.x, y),
        Align2::CENTER_TOP,
        "Scanning",
        style::semibold(24.0),
        th.text,
    );
    y += 34.0;
    let g = w::fit(
        &p,
        running.root.clone(),
        style::mono(12.5),
        th.dim,
        full.width() - 80.0,
    );
    p.galley(pos2(center.x - g.size().x / 2.0, y), g, th.dim);
    y += 34.0;
    let stats = [
        (style::short_count(dirs), "folders"),
        (style::bytes(bytes), "found"),
        (
            format!(
                "{}/s",
                style::short_count((files as f64 / elapsed.max(0.001)) as u64)
            ),
            "files per second",
        ),
        (format!("{elapsed:.1} s"), "elapsed"),
    ];
    let cw = 150.0;
    let gap = 12.0;
    let x0 = center.x - (cw * 4.0 + gap * 3.0) / 2.0;
    for (i, (value, label)) in stats.iter().enumerate() {
        let rect = Rect::from_min_size(pos2(x0 + i as f32 * (cw + gap), y), vec2(cw, 66.0));
        p.rect_filled(rect, 14, th.card);
        p.text(
            rect.center() + vec2(0.0, -9.0),
            Align2::CENTER_CENTER,
            value,
            style::semibold(17.0),
            th.text,
        );
        p.text(
            rect.center() + vec2(0.0, 14.0),
            Align2::CENTER_CENTER,
            *label,
            style::regular(11.5),
            th.faint,
        );
    }
    y += 90.0;
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_center_size(
                pos2(center.x, y + 16.0),
                vec2(260.0, 34.0),
            ))
            .layout(Layout::top_down(Align::Center)),
    );
    if w::button(
        &mut child,
        Kind::Soft,
        Some(Icon::Stop),
        "Stop and keep results",
    )
    .on_hover_text("Esc")
    .clicked()
    {
        running.control.cancel();
    }
    p.text(
        pos2(center.x, y + 52.0),
        Align2::CENTER_TOP,
        "Reads file metadata only. Nothing is opened, changed or uploaded.",
        style::regular(12.0),
        th.faint,
    );
    ui.ctx().request_repaint();
    Vec::new()
}

fn error_banner(ui: &mut Ui, error: &str, data: Option<&Data>) {
    let th = t();
    Frame::new()
        .fill(style::alpha(th.danger, if th.dark { 40 } else { 26 }))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let text = match data {
                Some(data) => format!(
                    "Scan failed: {error}. Still showing the previous scan of {}.",
                    data.snapshot.summary.root.display()
                ),
                None => format!("Scan failed: {error}"),
            };
            ui.label(RichText::new(text).color(th.danger));
        });
}

fn empty_state(ui: &mut Ui, icon: Icon, title: &str, text: &str) {
    let th = t();
    ui.vertical_centered(|ui| {
        ui.add_space(60.0);
        let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
        ui.painter().rect_filled(rect, 16, th.card);
        icons::paint(ui.painter(), rect.shrink(16.0), icon, th.dim);
        ui.add_space(12.0);
        ui.label(RichText::new(title).font(style::semibold(18.0)));
        ui.label(RichText::new(text).color(th.dim));
    });
}

fn entry_tooltip(ui: &mut Ui, data: &Data, id: usize, allocated: bool, view_total: u64) {
    let th = t();
    let snapshot = &data.snapshot;
    let entry = &snapshot.entries[id];
    let size = treemap::size_of(snapshot, id, allocated);
    ui.set_max_width(380.0);
    ui.label(RichText::new(snapshot.name(id)).font(style::semibold(14.0)));
    ui.label(
        RichText::new(snapshot.path(id).display().to_string())
            .font(style::regular(11.5))
            .color(th.faint),
    );
    ui.add_space(4.0);
    let mut line = format!(
        "{}  ·  {:.1}% of view",
        style::bytes(size),
        size as f64 * 100.0 / view_total.max(1) as f64
    );
    if entry.kind == EntryKind::Directory {
        line.push_str(&format!("  ·  {}", style::plural(entry.file_count, "file")));
    }
    ui.label(RichText::new(line).font(style::semibold(12.5)));
    ui.label(
        RichText::new(format!(
            "Changed {}",
            style::age(now_ms(), entry.modified_unix_ms)
        ))
        .color(th.dim),
    );
    if let Some(&c) = data.candidate_of.get(&id) {
        let c = &data.reclaim.candidates[c];
        ui.add_space(4.0);
        let mut text = format!("{} · {}", c.label, c.safety.label());
        if let Some(agent) = c.agent {
            text.push_str(&format!(" · {agent}"));
        }
        ui.label(
            RichText::new(text)
                .font(style::semibold(12.5))
                .color(style::safety_color(c.safety)),
        );
    }
    ui.add_space(2.0);
    ui.label(
        RichText::new(if entry.kind == EntryKind::Directory {
            "Click to open · right-click for more"
        } else {
            "Right-click for more"
        })
        .font(style::regular(11.0))
        .color(th.faint),
    );
}

fn entry_menu(ui: &mut Ui, snapshot: &Snapshot, id: usize, actions: &mut Vec<Action>) {
    let entry = &snapshot.entries[id];
    if ui.button("Show in Explore").clicked() {
        actions.push(Action::Explore(id));
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

/// Screen position of an entry in the visible chart, for scripted tours.
pub fn pointer_for(app: &RillApp, id: usize) -> Option<Pos2> {
    match (app.tab, app.chart) {
        (Tab::Overview, _) => app.mini.tile_of(id).map(|t| t.rect.center()),
        (_, Chart::Map) => app.map.tile_of(id).map(|t| t.rect.center()),
        (_, Chart::Rings) => app.rings.segment_of(id).map(|s| app.rings.anchor(s)),
    }
}
