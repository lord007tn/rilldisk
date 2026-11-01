// SPDX-License-Identifier: AGPL-3.0-only
//! Squarified treemap with soft, rounded tiles. Laid out and tessellated once per
//! (root, metric, size, theme) and drawn as a single cached GPU mesh.
use crate::style::{self, FileClass, t};
use crate::widgets::fit;
use eframe::egui::{
    Align2, Color32, CornerRadius, Mesh, Painter, Pos2, Rect, Shape, Stroke, StrokeKind,
    epaint::{RectShape, TessellationOptions, Tessellator},
    pos2, vec2,
};
use rilldisk_core::{EntryKind, Snapshot};
use std::sync::Arc;

/// Upper bound on tiles per layout; keeps frames cheap on multi-million-entry scans.
const MAX_TILES: usize = 40_000;
/// Nesting levels drawn below the current folder.
const MAX_DEPTH: u16 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ColorBy {
    #[default]
    Folder,
    Kind,
}

#[derive(Clone, Copy)]
pub struct Tile {
    pub rect: Rect,
    pub id: u32,
    /// Deepest folder of a single-child folder chain drawn inside this tile.
    pub chain: u32,
    pub depth: u16,
    pub is_dir: bool,
    pub color: Color32,
    pub radius: u8,
}

pub type Key = (
    usize,
    bool,
    [i32; 4],
    usize,
    ColorBy,
    (style::Identity, bool),
    u32,
);

#[derive(Default)]
pub struct Layout {
    pub key: Option<Key>,
    pub tiles: Vec<Tile>,
    pub mesh: Arc<Mesh>,
    pub truncated: bool,
}

impl Layout {
    /// Deepest tile under `pos` (children are stored after their parents).
    pub fn hit(&self, pos: Pos2) -> Option<&Tile> {
        self.tiles.iter().rev().find(|t| t.rect.contains(pos))
    }

    pub fn tile_of(&self, id: usize) -> Option<&Tile> {
        self.tiles.iter().find(|t| t.id as usize == id)
    }
}

pub fn size_of(snapshot: &Snapshot, id: usize, allocated: bool) -> u64 {
    let entry = &snapshot.entries[id];
    if allocated {
        entry.allocated_bytes.unwrap_or(0)
    } else {
        entry.logical_bytes
    }
}

/// Children with a size, largest first.
pub fn sized_children(snapshot: &Snapshot, id: usize, allocated: bool) -> Vec<(u32, f64)> {
    let mut items: Vec<(u32, f64)> = snapshot
        .children(id)
        .iter()
        .map(|&c| (c, size_of(snapshot, c as usize, allocated) as f64))
        .filter(|(_, size)| *size > 0.0)
        .collect();
    if allocated {
        items.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    }
    items
}

/// Folds chains like SteamLibrary > steamapps > common into their deepest folder.
pub fn chain_end(snapshot: &Snapshot, id: u32) -> u32 {
    let mut chain = id;
    while let [only] = snapshot.children(chain as usize)
        && snapshot.entries[*only as usize].kind == EntryKind::Directory
    {
        chain = *only;
    }
    chain
}

pub fn file_color(snapshot: &Snapshot, id: usize) -> Color32 {
    FileClass::of(&style::extension_of(&snapshot.name(id))).color()
}

/// Nested tiles: lighter on paper, so each level reads as a frame around the
/// next; slightly lighter at night too.
fn nested(base: Color32, depth: u16) -> Color32 {
    let th = t();
    if th.dark {
        style::shade(base, depth)
    } else {
        let lift = [0.0, 0.34, 0.52, 0.64][depth.min(3) as usize];
        style::mix(base, th.bg, lift)
    }
}

pub fn neutral(depth: u16) -> Color32 {
    let th = t();
    style::mix(th.card, th.faint, 0.18 + depth as f32 * 0.07)
}

pub struct Params {
    pub root: usize,
    pub rect: Rect,
    pub allocated: bool,
    pub generation: usize,
    pub color_by: ColorBy,
    pub pixels_per_point: f32,
    /// Compact mode: smaller gaps and no headers (overview card).
    pub compact: bool,
}

pub fn key(p: &Params) -> Key {
    (
        p.root,
        p.allocated,
        [
            p.rect.min.x as i32,
            p.rect.min.y as i32,
            p.rect.max.x as i32,
            p.rect.max.y as i32,
        ],
        p.generation,
        p.color_by,
        (style::identity(), t().dark),
        (p.pixels_per_point * 100.0) as u32 + u32::from(p.compact) * 100_000,
    )
}

