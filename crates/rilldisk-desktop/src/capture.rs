// SPDX-License-Identifier: AGPL-3.0-only
//! Scripted tours for reproducible screenshots and recordings (`--capture DIR`).
//!
//! Pointer movement and clicks are injected as ordinary egui input events, so the
//! recording exercises the same code paths as a person using the app.
use crate::app::{Chart, RillApp, Tab as View, pointer_for};
use eframe::egui::{self, Pos2, pos2};
use rilldisk_core::Category;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone, Copy)]
pub enum Target {
    /// Entry whose path ends with these components.
    Entry(&'static str),
    /// Largest reclaim candidate of a category.
    Candidate(Category),
}

#[derive(Clone)]
pub enum Step {
    WaitData,
    Wait(u32),
    Shot(&'static str),
    Record(bool),
    Move(Target, u32),
    Click(Target),
    Tab(View),
    Highlight(bool),
    Search(&'static str),
    ChartKind(Chart),
    /// Back to the scanned root.
    Root,
    Theme(bool),
    /// Tick the largest candidates of a category in Clean up.
    Pick(Category, usize),
    Settings(bool),
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
    release: Option<Pos2>,
    pending: Arc<AtomicUsize>,
}

impl Tour {
    pub fn new(dir: PathBuf, kind: TourKind) -> Self {
        use Step::*;
        use Target::*;
        let steps = if kind == TourKind::Welcome {
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
                Tab(View::Cleanup),
                Wait(14),
                Pick(Category::GitWorktree, 2),
                Pick(Category::BuildOutput, 1),
                Wait(18),
                Shot("cleanup"),
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
                Tab(View::Cleanup),
                Wait(16),
                Shot("cleanup-dark"),
                Settings(true),
                Wait(20),
                Shot("settings-dark"),
                Wait(6),
                Quit,
            ]
        };
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
            release: None,
            pending: Arc::default(),
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
        let [w, h] = image.size;
        let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
        if let Err(error) =
            image::save_buffer(&path, &bytes, w as u32, h as u32, image::ColorType::Rgba8)
        {
            eprintln!("capture failed for {}: {error}", path.display());
        }
        pending.fetch_sub(1, Ordering::SeqCst);
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
    }
}

pub fn advance(app: &mut RillApp, ctx: &egui::Context) {
    let Some(mut tour) = app.tour.take() else {
        return;
    };
    ctx.request_repaint();
    tour.tick += 1;
    if tour.recording && tour.tick % 3 == 0 {
        let path = tour
            .dir
            .join("frames")
            .join(format!("frame_{:04}.png", tour.frame));
        tour.frame += 1;
        save(ctx, &tour.pending, path);
    }
    if let Some(pos) = tour.release.take() {
        app.injected.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
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
        Step::WaitData => {
            if app.data.is_none() {
                app.tour = Some(tour);
                return;
            }
        }
        Step::Wait(n) => tour.wait = n,
        Step::Shot(name) => save(ctx, &tour.pending, tour.dir.join(format!("{name}.png"))),
        Step::Record(on) => tour.recording = on,
        Step::Move(target, frames) => {
            if let Some(to) = resolve(app, target).and_then(|id| pointer_for(app, id)) {
                tour.motion = Some((tour.pointer, to, 0, frames));
            }
        }
        Step::Click(target) => {
            if let Some(pos) = resolve(app, target).and_then(|id| pointer_for(app, id)) {
                tour.pointer = pos;
                app.injected.push(egui::Event::PointerMoved(pos));
                app.injected.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                tour.release = Some(pos);
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
        Step::Theme(dark) => app.dark = dark,
        Step::Settings(open) => app.settings_open = open,
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
