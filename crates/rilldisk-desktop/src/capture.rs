// SPDX-License-Identifier: AGPL-3.0-only
//! Scripted tours for reproducible screenshots and recordings (`--capture DIR`).
//!
//! Pointer movement and clicks are injected as ordinary egui input events, so the
//! recording exercises the same code paths as a person using the app.
use crate::app::{Chart, RillApp, Tab as View, nav, pointer_for};
use eframe::egui::{self, Pos2, pos2};
use rilldisk_core::{Category, CleanupMode, LiveFolder, LiveView};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Key of a Clean up card's screen rect (with its entry id), for scripted tours.
pub const CARD_HITS: &str = "cleanup-card";

#[derive(Clone, Copy)]
pub enum Target {
    /// Entry whose path ends with these components.
    Entry(&'static str),
    /// Largest reclaim candidate of a category.
    Candidate(Category),
    /// Row of the folder panel, by path suffix.
    Row(&'static str),
    /// A card in Clean up, by path suffix.
    Card(&'static str),
    /// Breadcrumb, by path suffix.
    Crumb(&'static str),
    /// Row of the folder panel during a scan, by name.
    LiveRow(&'static str),
    /// The scan root's breadcrumb during a scan.
    LiveRoot,
    /// The folder panel's control for hidden levels.
    MoreLevels,
    Parent,
}

#[derive(Clone)]
pub enum Step {
    WaitData,
    /// Stop the tour, saving nothing more, unless the scan reported access-denied
    /// folders (the frames that follow show the guidance for them).
    ExpectDenied,
    Wait(u32),
    Shot(&'static str),
    Record(bool),
    Move(Target, u32),
    Click(Target),
    /// Right-click a target to open its context menu.
    Menu(Target),
    /// Close an open menu (Escape).
    CloseMenu,
    /// Clean up: show one category (or everything).
    Filter(Option<Category>),
    /// Clean up: show the safe half or the Review-first half.
    Half(crate::app::CleanupView),
    /// Clean up: untick everything.
    ClearPicks,
    /// Until Git has answered for every worktree (the fixture's real repositories).
    WaitGit,
    /// Start a caption at the next recorded frame (written to captions.json).
    Caption(&'static str),
    /// Clean up: "Select clean idle worktrees".
    SelectCleanIdle,
    /// The pointer leaves the window (hides hover tooltips).
    PointerAway,
    Tab(View),
    Highlight(bool),
    Search(&'static str),
    ChartKind(Chart),
    /// Back to the scanned root.
    Root,
    Back,
    ExpectFolder(&'static str),
    /// The folder panel marks this entry as the branch the user came from.
    ExpectSelected(&'static str),
    /// Scroll the panel under the pointer.
    Wheel(f32),
    /// Show the live dashboard with scripted views of a fictional drive.
    LivePreview(bool),
    /// The live dashboard shows this folder; empty for the scan root.
    ExpectLive(&'static str),
    Theme(bool),
    /// Tick the largest candidates of a category in Clean up.
    Pick(Category, usize),
    Settings(bool),
    /// Clean up: "Select all safe".
    SelectSafe,
    /// Clean up: open the confirmation dialog for the ticked folders.
    Confirm(CleanupMode),
    /// Clean up: confirm and wait for the result (temp-folder scans only).
    RunCleanup,
    /// Clean up: close any dialog.
    Dismiss,
    /// Show the elevated look (badge) without elevating.
    Elevated(bool),
    UpdatePreview(crate::update::Preview),
    ClickUpdate,
    ExpectUpdating,
    Quit,
}

/// Which scripted tour to run.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TourKind {
    /// No scan: the welcome page and Settings.
    Welcome,
    /// The full product tour of a scanned folder (made for the demo fixture).
    Full,
    /// Frames of the live dashboard while a (large) scan runs, then Settings.
    Live,
    /// The Issues view of a fixture with unreadable folders (`--tour issues`).
    Issues,
}

pub struct Tour {
    steps: VecDeque<Step>,
    dir: PathBuf,
    wait: u32,
    tick: u64,
    frame: u32,
    recording: bool,
    pointer: Pos2,
    motion: Option<(Pos2, Pos2, u32, u32)>,
    pending: Arc<AtomicUsize>,
    /// First recorded frame of each caption, for the video encoder.
    captions: Vec<(u32, &'static str)>,
    /// Keeps a scripted scan running.
    preview_scan: Option<std::sync::mpsc::Sender<Result<crate::app::Data, String>>>,
    pub(crate) update_sender: Option<std::sync::mpsc::Sender<Result<(), String>>>,
}

impl Tour {
    /// A captioned walk through worktree analysis on the synthetic demo disk
    /// (`scripts/make_demo_fixture.py` with Git), for `scripts/make_demo_video.py`.
    /// Recorded at one frame per three ticks; the encoder plays 20 frames a second.
    /// Nothing is removed: the removal dialog is opened and closed.
    pub fn video(dir: PathBuf) -> Self {
        use Step::*;
        use Target::*;
        let agent = "atlas-web/.claude/worktrees/agent-3f9c2a";
        let perf = "Projects/ferrite-engine-perf";
        // Not `worktrees/…` alone: Git keeps an admin folder of the same name.
        let spike = ".t3/worktrees/atlas-web-spike";
        Self::with_steps(
            dir,
            vec![
                WaitData,
                WaitGit,
                Wait(20),
                Record(true),
                Caption("A synthetic demo disk with fictional paths, scanned by RillDisk 0.1.3"),
                Wait(240),
                Caption("Size counts every byte; Clean up counts what removal would actually free"),
                Tab(View::Cleanup),
                Wait(250),
                Caption("Git worktrees: four found, with their size and what removing them frees"),
                Filter(Some(Category::GitWorktree)),
                Wait(260),
                Caption("Each worktree is checked with your own Git, read-only"),
                Move(Card(agent), 30),
                Wait(240),
                Caption(
                    "118 days idle, yet Git reports an uncommitted edit: it stays Review first",
                ),
                Move(Card(perf), 30),
                Wait(240),
                Caption("A commit that exists only on a detached HEAD would be lost: kept"),
                Move(Card(spike), 30),
                Wait(220),
                PointerAway,
                Caption(
                    "Bulk select ticks only worktrees with no Git-reported changes and no recent work",
                ),
                SelectCleanIdle,
                Wait(200),
                Caption(
                    "Removal asks first, uses the Recycle Bin, and checks Git again right before each worktree",
                ),
                Confirm(CleanupMode::Trash),
                Wait(300),
                Dismiss,
                Caption(
                    "Right-click any folder: Show in Explorer, copy its path, or scan just that folder",
                ),
                Filter(None),
                Tab(View::Explore),
                Wait(30),
                Menu(Row("Projects")),
                Wait(200),
                CloseMenu,
                Wait(10),
                Record(false),
                Quit,
            ],
        )
    }

    pub fn updates(dir: PathBuf) -> Self {
        use crate::update::Preview;
        use Step::*;
        Self::with_steps(
            dir,
            vec![
                Theme(false),
                Wait(10),
                UpdatePreview(Preview::Available),
                Wait(8),
                Shot("update-available"),
                Record(true),
                ClickUpdate,
                Wait(8),
                ExpectUpdating,
                Shot("update-downloading"),
                UpdatePreview(Preview::UnknownLength),
                Wait(8),
                Shot("update-unknown-size"),
                UpdatePreview(Preview::Verifying),
                Wait(8),
                Shot("update-verifying"),
                UpdatePreview(Preview::Permission),
                Wait(8),
                Shot("update-permission"),
                UpdatePreview(Preview::Declined),
                Wait(8),
                Shot("update-declined"),
                UpdatePreview(Preview::BadChecksum),
                Wait(8),
                Shot("update-checksum-error"),
                Record(false),
                Quit,
            ],
        )
    }

    /// Focused navigation acceptance on the fictional navigation fixture
    /// (`scripts/make_navigation_fixture.py`). The fixture scans in under a second,
    /// so the live part browses scripted views through the real dashboard.
    pub fn navigation(dir: PathBuf) -> Self {
        use Step::*;
        use Target::*;
        Self::with_steps(
            dir,
            vec![
                Theme(false),
                WaitData,
                Wait(10),
                Shot("navigation-overview"),
                LivePreview(true),
                Wait(10),
                ExpectLive(""),
                Shot("navigation-live-root"),
                Record(true),
                Move(LiveRow("Archive"), 6),
                Click(LiveRow("Archive")),
                ExpectLive("Archive"),
                Wait(4),
                Shot("navigation-live-folder"),
                Move(LiveRow("client-deliverables"), 6),
                Click(LiveRow("client-deliverables")),
                ExpectLive("client-deliverables"),
                Wait(4),
                Shot("navigation-live-deep"),
                Click(Parent),
                ExpectLive("Archive"),
                Wait(4),
                Shot("navigation-live-parent"),
                Click(LiveRoot),
                ExpectLive(""),
                Wait(4),
                LivePreview(false),
                Wait(4),
                ChartKind(Chart::Rings),
                Tab(View::Explore),
                Wait(5),
                Move(Entry("Projects"), 4),
                Wait(4),
                Click(Entry("Projects")),
                Wait(8),
                ExpectFolder("Projects"),
                Shot("navigation-folder"),
                Move(Row("Projects/atlas-web"), 6),
                Click(Row("Projects/atlas-web")),
                Wait(8),
                ExpectFolder("Projects/atlas-web"),
                Shot("navigation-deep"),
                Move(Entry("Projects/atlas-web/node_modules"), 6),
                Wait(6),
                ExpectFolder("Projects/atlas-web"),
                Shot("navigation-chart-hover"),
                Click(Row("atlas-web/packages")),
                Wait(6),
                Click(Row("packages/design-system")),
                Wait(6),
                Click(Row("design-system/src")),
                Wait(6),
                Click(Row("src/components")),
                Wait(6),
                Click(Row("components/forms")),
                Wait(6),
                Click(Row("forms/inputs")),
                Wait(6),
                Click(Row("inputs/date-picker")),
                Wait(8),
                ExpectFolder("inputs/date-picker"),
                Shot("navigation-collapsed"),
                Move(MoreLevels, 6),
                Click(MoreLevels),
                Wait(8),
                Shot("navigation-more-levels"),
                Move(Row("Projects/atlas-web"), 6),
                Click(Row("Projects/atlas-web")),
                Wait(8),
                ExpectFolder("Projects/atlas-web"),
                ExpectSelected("atlas-web/packages"),
                Move(Crumb("Projects"), 6),
                Click(Crumb("Projects")),
                Wait(8),
                ExpectFolder("Projects"),
                ExpectSelected("Projects/atlas-web"),
                Shot("navigation-breadcrumb"),
                Click(Row("Projects/atlas-web")),
                Wait(8),
                Back,
                Wait(8),
                ExpectFolder("Projects"),
                ExpectSelected("Projects/atlas-web"),
                Shot("navigation-immediate-parent"),
                Move(Parent, 6),
                Click(Parent),
                Wait(8),
                ExpectFolder(""),
                ExpectSelected("Projects"),
                Shot("navigation-parent"),
                Move(Row("Projects"), 6),
                Wheel(-600.0),
                Wait(20),
                Click(Row("Archive")),
                Wait(8),
                ExpectFolder("Archive"),
                Shot("navigation-long-names"),
                Move(Row("Archive/client-deliverables"), 6),
                Click(Row("Archive/client-deliverables")),
                Wait(8),
                Move(Row("client-deliverables/project-0239"), 6),
                Wheel(-3700.0),
                Wait(20),
                Click(Row("client-deliverables/project-0130")),
                Wait(8),
                ExpectFolder("client-deliverables/project-0130"),
                Back,
                Wait(10),
                ExpectFolder("Archive/client-deliverables"),
                ExpectSelected("client-deliverables/project-0130"),
                Shot("navigation-many-siblings"),
                Back,
                Wait(8),
                Click(Row("Archive/empty-placeholder")),
                Wait(8),
                ExpectFolder("Archive/empty-placeholder"),
                Shot("navigation-empty"),
                Back,
                Wait(8),
                Click(Row("Archive/locked-vault")),
                Wait(8),
                ExpectFolder("Archive/locked-vault"),
                Shot("navigation-unreadable"),
                Back,
                Wait(8),
                Record(false),
                Theme(true),
                Wait(10),
                ChartKind(Chart::Map),
                Wait(10),
                Shot("navigation-dark"),
                Quit,
            ],
        )
    }

    /// The Clean up view's two halves, kind filter and bulk ticks, recorded on
    /// the synthetic demo disk (`--tour cleanup-view`). Nothing is removed.
    pub fn cleanup_view(dir: PathBuf) -> Self {
        use crate::app::CleanupView::{Review, Safe};
        use Step::*;
        Self::with_steps(
            dir,
            vec![
                Theme(true),
                WaitData,
                Tab(View::Cleanup),
                WaitGit,
                PointerAway,
                Wait(10),
                Shot("cleanup-view-safe"),
                Record(true),
                Caption("Safe to delete: folders that tools rebuild or re-download"),
                Wait(120),
                Caption("Select all safe ticks every one of them"),
                SelectSafe,
                Wait(120),
                Shot("cleanup-view-selected"),
                Caption("Narrow to one kind with the All kinds menu"),
                ClearPicks,
                Filter(Some(Category::BuildOutput)),
                Wait(120),
                Shot("cleanup-view-kind"),
                Caption("Review first: folders that may hold unique work"),
                Filter(None),
                Half(Review),
                Wait(120),
                Shot("cleanup-view-review"),
                Caption("Git says which worktrees are clean and idle"),
                SelectCleanIdle,
                Wait(120),
                Shot("cleanup-view-review-selected"),
                ClearPicks,
                Half(Safe),
                Wait(20),
                Record(false),
                Settings(true),
                Wait(10),
                Shot("cleanup-view-scan-limit"),
                Quit,
            ],
        )
    }

    /// No steps: an app driven by tests, which must not touch the network.
    #[cfg(test)]
    pub fn idle(dir: PathBuf) -> Self {
        Self::with_steps(dir, Vec::new())
    }

    /// Selection bar and dialogs of the Clean up view. Removes folders only when
    /// the scanned folder is inside the temp folder (a synthetic fixture).
    pub fn cleanup(dir: PathBuf) -> Self {
        use Step::*;
        let steps = vec![
            Theme(false),
            WaitData,
            Tab(View::Cleanup),
            Wait(20),
            SelectSafe,
            Wait(20),
            Shot("cleanup-selection"),
            Confirm(CleanupMode::Trash),
            Wait(40),
            Shot("cleanup-confirm"),
            Dismiss,
            Confirm(CleanupMode::Permanent),
            Wait(40),
            Shot("cleanup-confirm-permanent"),
            Dismiss,
            Theme(true),
            Wait(20),
            Shot("cleanup-selection-dark"),
            Confirm(CleanupMode::Trash),
            Wait(40),
            Shot("cleanup-confirm-dark"),
            Dismiss,
            Confirm(CleanupMode::Permanent),
            Wait(10),
            RunCleanup,
            Wait(30),
            Shot("cleanup-result-dark"),
            Theme(false),
            Wait(20),
            Shot("cleanup-result"),
            Dismiss,
            Wait(20),
            Shot("cleanup-after"),
            Quit,
        ];
        Self::with_steps(dir, steps)
    }

    pub fn new(dir: PathBuf, kind: TourKind) -> Self {
        use Step::*;
        use Target::*;
        let steps = if kind == TourKind::Issues {
            // Scan a fixture with unreadable folders: the restart offer in the
            // Issues tab, the header and Settings, then the elevated look
            // (simulated; nothing is elevated).
            vec![
                Theme(false),
                WaitData,
                ExpectDenied,
                Tab(View::Issues),
                Wait(24),
                Shot("issues"),
                Theme(true),
                Wait(20),
                Shot("issues-dark"),
                Theme(false),
                Tab(View::Overview),
                Wait(20),
                Shot("issues-header"),
                Settings(true),
                Wait(20),
                Shot("issues-settings"),
                Settings(false),
                Tab(View::Issues),
                Elevated(true),
                Wait(20),
                Shot("issues-elevated"),
                Wait(6),
                Quit,
            ]
        } else if kind == TourKind::Welcome {
            vec![
                Theme(false),
                Wait(30),
                Shot("home"),
                Theme(true),
                Wait(20),
                Shot("home-dark"),
                Settings(true),
                Wait(20),
                Shot("settings-dark"),
                Theme(false),
                Wait(20),
                Shot("settings"),
                Quit,
            ]
        } else if kind == TourKind::Live {
            vec![
                Wait(90),
                Shot("live-1"),
                Wait(240),
                Shot("live-2"),
                Theme(false),
                Wait(240),
                Shot("live-3"),
                Theme(true),
                Settings(true),
                Wait(20),
                Shot("live-settings"),
                Quit,
            ]
        } else {
            vec![
                Theme(false),
                WaitData,
                Wait(24),
                Shot("overview"),
                Record(true),
                Wait(8),
                Move(Entry("Projects"), 22),
                Wait(16),
                Tab(View::Explore),
                Wait(10),
                Move(Entry("Videos"), 22),
                Wait(12),
                Move(Entry("Projects/ferrite-engine/target"), 22),
                Wait(18),
                Shot("explore"),
                Highlight(true),
                Wait(14),
                Move(Candidate(Category::GitWorktree), 22),
                Wait(18),
                Shot("cleanable"),
                Highlight(false),
                ChartKind(Chart::Rings),
                Wait(12),
                Move(Entry("Games"), 22),
                Wait(10),
                Move(Entry("Projects"), 18),
                Wait(8),
                Click(Entry("Projects")),
                Wait(16),
                Move(Entry("Projects/atlas-web/node_modules"), 22),
                Wait(18),
                Shot("rings"),
                ExpectFolder("Projects"),
                Back,
                Wait(20),
                ExpectFolder(""),
                Shot("navigation-parent"),
                Menu(Row("Projects")),
                Wait(12),
                Shot("context-menu"),
                CloseMenu,
                Wait(8),
                Tab(View::Cleanup),
                Wait(14),
                WaitGit,
                SelectCleanIdle,
                Pick(Category::BuildOutput, 1),
                Wait(18),
                Shot("cleanup"),
                WaitGit,
                Filter(Some(Category::GitWorktree)),
                Wait(14),
                Shot("worktrees"),
                Filter(None),
                Tab(View::Largest),
                Wait(18),
                Record(false),
                Shot("largest"),
                Tab(View::Types),
                Wait(14),
                Shot("types"),
                Search("worktree"),
                Wait(30),
                Shot("search"),
                Theme(true),
                Root,
                Tab(View::Overview),
                Wait(24),
                Shot("overview-dark"),
                Tab(View::Explore),
                Wait(10),
                Move(Entry("Videos"), 10),
                Wait(14),
                Shot("rings-dark"),
                ChartKind(Chart::Map),
                Wait(14),
                Shot("explore-dark"),
                Menu(Entry("VMs/ubuntu-dev.vhdx")),
                Wait(12),
                Shot("context-menu-dark"),
                CloseMenu,
                Wait(8),
                Tab(View::Cleanup),
                Wait(16),
                Shot("cleanup-dark"),
                WaitGit,
                Filter(Some(Category::GitWorktree)),
                Wait(14),
                Shot("worktrees-dark"),
                Filter(None),
                Settings(true),
                Wait(20),
                Shot("settings-dark"),
                Wait(6),
                Quit,
            ]
        };
        Self::with_steps(dir, steps)
    }

    fn with_steps(dir: PathBuf, steps: Vec<Step>) -> Self {
        std::fs::create_dir_all(dir.join("frames")).expect("create capture directory");
        Self {
            steps: steps.into(),
            dir,
            wait: 0,
            tick: 0,
            frame: 0,
            recording: false,
            pointer: pos2(700.0, 500.0),
            motion: None,
            pending: Arc::default(),
            captions: Vec::new(),
            preview_scan: None,
            update_sender: None,
        }
    }
}

/// Synthetic drives for screenshots, so public media shows no real machine.
pub fn demo_volumes() -> Vec<rilldisk_core::Volume> {
    const GB: u64 = 1 << 30;
    [
        ("C:\\", "Local Disk", 954 * GB, 212 * GB),
        ("D:\\", "Projects", 1863 * GB, 1210 * GB),
    ]
    .into_iter()
    .map(|(root, label, total, free)| rilldisk_core::Volume {
        root: PathBuf::from(root),
        label: label.into(),
        filesystem: "NTFS".into(),
        total_bytes: total,
        free_bytes: free,
        removable: false,
    })
    .collect()
}

fn save(ctx: &egui::Context, pending: &Arc<AtomicUsize>, path: PathBuf) {
    pending.fetch_add(1, Ordering::SeqCst);
    let pending = pending.clone();
    ctx.request_screenshot(move |image| {
        // Encoding on the UI thread would stall the frames being recorded.
        std::thread::spawn(move || {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Err(error) =
                image::save_buffer(&path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
            {
                eprintln!("capture failed for {}: {error}", path.display());
            }
            pending.fetch_sub(1, Ordering::SeqCst);
        });
    });
}

fn resolve(app: &RillApp, target: Target) -> Option<usize> {
    let data = app.data.as_ref()?;
    match target {
        Target::Entry(suffix) => data
            .snapshot
            .entries
            .iter()
            .find(|e| data.snapshot.path(e.id).ends_with(Path::new(suffix)))
            .map(|e| e.id),
        Target::Candidate(category) => data
            .reclaim
            .candidates
            .iter()
            .find(|c| c.category == category)
            .map(|c| c.entry_id),
        _ => None,
    }
}

fn hit(ctx: &egui::Context, key: &str, id: usize) -> Option<Pos2> {
    ctx.data(|d| d.get_temp::<Vec<(usize, egui::Rect)>>(egui::Id::unique(key)))?
        .into_iter()
        .find(|(hit, _)| *hit == id)
        .map(|(_, rect)| rect.center())
}

/// Where a step's pointer goes: None when the target is not on screen.
fn position(app: &RillApp, ctx: &egui::Context, target: Target) -> Option<Pos2> {
    match target {
        Target::Entry(_) | Target::Candidate(_) => {
            resolve(app, target).and_then(|id| pointer_for(app, id))
        }
        Target::Row(suffix) => hit(ctx, nav::PANEL_HITS, resolve(app, Target::Entry(suffix))?),
        Target::Card(suffix) => {
            let id = resolve(app, Target::Entry(suffix))?;
            ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::unique((CARD_HITS, id))))
                .map(|rect| rect.center() + egui::vec2(-rect.width() * 0.25, 0.0))
        }
        Target::Crumb(suffix) => hit(ctx, nav::CRUMB_HITS, resolve(app, Target::Entry(suffix))?),
        Target::LiveRow(name) => {
            let view = app.live_view()?;
            let folder = view.folders.iter().find(|f| f.name == name)?;
            hit(ctx, nav::PANEL_HITS, folder.id)
        }
        Target::LiveRoot => hit(ctx, nav::CRUMB_HITS, 0),
        Target::MoreLevels => hit(ctx, nav::PANEL_HITS, nav::MORE_LEVELS),
        Target::Parent => ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::unique(nav::PARENT_BUTTON)))
            .map(|rect| rect.center()),
    }
}

/// One level of the scripted scan: name, bytes and files found so far, subfolders.
struct Folder(&'static str, u64, u64, Vec<Folder>);

/// A fictional drive part-way through a scan, with one view per folder, as a
/// running scan would publish them. Ids are assigned depth first from 1.
pub fn preview_views() -> HashMap<usize, Arc<LiveView>> {
    const GB: u64 = 1 << 30;
    const LONG: &str =
        "quarterly-brand-campaign-final-approved-exports-for-regional-print-partners";
    let deliverables: Vec<Folder> = (1..=48)
        .rev()
        .map(|i| {
            let name: &'static str = format!("project-{i:04}").leak();
            Folder(name, (i * 41) << 20, i * 9, Vec::new())
        })
        .collect();
    let archive = vec![
        Folder("client-deliverables", 86 * GB, 4_210, deliverables),
        Folder(LONG, 41 * GB, 1_930, Vec::new()),
        Folder("mail-export", 12 * GB, 88_400, Vec::new()),
        Folder("empty-placeholder", 0, 0, Vec::new()),
    ];
    let projects = vec![
        Folder("atlas-web", 34 * GB, 96_000, Vec::new()),
        Folder("ferrite-engine", 21 * GB, 31_500, Vec::new()),
    ];
    let top = vec![
        Folder("Archive", 139 * GB, 94_540, archive),
        Folder("Projects", 55 * GB, 127_500, projects),
        Folder("Videos", 48 * GB, 620, Vec::new()),
        Folder("Games", 31 * GB, 12_200, Vec::new()),
    ];
    let mut views = HashMap::new();
    let mut next = 1;
    add_views(0, &top, &[], &mut next, &mut views);
    views
}

fn add_views(
    id: usize,
    folders: &[Folder],
    ancestors: &[(usize, String)],
    next: &mut usize,
    views: &mut HashMap<usize, Arc<LiveView>>,
) {
    let live = |id: usize, folder: &Folder| LiveFolder {
        id,
        name: folder.0.to_string(),
        logical_bytes: folder.1,
        allocated_bytes: folder.1,
        files: folder.2,
        children: Vec::new(),
    };
    let ids: Vec<usize> = (0..folders.len()).map(|i| *next + i).collect();
    *next += folders.len();
    views.insert(
        id,
        Arc::new(LiveView {
            ancestors: ancestors.to_vec(),
            folders: folders
                .iter()
                .zip(&ids)
                .map(|(f, &id)| live(id, f))
                .collect(),
            ..Default::default()
        }),
    );
    for (folder, &child) in folders.iter().zip(&ids) {
        let mut chain = ancestors.to_vec();
        chain.push((child, folder.0.to_string()));
        add_views(child, &folder.3, &chain, next, views);
    }
}

pub fn advance(app: &mut RillApp, ctx: &egui::Context) {
    let Some(mut tour) = app.tour.take() else {
        return;
    };
    ctx.request_repaint();
    // A tick can queue a recording frame and a named shot. Reserve both slots;
    // pause the tour while four encoders/screenshots are in flight.
    if tour.pending.load(Ordering::SeqCst) > 2 {
        app.tour = Some(tour);
        return;
    }
    tour.tick += 1;
    if tour.recording && tour.tick % 3 == 0 {
        let path = tour
            .dir
            .join("frames")
            .join(format!("frame_{:04}.png", tour.frame));
        tour.frame += 1;
        save(ctx, &tour.pending, path);
    }
    if let Some((from, to, step, total)) = tour.motion {
        let t = (step as f32 / total as f32).min(1.0);
        let eased = t * t * (3.0 - 2.0 * t);
        tour.pointer = from + (to - from) * eased;
        app.injected.push(egui::Event::PointerMoved(tour.pointer));
        tour.motion = (step < total).then_some((from, to, step + 1, total));
        app.tour = Some(tour);
        return;
    }
    if tour.wait > 0 {
        tour.wait -= 1;
        app.tour = Some(tour);
        return;
    }
    let Some(step) = tour.steps.front().cloned() else {
        app.tour = Some(tour);
        return;
    };
    match step {
        Step::UpdatePreview(preview) => tour.update_sender = app.preview_update(preview),
        Step::ClickUpdate => {
            let rect = ctx
                .data(|d| d.get_temp::<egui::Rect>(egui::Id::unique("update-install-button")))
                .expect("visible update button");
            let pos = rect.center();
            app.injected.push(egui::Event::PointerMoved(pos));
            for pressed in [true, false] {
                app.injected.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
            }
        }
        Step::ExpectUpdating => assert!(
            app.updating(),
            "update button must start its progress state"
        ),
        Step::WaitData => {
            if app.data.is_none() {
                app.tour = Some(tour);
                return;
            }
        }
        Step::ExpectDenied => {
            let denied = app
                .data
                .as_ref()
                .map_or(0, |d| d.snapshot.summary.access_denied_count);
            if denied == 0 {
                eprintln!(
                    "--tour issues: the scan reported no access-denied folders, so the frames would lack the guidance; stopping. Scan a folder with unreadable subfolders."
                );
                tour.steps.clear();
                tour.steps.push_back(Step::Quit);
                app.tour = Some(tour);
                return;
            }
        }
        Step::Wait(n) => tour.wait = n,
        Step::Shot(name) => save(ctx, &tour.pending, tour.dir.join(format!("{name}.png"))),
        Step::Record(on) => tour.recording = on,
        Step::Move(target, frames) => {
            if let Some(to) = position(app, ctx, target) {
                tour.motion = Some((tour.pointer, to, 0, frames));
            }
        }
        Step::Click(target) => {
            let pos = position(app, ctx, target);
            assert!(
                pos.is_some() || matches!(target, Target::Entry(_) | Target::Candidate(_)),
                "navigation target must be on screen"
            );
            if let Some(pos) = pos {
                tour.pointer = pos;
                app.injected.push(egui::Event::PointerMoved(pos));
                app.injected.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                // Keep a scripted click short even when PNG capture stalls a
                // frame long enough to exceed egui's click-duration limit.
                app.injected.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }
        }
        Step::Menu(target) => {
            if let Some(pos) = position(app, ctx, target) {
                tour.pointer = pos;
                app.injected.push(egui::Event::PointerMoved(pos));
                for pressed in [true, false] {
                    app.injected.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Secondary,
                        pressed,
                        modifiers: Default::default(),
                    });
                }
            }
        }
        Step::CloseMenu => {
            for pressed in [true, false] {
                app.injected.push(egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: Default::default(),
                });
            }
        }
        Step::Filter(category) => {
            app.category_filter = category;
            app.cleanup_view = crate::app::CleanupView::for_category(category);
        }
        Step::Half(view) => app.cleanup_view = view,
        Step::ClearPicks => app.picked.clear(),
        Step::Caption(text) => {
            tour.captions.push((tour.frame, text));
            let lines: Vec<String> = tour
                .captions
                .iter()
                .map(|(frame, text)| format!("{{\"frame\": {frame}, \"text\": {text:?}}}"))
                .collect();
            let json = format!("[\n  {}\n]\n", lines.join(",\n  "));
            if let Err(error) = std::fs::write(tour.dir.join("captions.json"), json) {
                eprintln!("captions: {error}");
            }
        }
        Step::PointerAway => app.injected.push(egui::Event::PointerGone),
        Step::SelectCleanIdle => {
            if let Some(data) = app.data.clone() {
                let idle = app.clean_idle_worktrees(&data);
                app.picked.extend(idle);
            }
        }
        Step::WaitGit => {
            if !app.git_settled() {
                app.tour = Some(tour);
                return;
            }
        }
        Step::Tab(tab) => app.tab = tab,
        Step::Highlight(on) => app.highlight = on,
        Step::Search(text) => {
            app.search = text.into();
            app.run_search();
            app.tab = View::Search;
        }
        Step::ChartKind(chart) => app.chart = chart,
        Step::Root => {
            app.zoom = 0;
            app.selected = None;
        }
        Step::Back => {
            app.injected.push(egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            });
            app.injected.push(egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Default::default(),
            });
        }
        Step::ExpectFolder(suffix) => {
            let data = app
                .data
                .as_ref()
                .expect("navigation tour requires scan results");
            if suffix.is_empty() {
                assert_eq!(app.zoom, 0, "Backspace must return to scan root");
            } else {
                assert!(
                    data.snapshot.path(app.zoom).ends_with(suffix),
                    "folder click must navigate"
                );
            }
        }
        Step::ExpectSelected(suffix) => {
            assert!(
                app.selected == resolve(app, Target::Entry(suffix)),
                "going up must mark the branch that was left"
            );
        }
        Step::Wheel(delta) => {
            app.injected.push(egui::Event::PointerMoved(tour.pointer));
            app.injected.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, delta),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: Default::default(),
            });
        }
        Step::LivePreview(true) => {
            tour.preview_scan = Some(app.preview_scan(r"D:\", preview_views()));
            if let Some(control) = app.scan_control() {
                control.files.store(234_660, Ordering::Relaxed);
                control.directories.store(18_204, Ordering::Relaxed);
                control.bytes.store(273 << 30, Ordering::Relaxed);
            }
        }
        Step::LivePreview(false) => {
            app.stop_scan_preview();
            tour.preview_scan = None;
        }
        Step::ExpectLive(name) => {
            let view = app.live_view().expect("the scripted scan is running");
            assert_eq!(
                view.ancestors.last().map_or("", |a| a.1.as_str()),
                name,
                "live navigation must open the folder"
            );
        }
        Step::Theme(dark) => app.dark = dark,
        Step::Settings(open) => app.settings_open = open,
        Step::Elevated(on) => app.elevate.preview_elevated = on,
        Step::Pick(category, n) => {
            if let Some(data) = &app.data {
                let picks: Vec<usize> = data
                    .reclaim
                    .candidates
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.category == category)
                    .take(n)
                    .map(|(i, _)| i)
                    .collect();
                app.picked.extend(picks);
            }
        }
        Step::SelectSafe => crate::cleanup_ui::tour::select_safe(app),
        Step::Confirm(mode) => crate::cleanup_ui::tour::confirm(app, mode),
        Step::RunCleanup => {
            if crate::cleanup_ui::tour::busy(app) {
                app.tour = Some(tour);
                return;
            }
            crate::cleanup_ui::tour::run(app, ctx);
            if crate::cleanup_ui::tour::busy(app) {
                // Wait for the result on the following frames.
                app.tour = Some(tour);
                return;
            }
        }
        Step::Dismiss => crate::cleanup_ui::tour::dismiss(app),
        Step::Quit => {
            if tour.pending.load(Ordering::SeqCst) == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            app.tour = Some(tour);
            return;
        }
    }
    tour.steps.pop_front();
    app.tour = Some(tour);
}
