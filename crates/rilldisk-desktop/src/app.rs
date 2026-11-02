// SPDX-License-Identifier: AGPL-3.0-only
pub(crate) mod nav;
mod settings_page;

use crate::capture::Tour;
use crate::icons::{self, Icon};
use crate::live;
use crate::settings::{self, Prefs};
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
    Candidate, Category, EntryKind, LiveView, ReclaimReport, Safety, ScanControl, ScanOptions,
    Snapshot, TypeStat, Volume, file_types, find_reclaimable, largest_files, scan, volumes,
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
    pub(crate) fn analyze(snapshot: Snapshot) -> Self {
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
    /// Scripted views by focused folder, standing in for a scan in capture tours.
    preview: Option<HashMap<usize, Arc<LiveView>>>,
}

impl Running {
    fn live(&self) -> Option<Arc<LiveView>> {
        match &self.preview {
            Some(views) => views.get(&self.control.live_focus()).cloned(),
            None => self.control.live(),
        }
    }
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
    /// Sidebar or top-bar theme toggle: a fixed light or dark choice.
    Theme(bool),
    /// Settings: None follows the system theme.
    ThemeChoice(Option<bool>),
    Identity(Identity),
    /// Default size metric (Size or On disk).
    Metric(bool),
    /// Show a place or drive: inside the current results when they cover it,
    /// otherwise scan it.
    Open(String),
    Settings(bool),
    /// The user agreed to install this update.
    Install(Box<update::Release>),
    CheckUpdates,
}