pub fn build(snapshot: &Snapshot, params: &Params) -> Layout {
    let mut layout = Layout {
        key: Some(key(params)),
        ..Default::default()
    };
    let mut shapes = Vec::new();
    let items = sized_children(snapshot, params.root, params.allocated);
    let total: f64 = items.iter().map(|i| i.1).sum();
    let mut placed = Vec::with_capacity(items.len().min(4096));
    squarify(&items, params.rect, &mut placed);
    // Hue follows each top-level item's position in the size order, exactly like
    // the rings view, so a folder keeps its color across views.
    let mut offsets = std::collections::HashMap::with_capacity(items.len());
    let mut cumulative = 0.0;
    for (id, size) in &items {
        offsets.insert(*id, ((cumulative + size / 2.0) / total.max(1.0)) as f32);
        cumulative += size;
    }
    for (child, rect) in placed {
        let base = match params.color_by {
            ColorBy::Folder => style::chart(offsets[&child], 0),
            ColorBy::Kind => Color32::TRANSPARENT,
        };
        place(
            snapshot,
            params,
            child,
            rect,
            0,
            base,
            &mut layout,
            &mut shapes,
        );
        if layout.truncated {
            break;
        }
    }
    let mut mesh = Mesh::default();
    let mut tessellator = Tessellator::new(
        params.pixels_per_point,
        TessellationOptions::default(),
        [1, 1],
        Vec::new(),
    );
    for shape in shapes {
        match shape {
            Plain(rect, color) => mesh.add_colored_rect(rect, color),
            Round(shape) => tessellator.tessellate_rect(&shape, &mut mesh),
        }
    }
    layout.mesh = Arc::new(mesh);
    layout
}

enum Paint {
    Plain(Rect, Color32),
    Round(RectShape),
}
use Paint::{Plain, Round};

#[allow(clippy::too_many_arguments)]
fn place(
    snapshot: &Snapshot,
    params: &Params,
    id: u32,
    slot: Rect,
    depth: u16,
    base: Color32,
    layout: &mut Layout,
    shapes: &mut Vec<Paint>,
) {
    if layout.tiles.len() >= MAX_TILES {
        layout.truncated = true;
        return;
    }
    let gap = match (params.compact, depth) {
        (true, 0) => 4.0,
        (true, _) => 2.0,
        (false, 0) => 6.0,
        (false, 1) => 3.0,
        _ => 2.0,
    };
    let rect = slot.shrink(gap / 2.0);
    if rect.width() < 2.0 || rect.height() < 2.0 {
        return;
    }
    let entry = &snapshot.entries[id as usize];
    let is_dir = entry.kind == EntryKind::Directory;
    let color = match params.color_by {
        ColorBy::Folder => nested(base, depth),
        ColorBy::Kind if is_dir => neutral(depth),
        ColorBy::Kind => style::shade(file_color(snapshot, id as usize), depth.min(1)),
    };
    let wanted: f32 = match depth {
        0 => 10.0,
        1 => 7.0,
        2 => 5.0,
        _ => 3.0,
    };
    let radius = wanted
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0)
        .floor();
    let chain = if is_dir { chain_end(snapshot, id) } else { id };
    layout.tiles.push(Tile {
        rect,
        id,
        chain,
        depth,
        is_dir,
        color,
        radius: radius as u8,
    });
    if radius >= 1.0 && rect.width() > 6.0 && rect.height() > 6.0 {
        shapes.push(Round(RectShape::filled(
            rect,
            CornerRadius::same(radius as u8),
            color,
        )));
    } else {
        shapes.push(Plain(rect, color));
    }
    if !is_dir || depth + 1 >= MAX_DEPTH {
        return;
    }
    let header = header_height(rect, depth, params.compact);
    let pad = if depth == 0 { 6.0 } else { 3.0 };
    let mut body = rect.shrink(pad);
    body.min.y = rect.min.y + header.max(pad);
    if body.width() < 12.0 || body.height() < 12.0 {
        return;
    }
    let items = sized_children(snapshot, chain as usize, params.allocated);
    let mut placed = Vec::with_capacity(items.len().min(1024));
    squarify(&items, body, &mut placed);
    for (child, slot) in placed {
        place(
            snapshot,
            params,
            child,
            slot,
            depth + 1,
            base,
            layout,
            shapes,
        );
        if layout.truncated {
            return;
        }
    }
}

/// Space reserved at the top of a folder tile for its label.
pub fn header_height(rect: Rect, depth: u16, compact: bool) -> f32 {
    match depth {
        0 if !compact && rect.height() >= 80.0 && rect.width() >= 80.0 => 40.0,
        0 if rect.height() >= 50.0 && rect.width() >= 60.0 => 22.0,
        1 if !compact && rect.height() >= 60.0 && rect.width() >= 70.0 => 20.0,
        _ => 0.0,
    }
}

fn label_for(snapshot: &Snapshot, tile: &Tile) -> String {
    let (top, end) = (tile.id as usize, tile.chain as usize);
    if top == end {
        snapshot.name(top).into_owned()
    } else if snapshot.entries[end].parent_id == Some(top) {
        format!("{} › {}", snapshot.name(top), snapshot.name(end))
    } else {
        format!("{} › … › {}", snapshot.name(top), snapshot.name(end))
    }
}

