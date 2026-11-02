// SPDX-License-Identifier: AGPL-3.0-only
//! The dashboard while a scan runs: the largest folders found so far, drawn as
//! a treemap whose tiles grow (smoothly) as the scanner reads more.
use crate::app::nav;
use crate::style::{self, t};
use crate::treemap;
use crate::widgets as w;
use eframe::egui::{self, Align2, Color32, Rect, Sense, Ui, pos2, vec2};
use rilldisk_core::{LiveFolder, LiveView, ScanControl};
use std::{collections::HashMap, path::PathBuf};

/// Tile positions carried between frames so growth animates instead of jumping.
#[derive(Default)]
pub struct Anim {
    rects: HashMap<String, Rect>,
    last_time: Option<f64>,
    pressed: Option<usize>,
    /// Selected folder; going up selects the branch the user just left.
    came_from: Option<usize>,
    reveal: bool,
}

impl Anim {
    pub fn reset(&mut self) {
        self.rects.clear();
        self.last_time = None;
        self.pressed = None;
        self.came_from = None;
        self.reveal = false;
    }

    /// Browse to `id`, remembering the branch left when going up from `view`.
    pub fn open(&mut self, control: &ScanControl, id: usize, view: Option<&LiveView>) {
        let came_from = view.and_then(|view| branch_below(view, id));
        control.focus_live(id);
        self.reset();
        self.came_from = came_from;
        self.reveal = came_from.is_some();
    }

    #[cfg(test)]
    pub fn selection(&self) -> Option<usize> {
        self.came_from
    }

    /// Select a visible folder, or open the selected folder on Enter.
    pub fn keyboard(
        &mut self,
        view: &LiveView,
        up: bool,
        down: bool,
        enter: bool,
    ) -> Option<usize> {
        if (up || down) && !view.folders.is_empty() {
            let position = self
                .came_from
                .and_then(|id| view.folders.iter().position(|f| f.id == id));
            let next = match position {
                None => 0,
                Some(p) if up => p.saturating_sub(1),
                Some(p) => (p + 1).min(view.folders.len() - 1),
            };
            self.came_from = Some(view.folders[next].id);
            self.reveal = true;
        }
        self.came_from
            .filter(|id| enter && view.folders.iter().any(|f| f.id == *id))
    }

    /// Move the remembered rect for `key` toward `target`.
    fn step(&mut self, key: String, target: Rect, k: f32) -> Rect {
        // A folder seen for the first time appears in place; only later growth
        // and reshuffles animate, so nothing streaks in from a point.
        let current = self.rects.entry(key).or_insert(target);
        let lerp = |a: f32, b: f32| a + (b - a) * k;
        *current = Rect::from_min_max(
            pos2(
                lerp(current.min.x, target.min.x),
                lerp(current.min.y, target.min.y),
            ),
            pos2(
                lerp(current.max.x, target.max.x),
                lerp(current.max.y, target.max.y),
            ),
        );
        *current
    }
}

/// Navigate only from the view the user actually sees, after it catches up.
pub fn parent(view: Option<&LiveView>, requested: usize) -> Option<usize> {
    let ancestors = &view?.ancestors;
    (ancestors.last()?.0 == requested).then(|| ancestors.iter().rev().nth(1).map_or(0, |a| a.0))
}

/// Whether the published view shows the requested folder yet.
pub fn caught_up(view: &LiveView, requested: usize) -> bool {
    view.ancestors.last().map_or(0, |a| a.0) == requested
}

/// The child of ancestor `id` that leads to the shown folder.
fn branch_below(view: &LiveView, id: usize) -> Option<usize> {
    let mut chain = view.ancestors.iter().map(|a| a.0);
    if id != 0 {
        chain.find(|&a| a == id)?;
    }
    chain.next()
}