/// Scan generation, folder and metric of a sorted child list.
type FolderRowsKey = (usize, usize, bool);

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
    /// Entry to scroll into view in the folder panel.
    reveal: Option<usize>,
    /// The shown folder's children, sorted by the current metric.
    folder_rows: Option<(FolderRowsKey, Arc<Vec<u32>>)>,
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
    prefs: Prefs,
    pub settings_open: bool,
    applied: Option<(Identity, bool)>,
    live_anim: live::Anim,
    /// A place picked during a scan that covers it: opened when the scan ends.
    pending_open: Option<String>,
    title: String,
    /// Background update check, its result and a running install.
    update_check: Option<update::Pending<Result<Option<update::Release>, String>>>,
    update_manual: bool,
    pub available: Option<update::Release>,
    installing: Option<update::Install>,
    update_error: Option<String>,
    update_notice: Option<String>,
    next_update_check: Instant,
    after_scan: Option<(Tab, Option<Category>)>,
    pub tour: Option<Tour>,
    pub injected: Vec<egui::Event>,
    /// Selection removal: confirmation, progress and results (cleanup_ui.rs).
    pub cleanup: crate::cleanup_ui::CleanupUi,
    assistants: crate::assistants::State,
    pub elevate: crate::elevation::Elevate,
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
        // `--theme` wins, then the saved choice; otherwise follow the system.
        let theme = if dark.is_some() {
            dark
        } else {
            saved.theme.unwrap_or(None)
        };
        let dark = theme.unwrap_or(cc.egui_ctx.system_theme() != Some(egui::Theme::Light));
        let allocated = saved.allocated.unwrap_or(false);
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
            allocated,
            tab: Tab::Overview,
            chart: Chart::Map,
            color_by: ColorBy::Folder,
            selected: None,
            zoom: 0,
            reveal: None,
            folder_rows: None,
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
            prefs: Prefs {
                identity,
                theme,
                allocated,
            },
            settings_open: false,
            applied: Some((identity, dark)),
            live_anim: live::Anim::default(),
            pending_open: None,
            title: String::new(),
            update_check: None,
            update_manual: false,
            available: None,
            installing: None,
            update_error: None,
            update_notice: None,
            next_update_check: Instant::now() + std::time::Duration::from_secs(900),
            after_scan: None,
            tour,
            injected: Vec::new(),
            cleanup: Default::default(),
            assistants: Default::default(),
            elevate: crate::elevation::Elevate::new(),
        };
        if path.is_some() {
            app.start_scan(&cc.egui_ctx);
        }
        // Scripted captures never touch the network.
        if app.tour.is_none() {
            match update::installer_notice() {
                Some(Ok(notice)) => app.update_notice = Some(notice),
                Some(Err(error)) => app.update_error = Some(error),
                None => {}
            }
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
        self.live_anim.reset();
        self.running = Some(Running {
            control,
            root,
            started: Instant::now(),
            rx,
            preview: None,
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(running) = &self.running {
            match running.rx.try_recv() {
                Ok(Ok(data)) => {
                    let long = running.started.elapsed().as_secs() >= 10;
                    let id = running.control.live_focus();
                    // A rescan of the shown results keeps the user where they are.
                    let location = self
                        .data
                        .as_ref()
                        .filter(|_| {
                            self.zoom != 0 && self.rescanning() && self.pending_open.is_none()
                        })
                        .map(|data| data.snapshot.path(self.zoom).display().to_string());
                    let focus = (id != 0
                        && self.pending_open.is_none()
                        && data
                            .snapshot
                            .entries
                            .get(id)
                            .is_some_and(|e| e.kind == EntryKind::Directory))
                    .then_some(id);
                    self.running = None;
                    self.install(data);
                    if let Some(id) = focus {
                        self.zoom = id;
                        self.tab = Tab::Explore;
                    }
                    if let Some(path) = location {
                        let found = self
                            .data
                            .as_ref()
                            .and_then(|data| find_path(&data.snapshot, &path));
                        match found {
                            Some(id) => self.zoom = id,
                            None => {
                                self.toast = Some((
                                    "That folder is gone. Showing the top of the scan".into(),
                                    ctx.input(|i| i.time),
                                ));
                            }
                        }
                    }
                    if long && !ctx.input(|i| i.viewport().focused.unwrap_or(true)) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                            egui::UserAttentionType::Informational,
                        ));
                    }
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
        if self.tour.is_none()
            && self.update_check.is_none()
            && self.installing.is_none()
            && Instant::now() >= self.next_update_check
        {
            self.next_update_check = Instant::now() + std::time::Duration::from_secs(900);
            self.update_check = Some(update::check(ctx, update::Trigger::Automatic));
        }
        if self.tour.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_secs(60));
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

    pub(crate) fn install(&mut self, data: Data) {
        self.selected = None;
        self.reveal = None;
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
        if let Some(path) = self.pending_open.take() {
            self.open_in_results(&path);
        }
    }

    /// Show `path` inside the current results. False when they do not cover it.
    fn open_in_results(&mut self, path: &str) -> bool {
        let Some(data) = &self.data else { return false };
        let Some(id) = find_path(&data.snapshot, path) else {
            return false;
        };
        if id == 0 {
            if self.tab != Tab::Explore {
                self.tab = Tab::Overview;
            }
        } else {
            self.tab = Tab::Explore;
        }
        self.zoom = id;
        self.selected = None;
        true
    }

    fn save_prefs(&mut self) {
        self.prefs.identity = self.identity;
        self.prefs.allocated = self.allocated;
        settings::save(&self.prefs);
    }

    fn size(&self, snapshot: &Snapshot, id: usize) -> u64 {
        treemap::size_of(snapshot, id, self.allocated)
    }

    /// No size can be stated: the metric is missing, or nothing could be listed.
    fn size_unknown(&self, snapshot: &Snapshot, id: usize) -> bool {
        let entry = &snapshot.entries[id];
        (self.allocated && entry.allocated_bytes.is_none())
            || (!entry.fully_listed && snapshot.children(id).is_empty())
    }

    fn size_text(&self, snapshot: &Snapshot, id: usize) -> String {
        if self.size_unknown(snapshot, id) {
            "unknown".to_string()
        } else {
            style::bytes(self.size(snapshot, id))
        }
    }

    /// Every child of the shown folder, largest first; nothing is hidden, so the
    /// item count, the list and arrow-key navigation agree.
    fn children_of_zoom(&mut self, snapshot: &Snapshot) -> Arc<Vec<u32>> {
        let key = (self.generation, self.zoom, self.allocated);
        if let Some((cached, rows)) = &self.folder_rows
            && *cached == key
        {
            return rows.clone();
        }
        let mut rows = snapshot.children(self.zoom).to_vec();
        if self.allocated {
            rows.sort_by_key(|&id| {
                std::cmp::Reverse(snapshot.entries[id as usize].allocated_bytes)
            });
        }
        let rows = Arc::new(rows);
        self.folder_rows = Some((key, rows.clone()));
        rows
    }

    /// Scan root to `id`, for the location bar and the folder panel.
    fn location(&self, snapshot: &Snapshot, id: usize) -> Vec<nav::Place> {
        ancestors(snapshot, id)
            .into_iter()
            .map(|level| {
                let detail = snapshot.path(level).display().to_string();
                let name = if level == 0 {
                    nav::root_name(&detail)
                } else {
                    snapshot.name(level).into_owned()
                };
                nav::Place {
                    id: level,
                    name,
                    detail,
                    size: self.size_text(snapshot, level),
                }
            })
            .collect()
    }

    /// A scan is refreshing the results on screen, which stay usable meanwhile.
    fn rescanning(&self) -> bool {
        match (&self.data, &self.running) {
            (Some(data), Some(running)) => same_path(
                &data.snapshot.summary.root.display().to_string(),
                &running.root,
            ),
            _ => false,
        }
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
                Action::Select(id) => {
                    self.selected = Some(id);
                    self.reveal = Some(id);
                }
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
                        // Going back up marks the branch the user came from.
                        let came_from = nav::child_under(
                            |at| data.snapshot.entries[at].parent_id,
                            self.zoom,
                            folder,
                        );
                        self.tab = Tab::Explore;
                        self.zoom = folder;
                        self.selected = (folder != id).then_some(id).or(came_from);
                    }
                    self.reveal = self.selected;
                }
                Action::Up => {
                    if let Some(data) = &self.data
                        && let Some(parent) = data.snapshot.entries[self.zoom].parent_id
                    {
                        self.selected = Some(self.zoom);
                        self.reveal = self.selected;
                        self.zoom = parent;
                    }
                }
                Action::Copy(text, what) => {
                    ctx.copy_text(text);
                    self.toast = Some((format!("Copied {what}"), ctx.input(|i| i.time)));
                }
                Action::Tab(tab) => self.tab = tab,
                Action::Scan(_) | Action::ScanThen(..) | Action::Open(_)
                    if self.installing.is_some() => {}
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
                    self.prefs.theme = Some(dark);
                    self.save_prefs();
                }
                Action::ThemeChoice(choice) => {
                    self.prefs.theme = choice;
                    self.dark = choice.unwrap_or(ctx.system_theme() != Some(egui::Theme::Light));
                    self.save_prefs();
                }
                Action::Identity(identity) => {
                    self.identity = identity;
                    self.save_prefs();
                }
                Action::Metric(allocated) => {
                    self.allocated = allocated;
                    self.save_prefs();
                }
                Action::Settings(open) => self.settings_open = open,
                Action::Open(path) => {
                    self.settings_open = false;
                    if self.open_in_results(&path) {
                        continue;
                    }
                    let now = ctx.input(|i| i.time);
                    match &self.running {
                        Some(running) if covers(&running.root, &path) => {
                            self.pending_open = Some(path);
                            self.toast = Some(("Opens here when the scan finishes".into(), now));
                        }
                        Some(_) => {
                            self.toast =
                                Some(("A scan is running. Stop it first (Esc)".into(), now));
                        }
                        None => {
                            self.path = path;
                            self.start_scan(ctx);
                        }
                    }
                }
                Action::Install(_)
                    if self.installing.is_some()
                        || self.running.is_some()
                        || self.cleanup.modal_open() =>
                {
                    self.toast = Some((
                        "Finish the current scan or cleanup before updating".into(),
                        ctx.input(|i| i.time),
                    ));
                }
                Action::Install(_) if self.tour.is_some() => {
                    let sender = self.preview_update(update::Preview::Downloading);
                    if let Some(tour) = &mut self.tour {
                        tour.update_sender = sender;
                    }
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
        if self.cleanup.modal_open() {
            return actions;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.settings_open {
                actions.push(Action::Settings(false));
            } else if let Some(running) = &self.running {
                running.control.cancel();
            }
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
        let live_dashboard = self.running.is_some() && !self.rescanning();
        if ctx.egui_wants_keyboard_input() || live_dashboard || self.settings_open {
            return actions;
        }
        let snapshot = &data.snapshot;
        // Only Explore shows a folder; elsewhere going up would be invisible.
        if self.tab == Tab::Explore
            && ctx.input(|i| {
                i.key_pressed(Key::Backspace)
                    || i.pointer.button_pressed(egui::PointerButton::Extra1)
            })
        {
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
            let rows = self.children_of_zoom(snapshot);
            let (up, down, enter) = ctx.input(|i| {
                (
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::Enter) || i.key_pressed(Key::ArrowRight),
                )
            });
            if (up || down) && !rows.is_empty() {
                let position = self
                    .selected
                    .and_then(|s| rows.iter().position(|&r| r as usize == s));
                let next = match position {
                    None => 0,
                    Some(p) if up => p.saturating_sub(1),
                    Some(p) => (p + 1).min(rows.len() - 1),
                };
                actions.push(Action::Select(rows[next] as usize));
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
        // What the main area shows: the closest drive or place containing it is lit.
        let viewing = match (&self.data, &self.running) {
            (_, Some(running)) => running.root.clone(),
            (Some(data), None) => data.snapshot.path(self.zoom).display().to_string(),
            (None, None) => String::new(),
        };
        let located = self
            .volumes
            .iter()
            .map(|v| v.root.display().to_string())
            .chain(self.places.iter().map(|(_, _, path)| path.clone()))
            .filter(|path| !viewing.is_empty() && covers(path, &viewing))
            .max_by_key(|path| path.trim_end_matches(['\\', '/']).len());
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
            logo(ui.painter(), rect);
            ui.add_space(2.0);
            ui.label(RichText::new("RillDisk").font(style::semibold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let (icon, tip) = if self.dark {
                    (Icon::Sun, "Light theme")
                } else {
                    (Icon::Moon, "Dark theme")
                };
                if w::icon_button(ui, icon, tip).clicked() {
                    actions.push(Action::Theme(!self.dark));
                }
            });
        });
        ui.add_space(20.0);
        // Settings and the status lines below stay put however many drives there are.
        let footer_height = 96.0;
        egui::ScrollArea::vertical()
            .id_salt("sidebar-places")
            .max_height((ui.available_height() - footer_height).max(0.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                w::section_label(ui, "Drives");
                ui.add_space(2.0);
                for volume in &self.volumes {
                    let letter = volume.root.display().to_string();
                    let active = located.as_deref() == Some(letter.as_str());
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click());
                    let p = ui.painter();
                    w::row_fill(p, rect, response.hovered(), active);
                    let tile =
                        Rect::from_min_size(rect.left_top() + vec2(8.0, 10.0), vec2(30.0, 30.0));
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
                    let bar =
                        Rect::from_min_size(pos2(text_left, rect.bottom() - 7.0), vec2(width, 3.0));
                    w::progress(p, bar, share, usage_color(share));
                    if response
                        .on_hover_text(format!(
                            "{} used of {} · {}\nClick to open",
                            style::bytes(used),
                            style::bytes(volume.total_bytes),
                            volume.filesystem
                        ))
                        .clicked()
                    {
                        actions.push(Action::Open(letter));
                    }
                }
                ui.add_space(16.0);
                w::section_label(ui, "Places");
                ui.add_space(2.0);
                for (i, (name, icon, path)) in self.places.iter().enumerate() {
                    let active = located.as_deref() == Some(path.as_str());
                    let row = place_row(
                        ui,
                        *icon,
                        name,
                        active,
                        Some(style::chart(i as f32 / 5.0, 0)),
                    )
                    .on_hover_text(format!("{path}\nRight-click to scan only this folder"));
                    if row.clicked() {
                        actions.push(Action::Open(path.clone()));
                    }
                    row.context_menu(|ui| {
                        if ui.button("Scan only this folder").clicked() {
                            actions.push(Action::Scan(path.clone()));
                            ui.close();
                        }
                    });
                }
                if place_row(ui, Icon::Folder, "Choose folder…", false, None)
                    .on_hover_text("Ctrl+O")
                    .clicked()
                    && let Some(folder) = rfd::FileDialog::new().pick_folder()
                {
                    actions.push(Action::Scan(folder.display().to_string()));
                }
            });
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    RichText::new(concat!("v", env!("CARGO_PKG_VERSION"), " · AGPL-3.0 ·"))
                        .font(style::regular(11.0))
                        .color(th.faint),
                );
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, th.good);
                ui.label(
                    RichText::new("Local only · nothing leaves this PC")
                        .font(style::regular(11.5))
                        .color(th.dim),
                );
            });
            ui.add_space(8.0);
            if place_row(ui, Icon::Gear, "Settings", self.settings_open, None).clicked() {
                actions.push(Action::Settings(!self.settings_open));
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
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let root = snapshot.summary.root.display().to_string();
                if w::icon_button(ui, Icon::Refresh, "Rescan (F5)").clicked() {
                    actions.push(Action::Scan(root));
                }
                let mut allocated = self.allocated;
                if w::segmented(
                    ui,
                    &mut allocated,
                    &[(false, None, "Size"), (true, None, "On disk")],
                    "Size: file length. On disk: allocated clusters, hard links counted once.",
                ) {
                    actions.push(Action::Metric(allocated));
                }
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
                        format!("Partial · {}", style::plural(s.issue_count, "issue")),
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
                // Restart (or the Administrator badge) right beside the status chip.
                let scanning = self.running.is_some();
                self.elevate
                    .header(ui, s, scanning, self.tab != Tab::Issues);
                // The scan root takes the room the controls leave.
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let root = s.root.display().to_string();
                    let font = style::semibold(15.0);
                    let text = nav::fit_path(ui, &root, font.clone(), ui.available_width() - 12.0);
                    ui.label(RichText::new(text).font(font).color(th.text))
                        .on_hover_text(root);
                });
            });
        });
        let s = &snapshot.summary;
        ui.label(RichText::new(format!("Scan logical: {} · unique: {} · allocated: {} · backend: {}",
            style::bytes(s.logical_bytes), style::bytes(s.unique_logical_bytes),
            s.allocated_bytes.map(style::bytes).unwrap_or_else(|| "unknown".into()), s.backend)).small().color(th.dim))
            .on_hover_text(format!("Exact bytes: logical {} · unique {} · allocated {:?}. Logical includes every hard-link name; allocated counts identities once. Sparse/compressed files differ. Partial scans omit unreadable paths. Volume used space also includes filesystem metadata, snapshots and unscanned files.", s.logical_bytes, s.unique_logical_bytes, s.allocated_bytes));
        if let Some(v) = self
            .volumes
            .iter()
            .find(|v| same_path(&v.root.display().to_string(), &s.root.display().to_string()))
        {
            ui.label(RichText::new(format!("Volume used: {} · free: {} · capacity: {} (volume information at app startup)", style::bytes(v.total_bytes.saturating_sub(v.free_bytes)), style::bytes(v.free_bytes), style::bytes(v.total_bytes))).small().color(th.dim))
                .on_hover_text(format!("Volume used: {} bytes. Compare this field with Explorer drive properties. File totals above measure different things.", v.total_bytes.saturating_sub(v.free_bytes)));
        }
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
        // A scan of something the current results do not show: draw it live.
        if self.running.is_some() && !self.rescanning() {
            return self.live_dashboard(ui);
        }
        let Some(data) = self.data.clone() else {
            return actions;
        };
        actions.extend(self.header(ui, &data));
        ui.add_space(12.0);
        if let Some(error) = &self.error {
            error_banner(ui, error, Some(&data));
            ui.add_space(10.0);
        }
        // Rescanning the same folder: keep the results usable, show progress.
        if let Some(running) = &self.running {
            let progress = live::Progress {
                files: running.control.files.load(Ordering::Relaxed),
                folders: running.control.directories.load(Ordering::Relaxed),
                bytes: running.control.bytes.load(Ordering::Relaxed),
                seconds: running.started.elapsed().as_secs_f64(),
            };
            let control = running.control.clone();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Rescanning · {}", progress.summary()))
                        .font(style::regular(12.0))
                        .color(t().dim),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::button(ui, Kind::Ghost, Some(Icon::Stop), "Stop")
                        .on_hover_text("Stop and keep what was found (Esc)")
                        .clicked()
                    {
                        control.cancel();
                    }
                });
            });
            let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), Sense::hover());
            live::sweep(ui.painter(), bar, ui.input(|i| i.time), t().accent);
            ui.add_space(10.0);
            ui.ctx().request_repaint();
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

    /// A scan in progress: its root, live numbers and the folders found so far.
    fn live_dashboard(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let Some(running) = &self.running else {
            return actions;
        };
        let control = running.control.clone();
        let root = running.root.clone();
        let progress = live::Progress {
            files: control.files.load(Ordering::Relaxed),
            folders: control.directories.load(Ordering::Relaxed),
            bytes: control.bytes.load(Ordering::Relaxed),
            seconds: running.started.elapsed().as_secs_f64(),
        };
        let view = running.live();
        ui.horizontal(|ui| {
            ui.set_height(34.0);
            let (rect, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
            // A drive root gets the drive icon, any other folder a folder.
            let is_drive = self
                .volumes
                .iter()
                .any(|v| same_path(&v.root.display().to_string(), &root));
            let icon = if is_drive { Icon::Drive } else { Icon::Folder };
            icons::paint(ui.painter(), rect, icon, th.dim);
            ui.add_space(4.0);
            ui.label(RichText::new("Scan in progress").font(style::semibold(16.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if w::button(ui, Kind::Soft, Some(Icon::Stop), "Stop")
                    .on_hover_text("Stop and keep what was found (Esc)")
                    .clicked()
                {
                    control.cancel();
                }
                let mut allocated = self.allocated;
                if w::segmented(
                    ui,
                    &mut allocated,
                    &[(false, None, "Size"), (true, None, "On disk")],
                    "Size: file length. On disk: allocated clusters, hard links counted once.",
                ) {
                    actions.push(Action::Metric(allocated));
                }
                let label = if control.is_cancelled() {
                    "Stopping…"
                } else {
                    "Scanning"
                };
                w::chip(ui, label, th.accent);
            });
        });
        ui.add_space(4.0);
        let places = view
            .as_deref()
            .map_or_else(Vec::new, |view| live::location(&root, view, self.allocated));
        if let Some(view) = &view {
            let focus = control.live_focus();
            let parent = live::parent(Some(view), focus);
            let status = if live::caught_up(view, focus) {
                format!("{} so far", style::bytes(live::total(view, self.allocated)))
            } else {
                "Opening folder…".to_string()
            };
            let go = nav::location_bar(ui, &places, parent.is_some(), &status);
            let keys_go_up = !ui.ctx().egui_wants_keyboard_input()
                && ui.input(|i| {
                    i.key_pressed(Key::Backspace)
                        || i.key_pressed(Key::ArrowLeft)
                        || i.pointer.button_pressed(egui::PointerButton::Extra1)
                });
            let target = match (go, parent) {
                (Some(nav::Go::To(id)), _) => Some(id),
                (Some(nav::Go::Parent), parent) => parent,
                (None, parent) if keys_go_up => parent,
                (None, _) => None,
            };
            if let Some(id) = target {
                self.live_anim.open(&control, id, Some(view));
            }
        }
        ui.label(RichText::new(control.backend_status()).small().color(th.dim))
            .on_hover_text("Auto uses the fast MFT backend for whole NTFS volumes when existing administrator access permits it. Other paths use a directory walk. Standard-access whole-drive scans may be much slower; restart as administrator from Settings before scanning if desired.");
        ui.label(
            RichText::new(progress.summary())
                .font(style::regular(12.5))
                .color(th.dim),
        );
        ui.add_space(10.0);
        let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
        live::sweep(ui.painter(), bar, ui.input(|i| i.time), th.accent);
        ui.add_space(16.0);
        let full = ui.available_rect_before_wrap();
        let area = Rect::from_min_max(full.min, egui::pos2(full.right(), full.bottom() - 26.0));
        if let Some(id) = live::paint(
            ui,
            area,
            view.as_deref(),
            &places,
            &mut self.live_anim,
            self.allocated,
        ) {
            self.live_anim.open(&control, id, view.as_deref());
        }
        ui.painter().text(
            egui::pos2(full.left() + 4.0, full.bottom() - 4.0),
            Align2::LEFT_BOTTOM,
            if view.is_some() { "Partial sizes grow as folders are read. Click folders to browse; Parent, Backspace or Left goes up. On disk is file allocation, not volume used space." } else { "Reading metadata. MFT scans publish folders after table processing; directory scans publish as batches arrive." },
            style::regular(11.5),
            th.faint,
        );
        ui.allocate_rect(full, Sense::hover());
        ui.ctx().request_repaint();
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
                        used,
                        format!("volume used of {}", style::bytes(v.total_bytes)),
                        format!("Scanned files {}", style::bytes(total)),
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
                w::progress(&p, bar, (*size / top_size) as f32, color);
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
        let snapshot = &data.snapshot;
        let status = if self.size_unknown(snapshot, self.zoom) {
            "Size unknown".to_string()
        } else {
            format!(
                "{} · {}",
                self.size_text(snapshot, self.zoom),
                style::plural(snapshot.entries[self.zoom].file_count, "file")
            )
        };
        let places = self.location(snapshot, self.zoom);
        match nav::location_bar(ui, &places, self.zoom != 0, &status) {
            Some(nav::Go::Parent) => actions.push(Action::Up),
            Some(nav::Go::To(id)) => actions.push(Action::Zoom(id)),
            None => {}
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            self.chart_controls(ui);
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
        actions.extend(self.side_panel(ui, data, &places, panel_rect, th.card));
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
                        nothing_to_draw(snapshot, self.zoom),
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
                let number = rings.center + vec2(0.0, -6.0);
                let number_size = (rings.inner * 0.3).clamp(16.0, 28.0);
                if self.size_unknown(snapshot, focus) {
                    painter.text(
                        number,
                        Align2::CENTER_CENTER,
                        "unknown",
                        style::semibold(number_size * 0.7),
                        th.dim,
                    );
                } else {
                    w::big_bytes(
                        &painter,
                        number,
                        Align2::CENTER_CENTER,
                        focus_size,
                        number_size,
                        th.text,
                    );
                }
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
                        nothing_to_draw(snapshot, self.zoom),
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

    /// The folder tree around the shown folder. It always lists the shown folder, so
    /// rows do not change under the pointer on its way across the chart.
    fn side_panel(
        &mut self,
        ui: &mut Ui,
        data: &Data,
        places: &[nav::Place],
        rect: Rect,
        fill: Color32,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        let snapshot = &data.snapshot;
        ui.painter().rect_filled(rect, 16, fill);
        let inner = rect.shrink(18.0);
        let p = ui.painter().clone();
        let rows = self.children_of_zoom(snapshot);
        let entry = &snapshot.entries[self.zoom];
        let footer_h = 58.0;
        let body = Rect::from_min_max(
            pos2(rect.left() + 6.0, rect.top() + 8.0),
            pos2(rect.right() - 6.0, rect.bottom() - footer_h),
        );
        let colors: HashMap<u32, Color32> = match self.chart {
            Chart::Map => self.map.tiles.iter().map(|t| (t.id, t.color)).collect(),
            Chart::Rings => self
                .rings
                .segments
                .iter()
                .map(|s| (s.id, s.color))
                .collect(),
        };
        let parent_of = |at: usize| snapshot.entries[at].parent_id;
        let issues = snapshot.summary.issue_count > 0;
        let notice = (!entry.fully_listed).then(|| nav::Notice {
            text: if issues {
                "Not fully read, so sizes here are partial. See Issues".to_string()
            } else {
                "Not fully read, so sizes here are partial".to_string()
            },
            action: issues,
        });
        let spec = nav::Panel {
            places,
            summary: if self.size_unknown(snapshot, self.zoom) && rows.is_empty() {
                "Contents unknown".to_string()
            } else {
                format!(
                    "{} · {}",
                    style::plural(entry.file_count, "file"),
                    style::plural(rows.len() as u64, "item")
                )
            },
            notice,
            rows: rows.len(),
            empty: if entry.fully_listed {
                "This folder is empty"
            } else {
                "Nothing could be listed here"
            },
            selected: self.selected,
            hovered: self
                .hovered
                .and_then(|id| nav::child_under(parent_of, id, self.zoom)),
            reveal: self
                .reveal
                .take()
                .and_then(|id| rows.iter().position(|&r| r as usize == id)),
        };
        let row = |index: usize| {
            let id = rows[index] as usize;
            let child = &snapshot.entries[id];
            let path = snapshot.path(id).display().to_string();
            let folder = child.kind == EntryKind::Directory;
            nav::Row {
                id,
                name: snapshot.name(id).into_owned(),
                size: self.size_text(snapshot, id),
                folder,
                dot: colors.get(&(id as u32)).copied().unwrap_or(th.faint),
                mark: data
                    .candidate_of
                    .get(&id)
                    .map(|&c| style::safety_color(data.reclaim.candidates[c].safety)),
                partial: !child.fully_listed,
                tip: if folder {
                    format!("{path}\nClick to open · right-click for more")
                } else {
                    format!("{path}\nRight-click for more")
                },
            }
        };
        let mut menus = Vec::new();
        let result = nav::panel(ui, body, spec, row, |response, row| {
            response.context_menu(|ui| entry_menu(ui, snapshot, row.id, &mut menus));
        });
        actions.append(&mut menus);
        self.list_hover = result.hovered;
        if let Some(id) = result.go {
            actions.push(Action::Zoom(id));
        }
        match result.clicked {
            Some((id, true)) => actions.push(Action::Zoom(id)),
            Some((id, false)) => actions.push(Action::Select(id)),
            None => {}
        }
        if result.notice_clicked {
            actions.push(Action::Tab(Tab::Issues));
        }
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
            .path(self.selected.unwrap_or(self.zoom))
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
        let shown = nav::fit_path(&row, &path, style::mono(11.5), footer.width() - 40.0);
        p.text(
            pos2(footer.left(), footer.center().y),
            Align2::LEFT_CENTER,
            shown,
            style::mono(11.5),
            th.faint,
        );
        actions
    }

    // ------------------------------------------------------------- clean up

    fn cleanup(&mut self, ui: &mut Ui, data: &Arc<Data>) -> Vec<Action> {
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
        crate::cleanup_ui::toolbar(self, ui, data);
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
    fn plan_bar(&mut self, ui: &mut Ui, data: &Arc<Data>, area: Rect) -> Vec<Action> {
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
        let width = area.width().min(760.0);
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
            format!(
                "{} · {} selected",
                style::plural(chosen.len() as u64, "item"),
                style::bytes(chosen.iter().map(|c| size_metric(c, self.allocated)).sum())
            ),
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
        crate::cleanup_ui::bar_buttons(self, &mut bar, data);
        if w::button(&mut bar, Kind::Soft, Some(Icon::Copy), "Copy plan")
            .on_hover_text("A commented list of the suggested command for each folder. Review it, then run what you agree with.")
            .clicked()
        {
            let mut plan = format!(
                "# RillDisk cleanup plan: {} folders, frees at least {}\n# Review every line before running it.\n",
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
                        "—".into()
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
        let scanning = self.running.is_some();
        self.elevate.issues(ui, &data.snapshot.summary, scanning);
    }

    fn search_tab(&mut self, ui: &mut Ui, data: &Data) -> Vec<Action> {
        let th = t();
        let Some((query, hits)) = self.search_results.clone() else {
            ui.add(egui::Spinner::new().color(th.accent));
            return Vec::new();
        };
        ui.label(
            RichText::new(format!(
                "{} for “{}”{}",
                match hits.len() {
                    1 => "1 match".to_string(),
                    n => format!("{} matches", style::count(n as u64)),
                },
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
            let content = 470.0
                + self.volumes.len() as f32 * 88.0
                + crate::assistants::extra_height(&self.assistants);
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
                            if w::icon_button(ui, Icon::Gear, "Settings").clicked() {
                                actions.push(Action::Settings(true));
                            }
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
                    crate::assistants::panel(ui, &mut self.assistants, width);
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(concat!(
                                "Metadata only · nothing leaves this PC   ·   v",
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

    /// The picture of the running scan, for scripted tours.
    pub(crate) fn live_view(&self) -> Option<Arc<LiveView>> {
        self.running.as_ref()?.live()
    }

    /// Show the live dashboard of `root` with scripted views: real scans of a
    /// fixture finish before a recording can browse them. The sender keeps the
    /// pretend scan running; `stop_scan_preview` ends it.
    pub(crate) fn preview_scan(
        &mut self,
        root: &str,
        views: HashMap<usize, Arc<LiveView>>,
    ) -> mpsc::Sender<Result<Data, String>> {
        let (tx, rx) = mpsc::channel();
        self.live_anim.reset();
        self.running = Some(Running {
            control: Arc::new(ScanControl::default()),
            root: root.to_string(),
            started: Instant::now(),
            rx,
            preview: Some(views),
        });
        tx
    }

    pub(crate) fn scan_control(&self) -> Option<&ScanControl> {
        self.running.as_ref().map(|running| &*running.control)
    }

    pub(crate) fn stop_scan_preview(&mut self) {
        self.running = None;
    }

    pub(crate) fn preview_update(
        &mut self,
        preview: update::Preview,
    ) -> Option<mpsc::Sender<Result<(), String>>> {
        self.available = Some(update::preview_release());
        self.update_error = None;
        self.installing = None;
        let (phase, done, total) = match preview {
            update::Preview::Available => return None,
            update::Preview::Downloading => (update::Phase::Downloading, 524288, 1048576),
            update::Preview::UnknownLength => (update::Phase::Downloading, 524288, 0),
            update::Preview::Verifying => (update::Phase::Verifying, 1048576, 1048576),
            update::Preview::Permission => (update::Phase::WaitingPermission, 1048576, 1048576),
            update::Preview::Declined => {
                self.update_error = Some(
                    "Administrator consent was declined (preview). Nothing was installed.".into(),
                );
                return None;
            }
            update::Preview::BadChecksum => {
                self.update_error = Some(
                    "Download checksum did not match (preview). Nothing was installed.".into(),
                );
                return None;
            }
        };
        let (install, sender) = update::preview_install(phase, done, total);
        self.installing = Some(install);
        Some(sender)
    }

    pub(crate) fn updating(&self) -> bool {
        self.installing.is_some()
    }

    /// Persistent update access, including the welcome screen.
    fn update_bar(&self, ui: &mut Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let th = t();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(concat!("RillDisk ", env!("CARGO_PKG_VERSION"))).small().color(th.dim));
            ui.separator();
            if let Some(install) = &self.installing {
                ui.label(RichText::new(install.phase().label()).color(th.accent));
                let (received, total) = install.bytes();
                let (bar, _) = ui.allocate_exact_size(vec2(220.0, 6.0), Sense::hover());
                if let Some(progress) = install.progress() {
                    w::progress(ui.painter(), bar, progress, th.accent);
                    ui.label(format!("{:.0}% | {} / {}", progress.clamp(0.0, 1.0) * 100.0, style::bytes(received), style::bytes(total)));
                } else {
                    live::sweep(ui.painter(), bar, ui.input(|i| i.time), th.accent);
                    if received > 0 { ui.label(format!("{} received", style::bytes(received))); }
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            if let Some(error) = &self.update_error {
                ui.label(RichText::new("Update needs attention").color(th.danger)).on_hover_text(error);
                ui.menu_button("Details", |ui| {
                    ui.set_max_width(500.0);
                    ui.label(error);
                    if ui.button("Copy details").clicked() { ui.ctx().copy_text(error.clone()); }
                });
                if w::button(ui, Kind::Soft, None, "Retry").on_hover_text(error).clicked() {
                    actions.push(match &self.available {
                        Some(release) if release.blocked.is_none() && (release.update_command.is_none() || release.msi) => Action::Install(Box::new(release.clone())),
                        _ => Action::CheckUpdates,
                    });
                }
            } else if let Some(release) = &self.available {
                ui.label(RichText::new(format!("Update {} available", release.version)).color(th.accent));
                if release.blocked.is_none() && (release.update_command.is_none() || release.msi) {
                    let allowed = self.running.is_none() && !self.cleanup.modal_open();
                    let button = ui.add_enabled_ui(allowed, |ui| w::button(ui, Kind::Primary, None, "Update and restart")).inner;
                    if self.tour.is_some() { ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("update-install-button"), button.rect)); }
                    if button.on_disabled_hover_text("Finish the scan or cleanup before restarting")
                        .on_hover_text(if release.msi { "Downloads and verifies the MSI, then asks Windows to install it. RillDisk restarts afterward." } else { "Downloads, verifies and replaces both programs, then restarts RillDisk." }).clicked() {
                        actions.push(Action::Install(Box::new(release.clone())));
                    }
                } else {
                    if let Some(url) = &release.download_url
                        && w::button(ui, Kind::Soft, None, "Download package").clicked() { ui.ctx().open_url(egui::OpenUrl::new_tab(url)); }
                    if let Some(reason) = release.blocked.as_ref().or(release.update_command.as_ref()) { ui.label(reason); }
                }
                if ui.link("What's new").clicked() { ui.ctx().open_url(egui::OpenUrl::new_tab(&release.notes_url)); }
            } else {
                if let Some(notice) = &self.update_notice { ui.label(RichText::new(notice).color(th.good)); }
                let checking = self.update_check.is_some();
                if checking { ui.spinner(); ui.label("Checking for updates..."); }
                else if w::button(ui, Kind::Ghost, Some(Icon::Refresh), "Check for updates").clicked() { actions.push(Action::CheckUpdates); }
            }
        });
        actions
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
        self.elevate.poll(&ctx);
        let title = match (&self.running, &self.data) {
            (Some(running), _) => format!("Scanning {} · RillDisk", running.root),
            (None, Some(data)) => {
                let shown = if self.tab == Tab::Explore {
                    self.zoom
                } else {
                    0
                };
                format!("{} · RillDisk", data.snapshot.path(shown).display())
            }
            (None, None) => "RillDisk".to_string(),
        };
        if self.title != title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        let mut actions = self.shortcuts(&ctx);
        let th = t();
        // Theme "System" follows the OS live.
        if self.prefs.theme.is_none()
            && let Some(system) = ctx.system_theme()
        {
            self.dark = system == egui::Theme::Dark;
        }
        let update_bar = egui::Panel::bottom("update-status")
            .resizable(false)
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(th.sidebar)
                    .inner_margin(Margin::symmetric(18, 10)),
            )
            .show(ui, |ui| self.update_bar(ui));
        actions.extend(update_bar.inner);
        if self.data.is_none() && self.running.is_none() {
            let central = egui::CentralPanel::default()
                .frame(Frame::new().fill(th.bg).inner_margin(Margin::same(24)))
                .show(ui, |ui| {
                    if self.installing.is_some() {
                        ui.disable();
                    }
                    if self.settings_open {
                        self.settings_page(ui)
                    } else {
                        self.home(ui)
                    }
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
                .show(ui, |ui| {
                    if self.installing.is_some() {
                        ui.disable();
                    }
                    if self.settings_open {
                        self.settings_page(ui)
                    } else {
                        self.main(ui)
                    }
                });
            actions.extend(central.inner);
        }
        self.apply(&ctx, actions);
        crate::cleanup_ui::show(self, &ctx);
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

/// Case rules of the platform's file names (Linux is case-sensitive).
fn same_name(a: &str, b: &str) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

fn same_path(a: &str, b: &str) -> bool {
    same_name(
        a.trim().trim_end_matches(['\\', '/']),
        b.trim().trim_end_matches(['\\', '/']),
    )
}

/// `path` relative to `root`, if it is `root` or inside it.
fn relative<'a>(root: &str, path: &'a str) -> Option<&'a str> {
    let root = root.trim().trim_end_matches(['\\', '/']);
    let path = path.trim().trim_matches('"');
    let trimmed = path.trim_end_matches(['\\', '/']);
    if same_name(trimmed, root) {
        return Some("");
    }
    let head = path.get(..root.len())?;
    let rest = path.get(root.len()..)?;
    (same_name(head, root) && rest.starts_with(['\\', '/'])).then(|| rest.trim_matches(['\\', '/']))
}

/// Whether a scan of `root` will include `path`.
fn covers(root: &str, path: &str) -> bool {
    relative(root, path).is_some()
}

/// The entry for `path` in `snapshot`, if the scan covered it and listed it.
fn find_path(snapshot: &Snapshot, path: &str) -> Option<usize> {
    let rel = relative(&snapshot.summary.root.display().to_string(), path)?;
    let mut id = 0;
    for part in rel.split(['\\', '/']).filter(|p| !p.is_empty()) {
        id = snapshot
            .children(id)
            .iter()
            .map(|&c| c as usize)
            .find(|&c| {
                snapshot.entries[c].kind == EntryKind::Directory
                    && same_name(&snapshot.name(c), part)
            })?;
    }
    Some(id)
}

/// Space removing a candidate frees at least: hard-linked files are excluded.
pub(crate) fn frees(candidate: &Candidate, allocated: bool) -> u64 {
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

/// Why a folder's chart is blank.
fn nothing_to_draw(snapshot: &Snapshot, id: usize) -> &'static str {
    if snapshot.entries[id].fully_listed {
        "Nothing with a size here"
    } else {
        "This folder could not be fully read"
    }
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

#[cfg(test)]
mod tests {
    use super::{Data, RillApp, Tab, covers, nav, relative, same_path};
    use eframe::{App, egui};
    use rilldisk_core::{ScanControl, ScanOptions, Snapshot, scan};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// The whole app, driven by the input events a window would deliver.
    struct Driver {
        ctx: egui::Context,
        app: RillApp,
        frame: eframe::Frame,
        pointer: egui::Pos2,
        root: PathBuf,
        /// Widget names reported to assistive technology.
        announced: Vec<String>,
    }

    /// A unique, empty folder holding the fixture and the unused capture directory.
    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rilldisk-nav-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Projects/atlas-web/src/app.ts, a zero-byte file and an empty folder.
    fn fixture(root: &Path) {
        let web = root.join("Projects").join("atlas-web");
        std::fs::create_dir_all(web.join("src")).unwrap();
        std::fs::create_dir_all(web.join("empty-folder")).unwrap();
        std::fs::write(web.join("src").join("app.ts"), vec![1; 4096]).unwrap();
        std::fs::write(web.join("zero-bytes.txt"), b"").unwrap();
        std::fs::write(root.join("notes.txt"), vec![1; 512]).unwrap();
    }

    impl Drop for Driver {
        fn drop(&mut self) {
            if let Some(workspace) = self.root.parent() {
                let _ = std::fs::remove_dir_all(workspace);
            }
        }
    }

    fn snapshot(root: &Path) -> Snapshot {
        scan(
            root,
            ScanOptions::default(),
            Arc::new(ScanControl::default()),
        )
        .unwrap()
    }

    impl Driver {
        fn new(name: &str) -> Self {
            let dir = workspace(name);
            let root = dir.join("home");
            fixture(&root);
            let mut driver = Self::empty(&dir, root.clone());
            driver.app.path = root.display().to_string();
            driver.app.install(Data::analyze(snapshot(&root)));
            driver.app.tab = Tab::Explore;
            driver.run(Vec::new());
            driver
        }

        fn empty(dir: &Path, root: PathBuf) -> Self {
            let ctx = egui::Context::default();
            let cc = eframe::CreationContext::_new_kittest(ctx.clone());
            // A tour keeps the app away from the network.
            let tour = crate::capture::Tour::idle(dir.join("capture"));
            let mut app = RillApp::new(&cc, None, Some(false), None, Some(tour));
            app.allocated = false;
            Self {
                ctx,
                app,
                frame: eframe::Frame::_new_kittest(),
                pointer: egui::pos2(5.0, 5.0),
                root,
                announced: Vec::new(),
            }
        }

        fn run(&mut self, events: Vec<egui::Event>) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1480.0, 920.0),
                )),
                events,
                ..Default::default()
            };
            let (app, frame) = (&mut self.app, &mut self.frame);
            let output = self.ctx.run_ui(input, |ui| app.ui(ui, frame));
            for event in &output.platform_output.events {
                if let egui::output::OutputEvent::Clicked(info) = event {
                    self.announced.extend(info.label.clone());
                }
            }
            output.drop_without_applying_deltas();
        }

        fn settle(&mut self) {
            for _ in 0..3 {
                self.run(Vec::new());
            }
        }

        fn id(&self, path: &str) -> usize {
            let data = self.app.data.as_ref().unwrap();
            let target = self.root.join(path);
            (0..data.snapshot.entries.len())
                .find(|&id| data.snapshot.path(id) == target)
                .unwrap_or_else(|| panic!("no entry for {path}"))
        }

        fn hits(&self, key: &str) -> Vec<(usize, egui::Rect)> {
            self.ctx
                .data(|d| d.get_temp(egui::Id::unique(key)))
                .unwrap_or_default()
        }

        fn click_at(&mut self, pos: egui::Pos2) {
            self.pointer = pos;
            self.run(vec![egui::Event::PointerMoved(pos)]);
            self.run(
                [true, false]
                    .map(|pressed| egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    })
                    .to_vec(),
            );
            self.settle();
        }

        fn click_hit(&mut self, key: &str, id: usize) {
            let rect = self
                .hits(key)
                .into_iter()
                .find(|hit| hit.0 == id)
                .unwrap_or_else(|| panic!("{key} does not show entry {id}"))
                .1;
            self.click_at(rect.center());
        }

        fn click_row(&mut self, path: &str) {
            self.click_hit(nav::PANEL_HITS, self.id(path));
        }

        fn click_parent(&mut self) {
            let rect: egui::Rect = self
                .ctx
                .data(|d| d.get_temp(egui::Id::unique(nav::PARENT_BUTTON)))
                .unwrap();
            self.click_at(rect.center());
        }

        fn press(&mut self, key: egui::Key) {
            let event = |pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Default::default(),
            };
            self.run(vec![event(true), event(false)]);
            self.settle();
        }
    }

    #[test]
    fn parent_row_breadcrumb_and_keys_walk_the_tree_and_mark_the_way_back() {
        let mut d = Driver::new("walk");
        let (projects, web) = (d.id("Projects"), d.id("Projects/atlas-web"));
        d.click_row("Projects");
        assert_eq!(d.app.zoom, projects);
        d.click_row("Projects/atlas-web");
        assert_eq!(d.app.zoom, web);
        d.click_row("Projects/atlas-web/src");
        assert_eq!(d.app.zoom, d.id("Projects/atlas-web/src"));

        // Parent goes up exactly one level and marks the folder just left.
        d.click_parent();
        assert_eq!(d.app.zoom, web);
        assert_eq!(d.app.selected, Some(d.id("Projects/atlas-web/src")));

        // The breadcrumb jumps several levels; the branch taken stays marked.
        d.click_hit(nav::CRUMB_HITS, 0);
        assert_eq!(d.app.zoom, 0);
        assert_eq!(d.app.selected, Some(projects));

        // Enter opens the marked folder; Left and Backspace come back.
        d.press(egui::Key::Enter);
        assert_eq!(d.app.zoom, projects);
        d.press(egui::Key::ArrowLeft);
        assert_eq!(d.app.zoom, 0);
        d.click_row("Projects");
        d.press(egui::Key::Backspace);
        assert_eq!(d.app.zoom, 0);

        // The top of the scan is a floor, not an error.
        d.press(egui::Key::Backspace);
        d.click_parent();
        assert_eq!(d.app.zoom, 0);
    }

    #[test]
    fn rows_trail_and_breadcrumbs_are_named_for_assistive_technology() {
        let mut d = Driver::new("names");
        d.click_row("Projects");
        d.click_row("Projects/atlas-web");
        d.click_row("Projects/atlas-web/src");
        let heard = |d: &Driver, text: &str| d.announced.iter().any(|l| l.starts_with(text));
        assert!(
            heard(&d, "atlas-web, "),
            "rows state name and size: {:?}",
            d.announced
        );
        d.click_row("Projects");
        assert!(
            d.announced.last().is_some_and(|l| l == "Projects"),
            "trail row"
        );
        d.click_hit(nav::CRUMB_HITS, 0);
        assert!(
            d.announced.last().is_some_and(|l| l == "home"),
            "breadcrumb"
        );
    }

    #[test]
    fn on_disk_order_is_largest_first_with_unknown_sizes_last() {
        let dir = workspace("order");
        let root = dir.join("home");
        fixture(&root);
        let mut d = Driver::empty(&dir, root.clone());
        let mut scanned = snapshot(&root);
        let path_of = |s: &Snapshot, name: &str| {
            (0..s.entries.len())
                .find(|&id| s.path(id).ends_with(name))
                .unwrap()
        };
        let notes = path_of(&scanned, "notes.txt");
        let projects = path_of(&scanned, "Projects");
        scanned.entries[notes].allocated_bytes = Some(10);
        scanned.entries[projects].allocated_bytes = None;
        let hidden = scanned.children(0).len();
        d.app.install(Data::analyze(scanned));
        d.app.allocated = true;
        let data = d.app.data.clone().unwrap();
        let rows = d.app.children_of_zoom(&data.snapshot);
        assert_eq!(rows.len(), hidden, "unknown sizes are listed, not dropped");
        assert_eq!(rows.first().copied(), Some(notes as u32));
        assert_eq!(rows.last().copied(), Some(projects as u32));
    }

    #[test]
    fn the_trail_returns_to_any_ancestor() {
        let mut d = Driver::new("trail");
        d.click_row("Projects");
        d.click_row("Projects/atlas-web");
        d.click_row("Projects/atlas-web/src");
        // Root and parent are trail rows of the panel; the current folder is not.
        let trail = d.hits(nav::PANEL_HITS);
        assert!(trail.iter().any(|hit| hit.0 == 0));
        assert!(!trail.iter().any(|hit| hit.0 == d.app.zoom));
        d.click_row("Projects");
        assert_eq!(d.app.zoom, d.id("Projects"));
    }

    #[test]
    fn hidden_levels_stay_reachable_from_the_trail_and_the_breadcrumb() {
        let dir = workspace("levels");
        let root = dir.join("home");
        let chain = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
        let deepest = chain
            .iter()
            .fold(root.clone(), |path, name| path.join(name));
        std::fs::create_dir_all(&deepest).unwrap();
        std::fs::write(deepest.join("x.bin"), vec![1; 64]).unwrap();
        let mut d = Driver::empty(&dir, root.clone());
        d.app.install(Data::analyze(snapshot(&root)));
        d.app.tab = Tab::Explore;
        d.settle();
        let target = d.id("a/b/c/d/e");
        d.app.zoom = d.id("a/b/c/d/e/f/g/h/i/j/k/l");
        d.settle();

        d.click_hit(nav::PANEL_HITS, nav::MORE_LEVELS);
        d.click_hit(nav::PANEL_HITS, target);
        assert_eq!(d.app.zoom, target, "the trail menu opens a hidden level");

        d.app.zoom = d.id("a/b/c/d/e/f/g/h/i/j/k/l");
        d.settle();
        let crumbs = d.hits(nav::CRUMB_HITS);
        if crumbs.iter().any(|hit| hit.0 == nav::MORE_LEVELS) {
            d.click_hit(nav::CRUMB_HITS, nav::MORE_LEVELS);
        }
        d.click_hit(nav::CRUMB_HITS, target);
        assert_eq!(d.app.zoom, target, "the breadcrumb reaches a hidden level");
    }

    #[test]
    fn the_panel_keeps_the_shown_folder_while_the_pointer_crosses_the_chart() {
        let mut d = Driver::new("hover");
        d.click_row("Projects");
        d.click_row("Projects/atlas-web");
        let listed = |d: &Driver| {
            let mut ids: Vec<usize> = d.hits(nav::PANEL_HITS).iter().map(|h| h.0).collect();
            ids.sort_unstable();
            ids
        };
        let before = listed(&d);
        let src = d.id("Projects/atlas-web/src");
        let tile = d
            .app
            .map
            .tile_of(src)
            .expect("src has a tile")
            .rect
            .center();
        d.pointer = tile;
        d.run(vec![egui::Event::PointerMoved(tile)]);
        d.settle();
        assert!(d.app.hovered.is_some(), "the pointer is on the chart");
        assert_eq!(listed(&d), before);
    }

    #[test]
    fn every_child_is_listed_and_reachable_including_zero_sized_ones() {
        let mut d = Driver::new("zero");
        d.click_row("Projects");
        d.click_row("Projects/atlas-web");
        let (empty, zero) = (
            d.id("Projects/atlas-web/empty-folder"),
            d.id("Projects/atlas-web/zero-bytes.txt"),
        );
        let hits = d.hits(nav::PANEL_HITS);
        assert!(hits.iter().any(|hit| hit.0 == empty));
        assert!(hits.iter().any(|hit| hit.0 == zero));
        // Arrow keys walk the same list the panel shows.
        for _ in 0..3 {
            d.press(egui::Key::ArrowDown);
        }
        assert!(d.app.selected == Some(empty) || d.app.selected == Some(zero));
        d.click_hit(nav::PANEL_HITS, zero);
        assert_eq!(d.app.selected, Some(zero));
        assert_eq!(
            d.app.zoom,
            d.id("Projects/atlas-web"),
            "a file is not entered"
        );
        d.click_hit(nav::PANEL_HITS, empty);
        assert_eq!(d.app.zoom, empty);
        assert!(d.hits(nav::PANEL_HITS).iter().all(|hit| hit.0 != empty));
    }

    #[test]
    fn going_up_is_only_possible_where_a_folder_is_shown() {
        let mut d = Driver::new("tabs");
        d.click_row("Projects");
        let projects = d.app.zoom;
        d.app.tab = Tab::Overview;
        d.settle();
        d.press(egui::Key::Backspace);
        assert_eq!(
            d.app.zoom, projects,
            "Overview must not move the hidden folder"
        );
        d.app.tab = Tab::Explore;
        d.app.settings_open = true;
        d.settle();
        d.press(egui::Key::Backspace);
        assert_eq!(
            d.app.zoom, projects,
            "Settings must not move the hidden folder"
        );
    }

    #[test]
    fn an_unreadable_folder_says_so_and_links_to_issues() {
        let dir = workspace("unreadable");
        let root = dir.join("home");
        fixture(&root);
        let mut d = Driver::empty(&dir, root.clone());
        let mut scanned = snapshot(&root);
        let denied = (0..scanned.entries.len())
            .find(|&id| scanned.path(id) == root.join("Projects"))
            .unwrap();
        scanned.entries[denied].fully_listed = false;
        scanned.summary.issue_count = 1;
        // A partly read folder keeps what was found; a complete empty one is 0 B.
        let unread = (0..scanned.entries.len())
            .find(|&id| scanned.path(id) == root.join("Projects").join("atlas-web").join("src"))
            .unwrap();
        scanned.entries[unread].fully_listed = false;
        d.app.install(Data::analyze(scanned));
        let data = d.app.data.clone().unwrap();
        assert_ne!(d.app.size_text(&data.snapshot, unread), "unknown");
        assert_ne!(d.app.size_text(&data.snapshot, denied), "unknown");
        let empty = (0..data.snapshot.entries.len())
            .find(|&id| {
                data.snapshot.path(id)
                    == root.join("Projects").join("atlas-web").join("empty-folder")
            })
            .unwrap();
        assert_ne!(d.app.size_text(&data.snapshot, empty), "unknown");
        d.app.tab = Tab::Explore;
        d.settle();
        d.click_row("Projects");
        // Below the current-folder row of a root-level folder, under the trail.
        let panel = d.hits(nav::PANEL_HITS);
        let top = panel
            .iter()
            .map(|hit| hit.1)
            .find(|r| r.height() < 32.0)
            .unwrap();
        d.click_at(egui::pos2(top.center().x, top.bottom() + 48.0 + 12.0));
        assert_eq!(d.app.tab, Tab::Issues);
    }

    #[test]
    fn a_folder_that_listed_nothing_has_no_size_to_state() {
        let dir = workspace("unknown");
        let root = dir.join("home");
        fixture(&root);
        let mut d = Driver::empty(&dir, root.clone());
        let mut scanned = snapshot(&root);
        let empty = (0..scanned.entries.len())
            .find(|&id| scanned.path(id).ends_with("empty-folder"))
            .unwrap();
        scanned.entries[empty].fully_listed = false;
        scanned.entries[0].allocated_bytes = None;
        d.app.install(Data::analyze(scanned));
        let data = d.app.data.clone().unwrap();
        assert_eq!(d.app.size_text(&data.snapshot, empty), "unknown");
        assert_ne!(d.app.size_text(&data.snapshot, 0), "unknown");
        d.app.allocated = true;
        assert_eq!(d.app.size_text(&data.snapshot, 0), "unknown");
    }

    #[test]
    fn a_running_scan_is_browsed_with_the_same_controls() {
        let mut d = Driver::new("live");
        let _scan = d.app.preview_scan(r"D:\", crate::capture::preview_views());
        d.settle();
        let shown = |d: &Driver| {
            d.app
                .live_view()
                .and_then(|view| view.ancestors.last().map(|a| a.1.clone()))
                .unwrap_or_default()
        };
        d.click_hit(nav::PANEL_HITS, 1);
        assert_eq!(shown(&d), "Archive");
        let row = d.hits(nav::PANEL_HITS).last().copied().unwrap();
        d.click_at(row.1.center());
        let deep = shown(&d);
        assert_ne!(deep, "Archive");

        // Parent goes up one level and marks the folder left, then keys do too.
        d.click_parent();
        assert_eq!(shown(&d), "Archive");
        assert!(
            d.hits(nav::PANEL_HITS).iter().any(|hit| hit.0 == row.0),
            "the folder left is listed again"
        );
        d.press(egui::Key::Backspace);
        assert_eq!(shown(&d), "");
        d.click_hit(nav::PANEL_HITS, 1);
        d.press(egui::Key::ArrowLeft);
        assert_eq!(shown(&d), "");

        // Breadcrumbs jump straight back to the scan root.
        d.click_hit(nav::PANEL_HITS, 1);
        d.click_hit(nav::CRUMB_HITS, 0);
        assert_eq!(shown(&d), "");
    }

    #[test]
    fn a_rescan_keeps_the_location_and_says_when_it_is_gone() {
        let mut d = Driver::new("rescan");
        d.click_row("Projects");
        d.click_row("Projects/atlas-web");
        let finish = |d: &mut Driver| {
            for _ in 0..4000 {
                d.run(Vec::new());
                if d.app.running.is_none() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("the rescan did not finish");
        };
        // The results stay usable while the same folder is scanned again.
        d.press(egui::Key::F5);
        finish(&mut d);
        assert_eq!(d.app.tab, Tab::Explore);
        assert_eq!(d.app.zoom, d.id("Projects/atlas-web"));

        std::fs::remove_dir_all(d.root.join("Projects")).unwrap();
        d.press(egui::Key::F5);
        finish(&mut d);
        assert_eq!(d.app.zoom, 0);
        assert!(d.app.toast.is_some(), "the move to the root is explained");
    }

    #[test]
    fn places_resolve_inside_a_scan_root() {
        let sep = std::path::MAIN_SEPARATOR;
        let root = format!("{sep}data{sep}home");
        let inside = format!("{root}{sep}Downloads{sep}iso");
        assert_eq!(relative(&root, &root), Some(""));
        assert_eq!(relative(&format!("{root}{sep}"), &root), Some(""));
        assert_eq!(
            relative(&root, &inside),
            Some(format!("Downloads{sep}iso").as_str())
        );
        // A sibling that merely shares a prefix is not inside.
        assert!(!covers(&root, &format!("{root}-old{sep}x")));
        assert!(covers(&root, &inside));
        assert!(same_path(&format!("{root}{sep}"), &root));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_ignore_case_and_drive_roots_cover_everything() {
        assert!(covers(r"C:\", r"c:\Users\Me\Downloads"));
        assert_eq!(
            relative(r"C:\Users\Me", r"c:\users\me\Desktop"),
            Some("Desktop")
        );
        assert!(same_path(r"C:\", "c:"));
    }
}