/// Names and sizes on tiles that have room for them.
pub fn paint_labels(
    painter: &Painter,
    layout: &Layout,
    snapshot: &Snapshot,
    allocated: bool,
    compact: bool,
) {
    let th = t();
    for tile in &layout.tiles {
        let r = tile.rect;
        let header = if tile.is_dir {
            header_height(r, tile.depth, compact)
        } else {
            0.0
        };
        // Only label tiles wide enough to show a useful part of the name.
        let roomy = r.width() >= 96.0;
        let two_lines = (tile.is_dir && header >= 40.0)
            || (!tile.is_dir && tile.depth <= 1 && roomy && r.height() >= 44.0);
        let one_line = (tile.is_dir && header >= 20.0 && roomy)
            || (!tile.is_dir && roomy && r.height() >= 24.0 && !compact);
        if !two_lines && !one_line {
            continue;
        }
        let pad = if tile.depth == 0 { 9.0 } else { 6.0 };
        let width = r.width() - pad * 2.0;
        let name = label_for(snapshot, tile);
        let size = style::bytes(size_of(snapshot, tile.id as usize, allocated));
        let top = r.min + vec2(pad, if tile.depth == 0 { 7.0 } else { 4.0 });
        let name_font = style::semibold(if tile.depth == 0 { 12.5 } else { 11.0 });
        if two_lines {
            let galley = fit(painter, name, name_font, th.ink, width);
            painter.galley(top, galley, th.ink);
            let galley = fit(painter, size, style::regular(11.0), th.ink_dim, width);
            painter.galley(top + vec2(0.0, 16.0), galley, th.ink_dim);
        } else {
            let galley = fit(painter, name, name_font, th.ink, width - 50.0);
            let name_w = galley.size().x;
            painter.galley(top, galley, th.ink);
            if width - name_w > 46.0 {
                painter.text(
                    top + vec2(name_w + 6.0, 1.0),
                    Align2::LEFT_TOP,
                    size,
                    style::regular(10.5),
                    th.ink_dim,
                );
            }
        }
    }
}

/// Hover or selection ring that follows the tile's rounding.
pub fn outline(painter: &Painter, tile: &Tile, color: Color32, width: f32) {
    painter.rect_stroke(
        tile.rect,
        tile.radius,
        Stroke::new(width, color),
        StrokeKind::Inside,
    );
}

pub fn wash(painter: &Painter, tile: &Tile, color: Color32) {
    painter.add(Shape::Rect(RectShape::filled(
        tile.rect,
        CornerRadius::same(tile.radius),
        color,
    )));
}

/// Bruls/Huizing/van Wijk squarified layout. `items` must be sorted descending.
pub fn squarify(items: &[(u32, f64)], rect: Rect, out: &mut Vec<(u32, Rect)>) {
    let total: f64 = items.iter().map(|i| i.1).sum();
    if total <= 0.0 {
        return;
    }
    let scale = (rect.width() as f64 * rect.height() as f64) / total;
    let mut free = rect;
    let mut i = 0;
    while i < items.len() {
        let (w, h) = (free.width() as f64, free.height() as f64);
        let short = w.min(h);
        if short <= 0.25 {
            break;
        }
        // Remaining items too small to be seen: stop early.
        if items[i].1 * scale < 0.25 {
            break;
        }
        let mut sum = 0.0;
        let mut j = i;
        let mut best = f64::INFINITY;
        let (mut lo, mut hi) = (f64::MAX, 0.0_f64);
        while j < items.len() {
            let area = items[j].1 * scale;
            let s = sum + area;
            let (nlo, nhi) = (lo.min(area), hi.max(area));
            let worst = ((short * short * nhi) / (s * s)).max((s * s) / (short * short * nlo));
            if worst > best {
                break;
            }
            best = worst;
            sum = s;
            lo = nlo;
            hi = nhi;
            j += 1;
        }
        let thickness = sum / short;
        if w >= h {
            let mut y = free.min.y;
            for item in &items[i..j] {
                let height = (item.1 * scale / thickness) as f32;
                out.push((
                    item.0,
                    Rect::from_min_size(pos2(free.min.x, y), vec2(thickness as f32, height)),
                ));
                y += height;
            }
            free.min.x += thickness as f32;
        } else {
            let mut x = free.min.x;
            for item in &items[i..j] {
                let width = (item.1 * scale / thickness) as f32;
                out.push((
                    item.0,
                    Rect::from_min_size(pos2(x, free.min.y), vec2(width, thickness as f32)),
                ));
                x += width;
            }
            free.min.y += thickness as f32;
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squarify_preserves_area_and_stays_inside() {
        let items: Vec<(u32, f64)> = [60.0, 30.0, 20.0, 10.0, 5.0, 3.0, 1.0]
            .iter()
            .enumerate()
            .map(|(i, &s)| (i as u32, s))
            .collect();
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(400.0, 300.0));
        let mut out = Vec::new();
        squarify(&items, rect, &mut out);
        assert_eq!(out.len(), items.len());
        let area: f32 = out.iter().map(|(_, r)| r.area()).sum();
        assert!((area - rect.area()).abs() / rect.area() < 0.001, "{area}");
        for (_, r) in &out {
            assert!(rect.expand(0.01).contains_rect(*r), "{r:?}");
        }
        // Squarified tiles should not be slivers.
        let (_, first) = out[0];
        assert!(first.aspect_ratio().max(1.0 / first.aspect_ratio()) < 3.0);
    }
}