/// Everything found so far in the shown folder.
pub fn total(view: &LiveView, allocated: bool) -> u64 {
    let loose = if allocated {
        view.loose_allocated_bytes
    } else {
        view.loose_logical_bytes
    };
    loose
        + view
            .folders
            .iter()
            .map(|f| value(f, allocated))
            .sum::<u64>()
}

/// Scan root to the shown folder. Only the shown folder has a total yet.
pub fn location(root: &str, view: &LiveView, allocated: bool) -> Vec<nav::Place> {
    let mut path = PathBuf::from(root);
    let mut places = vec![nav::Place {
        id: 0,
        name: nav::root_name(root),
        detail: root.to_string(),
        size: String::new(),
    }];
    for (id, name) in &view.ancestors {
        path.push(name);
        places.push(nav::Place {
            id: *id,
            name: name.clone(),
            detail: path.display().to_string(),
            size: String::new(),
        });
    }
    if let Some(shown) = places.last_mut() {
        shown.size = style::bytes(total(view, allocated));
    }
    places
}

fn value(folder: &LiveFolder, allocated: bool) -> u64 {
    if allocated {
        folder.allocated_bytes
    } else {
        folder.logical_bytes
    }
}

/// Draw the live treemap and the folder panel into `area`.
pub fn paint(
    ui: &mut Ui,
    area: Rect,
    view: Option<&LiveView>,
    places: &[nav::Place],
    anim: &mut Anim,
    allocated: bool,
) -> Option<usize> {
    let mut clicked = None;
    let th = t();
    let mut hovered_top = None;
    let now = ui.input(|i| i.time);
    let dt = anim
        .last_time
        .map_or(0.016, |last| (now - last) as f32)
        .clamp(0.0, 0.1);
    anim.last_time = Some(now);
    // ~90% of the way in about 150 ms: lively, without long overlaps.
    let k = 1.0 - (-dt * 15.0).exp();

    let list_width = (area.width() * 0.3).clamp(260.0, 360.0);
    let map_rect = Rect::from_min_max(
        area.min,
        pos2(area.right() - list_width - 16.0, area.bottom()),
    );
    let list_rect = Rect::from_min_max(pos2(map_rect.right() + 16.0, area.top()), area.max);
    let p = ui.painter_at(area);
    p.rect_filled(map_rect, 18, th.card);
    p.rect_filled(list_rect, 18, th.card);

    let Some(view) = view else {
        p.text(
            map_rect.center(),
            Align2::CENTER_CENTER,
            "Reading the first folders…",
            style::regular(13.0),
            th.faint,
        );
        return None;
    };

    // ---- treemap of first-level folders, with their largest subfolders inside.
    let inner = map_rect.shrink(10.0);
    let tops: Vec<&LiveFolder> = view
        .folders
        .iter()
        .filter(|f| value(f, allocated) > 0)
        .take(48)
        .collect();
    let total: u64 = tops.iter().map(|f| value(f, allocated)).sum();
    let items: Vec<(u32, f64)> = tops
        .iter()
        .enumerate()
        .map(|(i, f)| (i as u32, value(f, allocated) as f64))
        .collect();
    let mut placed = Vec::new();
    treemap::squarify(&items, inner, &mut placed);
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let mut hovered: Option<(&LiveFolder, Option<&LiveFolder>)> = None;
    let mut offset = 0u64;
    let mut position = HashMap::new();
    for (i, f) in tops.iter().enumerate() {
        position.insert(
            i as u32,
            (offset as f32 + value(f, allocated) as f32 / 2.0) / total.max(1) as f32,
        );
        offset += value(f, allocated);
    }
    for (index, target) in placed {
        let folder = tops[index as usize];
        let rect = anim.step(folder.name.clone(), target.shrink(2.0), k);
        if rect.width() < 2.0 || rect.height() < 2.0 {
            continue;
        }
        let base = style::chart(position[&index], 0);
        p.rect_filled(rect, radius(rect, 8.0), base);
        let (ink, ink_dim) = treemap::ink_on(base);
        let roomy = rect.width() > 70.0 && rect.height() > 40.0;
        let header = if roomy { 34.0 } else { 0.0 };
        if roomy {
            let name = w::fit(
                &p,
                folder.name.clone(),
                style::semibold(12.5),
                ink,
                rect.width() - 16.0,
            );
            p.galley(rect.left_top() + vec2(8.0, 5.0), name, ink);
            p.text(
                rect.left_top() + vec2(8.0, 20.0),
                Align2::LEFT_TOP,
                style::bytes(value(folder, allocated)),
                style::regular(11.0),
                ink_dim,
            );
        }
        // Subfolders fill the tile below its header, in proportion to the tile.
        let body = Rect::from_min_max(rect.min + vec2(4.0, header), rect.max - vec2(4.0, 4.0));
        let own = value(folder, allocated) as f64;
        if body.width() > 24.0 && body.height() > 24.0 && own > 0.0 {
            let kids: Vec<&LiveFolder> = folder
                .children
                .iter()
                .filter(|c| value(c, allocated) > 0)
                .collect();
            let shown: f64 = kids.iter().map(|c| value(c, allocated) as f64).sum();
            // Scale the area so subfolders cover only their share of the folder.
            let share = (shown / own).clamp(0.0, 1.0) as f32;
            let kid_area = if body.width() >= body.height() {
                Rect::from_min_size(body.min, vec2(body.width() * share, body.height()))
            } else {
                Rect::from_min_size(body.min, vec2(body.width(), body.height() * share))
            };
            let kid_items: Vec<(u32, f64)> = kids
                .iter()
                .enumerate()
                .map(|(i, c)| (i as u32, value(c, allocated) as f64))
                .collect();
            let mut kid_rects = Vec::new();
            treemap::squarify(&kid_items, kid_area, &mut kid_rects);
            let fill = style::mix(base, th.bg, 0.34);
            for (ci, target) in kid_rects {
                let kid = kids[ci as usize];
                let r = anim.step(
                    format!("{}\u{0}{}", folder.name, kid.name),
                    target.shrink(1.5),
                    k,
                );
                if r.width() < 1.5 || r.height() < 1.5 {
                    continue;
                }
                p.rect_filled(r, radius(r, 5.0), fill);
                if r.width() > 64.0 && r.height() > 28.0 {
                    let (kink, kdim) = treemap::ink_on(fill);
                    let name = w::fit(
                        &p,
                        kid.name.clone(),
                        style::regular(11.5),
                        kink,
                        r.width() - 12.0,
                    );
                    p.galley(r.left_top() + vec2(6.0, 4.0), name, kink);
                    if r.height() > 40.0 {
                        p.text(
                            r.left_top() + vec2(6.0, 19.0),
                            Align2::LEFT_TOP,
                            style::bytes(value(kid, allocated)),
                            style::regular(10.5),
                            kdim,
                        );
                    }
                }
                if pointer.is_some_and(|pt| r.contains(pt)) {
                    hovered = Some((folder, Some(kid)));
                }
            }
        }
        if hovered.is_none() && pointer.is_some_and(|pt| rect.contains(pt)) {
            hovered = Some((folder, None));
        }
    }
    let response = ui.interact(map_rect, ui.scope_id().with("live-map"), Sense::click());
    if ui.input(|i| i.pointer.primary_pressed()) {
        anim.pressed = hovered
            .filter(|_| response.contains_pointer())
            .map(|(folder, kid)| kid.unwrap_or(folder).id);
    }
    if response.clicked() {
        clicked = anim.pressed.take();
    }
    if ui.input(|i| i.pointer.primary_released()) {
        anim.pressed = None;
    }
    if let Some((folder, kid)) = hovered {
        hovered_top = Some(folder.id);
        response.on_hover_ui_at_pointer(|ui| {
            let f = kid.unwrap_or(folder);
            let label = match kid {
                Some(k) => format!("{} / {}", folder.name, k.name),
                None => folder.name.clone(),
            };
            ui.label(egui::RichText::new(label).font(style::semibold(13.0)));
            ui.label(format!(
                "{} so far · {} files",
                style::bytes(value(f, allocated)),
                style::count(f.files)
            ));
        });
    }

    // Every immediate folder remains accessible in the panel, even if it has no
    // sized tiles yet or is too small to hit on the map.
    let tile_color: HashMap<usize, Color32> = tops
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, style::chart(position[&(i as u32)], 0)))
        .collect();
    let reveal = anim
        .came_from
        .filter(|_| anim.reveal)
        .and_then(|id| view.folders.iter().position(|f| f.id == id));
    if reveal.is_some() {
        anim.reveal = false;
    }
    let spec = nav::Panel {
        places,
        summary: format!(
            "{} · {} directly here",
            style::plural(view.folders.len() as u64, "folder"),
            style::plural(view.loose_files, "file")
        ),
        notice: None,
        rows: view.folders.len(),
        empty: "No subfolders found yet. Scanning continues.",
        selected: anim.came_from,
        hovered: hovered_top,
        reveal,
    };
    let row = |index: usize| {
        let folder = &view.folders[index];
        nav::Row {
            id: folder.id,
            name: folder.name.clone(),
            size: style::bytes(value(folder, allocated)),
            folder: true,
            dot: tile_color.get(&folder.id).copied().unwrap_or(th.faint),
            mark: None,
            partial: false,
            tip: format!(
                "{}\n{} files so far · click to open",
                folder.name,
                style::count(folder.files)
            ),
        }
    };
    let result = nav::panel(ui, list_rect.shrink(8.0), spec, row, |_, _| {});
    clicked.or(result.go).or(result.clicked.map(|row| row.0))
}

