// SPDX-License-Identifier: AGPL-3.0-only
//! The dashboard while a scan runs: the largest folders found so far, drawn as
//! a treemap whose tiles grow (smoothly) as the scanner reads more.
use crate::style::{self, t};
use crate::treemap;
use crate::widgets as w;
use eframe::egui::{self, Align2, Color32, Rect, Sense, Ui, pos2, vec2};
use rilldisk_core::{LiveFolder, LiveView};
use std::collections::HashMap;

/// Tile positions carried between frames so growth animates instead of jumping.
#[derive(Default)]
pub struct Anim {
    rects: HashMap<String, Rect>,
    last_time: Option<f64>,
}

impl Anim {
    pub fn reset(&mut self) {
        self.rects.clear();
        self.last_time = None;
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

fn value(folder: &LiveFolder, allocated: bool) -> u64 {
    if allocated {
        folder.allocated_bytes
    } else {
        folder.logical_bytes
    }
}

/// Draw the live treemap and the "largest so far" list into `area`.
pub fn paint(ui: &mut Ui, area: Rect, view: Option<&LiveView>, anim: &mut Anim, allocated: bool) {
    let th = t();
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

    let Some(view) = view.filter(|v| v.folders.iter().any(|f| value(f, allocated) > 0)) else {
        p.text(
            map_rect.center(),
            Align2::CENTER_CENTER,
            "Reading the first folders…",
            style::regular(13.0),
            th.faint,
        );
        return;
    };

    // ---- treemap of first-level folders, with their largest subfolders inside.
    let inner = map_rect.shrink(10.0);
    let tops: Vec<&LiveFolder> = view
        .folders
        .iter()
        .filter(|f| value(f, allocated) > 0)
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
    let response = ui.interact(map_rect, ui.scope_id().with("live-map"), Sense::hover());
    if let Some((folder, kid)) = hovered {
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

    // ---- "Largest so far" list.
    let lp = ui.painter_at(list_rect);
    let mut y = list_rect.top() + 18.0;
    lp.text(
        pos2(list_rect.left() + 18.0, y),
        Align2::LEFT_TOP,
        "Largest so far",
        style::semibold(14.0),
        th.text,
    );
    y += 28.0;
    let biggest = tops.first().map_or(1, |f| value(f, allocated)).max(1);
    let row_h = 40.0;
    for (i, folder) in tops.iter().enumerate() {
        if y + row_h > list_rect.bottom() - 12.0 {
            break;
        }
        let left = list_rect.left() + 18.0;
        let right = list_rect.right() - 18.0;
        let color = style::chart(position[&(i as u32)], 0);
        lp.circle_filled(pos2(left + 4.0, y + 9.0), 4.0, color);
        let size = style::bytes(value(folder, allocated));
        let size_g = lp.layout_no_wrap(size, style::semibold(12.0), th.text);
        let name = w::fit(
            &lp,
            folder.name.clone(),
            style::regular(12.5),
            th.text,
            right - left - 22.0 - size_g.size().x - 8.0,
        );
        lp.galley(pos2(left + 16.0, y + 1.0), name, th.text);
        lp.galley(pos2(right - size_g.size().x, y + 1.0), size_g, th.text);
        let bar = Rect::from_min_size(pos2(left + 16.0, y + 22.0), vec2(right - left - 16.0, 4.0));
        w::progress(
            &lp,
            bar,
            value(folder, allocated) as f32 / biggest as f32,
            color,
        );
        y += row_h;
    }
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
            "{} files · {} folders · {} · {}/s · {}",
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