/// Corner radius that never turns a small tile into a dot.
fn radius(rect: Rect, max: f32) -> f32 {
    max.min(rect.width() / 4.0).min(rect.height() / 4.0)
}

/// Header strip of a running scan: what, how much, how fast, and Stop.
pub struct Progress {
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
    pub seconds: f64,
}

impl Progress {
    pub fn summary(&self) -> String {
        format!(
            "{} files · {} folders · {} logical · {}/s · {}",
            style::short_count(self.files),
            style::short_count(self.folders),
            style::bytes(self.bytes),
            style::short_count((self.files as f64 / self.seconds.max(0.001)) as u64),
            clock(self.seconds)
        )
    }
}

pub fn clock(seconds: f64) -> String {
    let s = seconds as u64;
    if s < 60 {
        format!("{seconds:.1} s")
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// An indeterminate progress line: a soft highlight sweeping across.
pub fn sweep(p: &egui::Painter, rect: Rect, time: f64, color: Color32) {
    let th = t();
    p.rect_filled(rect, rect.height() / 2.0, th.field);
    let width = rect.width() * 0.28;
    let phase = ((time * 0.55) % 1.0) as f32;
    let x = rect.left() - width + (rect.width() + width) * phase;
    let bar = Rect::from_min_max(
        pos2(x.max(rect.left()), rect.top()),
        pos2((x + width).min(rect.right()), rect.bottom()),
    );
    if bar.width() > 0.0 {
        p.rect_filled(bar, rect.height() / 2.0, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_waits_for_current_view_and_moves_exactly_one_level() {
        let view = LiveView {
            ancestors: vec![(1, "A".into()), (2, "B".into())],
            ..Default::default()
        };
        assert_eq!(parent(Some(&view), 3), None);
        assert_eq!(parent(Some(&view), 2), Some(1));
        let top = LiveView {
            ancestors: vec![(1, "A".into())],
            ..Default::default()
        };
        assert_eq!(parent(Some(&top), 1), Some(0));
        assert_eq!(parent(None, 1), None);
        assert_eq!(parent(Some(&LiveView::default()), 0), None);
    }

    #[test]
    fn going_up_remembers_the_branch_that_was_left() {
        let view = LiveView {
            ancestors: vec![(1, "A".into()), (2, "B".into()), (3, "C".into())],
            ..Default::default()
        };
        assert_eq!(branch_below(&view, 0), Some(1));
        assert_eq!(branch_below(&view, 1), Some(2));
        assert_eq!(branch_below(&view, 2), Some(3));
        // The shown folder and its children have nothing below them to mark.
        assert_eq!(branch_below(&view, 3), None);
        assert_eq!(branch_below(&view, 9), None);
    }

    #[test]
    fn location_names_the_chain_and_totals_only_the_shown_folder() {
        let view = LiveView {
            ancestors: vec![(4, "Projects".into())],
            folders: vec![LiveFolder {
                logical_bytes: 2048,
                ..Default::default()
            }],
            loose_logical_bytes: 1024,
            ..Default::default()
        };
        let list = location(r"C:\Synthetic\home", &view, false);
        assert_eq!(list[0].name, "home");
        assert_eq!(list[0].size, "");
        assert_eq!(list[1].id, 4);
        assert!(list[1].detail.ends_with("Projects"));
        assert_eq!(list[1].size, style::bytes(3072));
        assert!(caught_up(&view, 4));
        assert!(!caught_up(&view, 0));
    }

    #[test]
    fn panel_row_click_enters_a_folder_during_scan() {
        let ctx = egui::Context::default();
        let view = LiveView {
            folders: vec![LiveFolder {
                id: 42,
                name: "folder without any size yet".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let places = location("C:\\Synthetic", &view, false);
        let mut anim = Anim::default();
        let mut opened = None;
        let hit = |ctx: &egui::Context| {
            ctx.data(|d| d.get_temp::<Vec<(usize, Rect)>>(egui::Id::unique(nav::PANEL_HITS)))
                .and_then(|hits| hits.first().copied())
        };
        for step in 0..3 {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0))),
                ..Default::default()
            };
            if let Some((_, rect)) = hit(&ctx).filter(|_| step > 0) {
                let pos = rect.center();
                input.events.push(egui::Event::PointerMoved(pos));
                if step == 2 {
                    for pressed in [true, false] {
                        input.events.push(egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        });
                    }
                }
            }
            ctx.run_ui(input, |ui| {
                opened = paint(
                    ui,
                    ui.available_rect_before_wrap(),
                    Some(&view),
                    &places,
                    &mut anim,
                    false,
                )
                .or(opened);
            })
            .drop_without_applying_deltas();
        }
        assert_eq!(opened, Some(42));
    }

    #[test]
    fn map_click_enters_a_folder_during_scan() {
        let ctx = egui::Context::default();
        let mut view = LiveView {
            folders: vec![LiveFolder {
                id: 42,
                name: "Synthetic folder".into(),
                logical_bytes: 100,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut anim = Anim::default();
        let mut selected = None;
        let places = location("C:\\Synthetic", &view, false);
        for pressed in [None, Some(true), Some(false)] {
            // A publication replaces the tile under the stationary pointer
            // between press and release; the original folder must still open.
            if pressed == Some(false) {
                view.folders[0].id = 43;
            }
            let pos = pos2(150.0, 150.0);
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0))),
                ..Default::default()
            };
            input.events.push(egui::Event::PointerMoved(pos));
            if let Some(pressed) = pressed {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
            }
            ctx.run_ui(input, |ui| {
                selected = paint(
                    ui,
                    ui.available_rect_before_wrap(),
                    Some(&view),
                    &places,
                    &mut anim,
                    false,
                )
                .or(selected);
            })
            .drop_without_applying_deltas();
        }
        assert_eq!(selected, Some(42));
    }
}
