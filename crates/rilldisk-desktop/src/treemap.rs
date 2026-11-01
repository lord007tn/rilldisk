// SPDX-License-Identifier: AGPL-3.0-only
//! Squarified treemap: layout once per (root, metric, size), render as one GPU mesh.
use crate::style::{self, FileClass};
use eframe::egui::{Color32, Mesh, Pos2, Rect, pos2, vec2};
use eframe::epaint::WHITE_UV;
use rilldisk_core::{EntryKind, Snapshot};
use std::sync::Arc;

/// Upper bound on tiles per layout; keeps frames cheap on multi-million-entry scans.
const MAX_TILES: usize = 120_000;
const HEADER: f32 = 17.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TileKind {
    File(FileClass),
    /// A directory whose children are drawn inside it.
    Dir,
    /// A directory too small to subdivide.
    Collapsed,
}

#[derive(Clone, Copy)]
pub struct Tile {
    pub rect: Rect,
    pub id: u32,
    /// Deepest folder of a single-child folder chain drawn inside this tile.
    pub chain: u32,
    pub kind: TileKind,
}

#[derive(Default)]
pub struct Layout {
    pub key: Option<(usize, bool, [i32; 4], usize)>,
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

pub fn build(
    snapshot: &Snapshot,
    root: usize,
    rect: Rect,
    allocated: bool,
    generation: usize,
) -> Layout {
    let key = (
        root,
        allocated,
        [
            rect.min.x as i32,
            rect.min.y as i32,
            rect.max.x as i32,
            rect.max.y as i32,
        ],
        generation,
    );
    let mut layout = Layout {
        key: Some(key),
        ..Default::default()
    };
    let mut mesh = Mesh::default();
    subdivide(snapshot, root, rect, 0, allocated, &mut layout, &mut mesh);
    layout.mesh = Arc::new(mesh);
    layout
}

fn subdivide(
    snapshot: &Snapshot,
    id: usize,
    inner: Rect,
    depth: u16,
    allocated: bool,
    layout: &mut Layout,
    mesh: &mut Mesh,
) {
    if inner.width() < 2.0 || inner.height() < 2.0 {
        return;
    }
    let mut items: Vec<(u32, f64)> = snapshot
        .children(id)
        .iter()
        .map(|&c| (c, size_of(snapshot, c as usize, allocated) as f64))
        .filter(|(_, size)| *size > 0.0)
        .collect();
    if allocated {
        items.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    }
    let mut placed = Vec::with_capacity(items.len().min(4096));
    squarify(&items, inner, &mut placed);
    for (child, rect) in placed {
        if layout.tiles.len() >= MAX_TILES {
            layout.truncated = true;
            return;
        }
        let entry = &snapshot.entries[child as usize];
        let rect = rect.shrink(0.5);
        if rect.width() < 0.6 || rect.height() < 0.6 {
            continue;
        }
        if entry.kind == EntryKind::Directory {
            let open = rect.width() >= 28.0 && rect.height() >= 24.0;
            let kind = if open {
                TileKind::Dir
            } else {
                TileKind::Collapsed
            };
            // Fold chains like SteamLibrary > steamapps > common into one header.
            let mut chain = child;
            while let [only] = snapshot.children(chain as usize)
                && snapshot.entries[*only as usize].kind == EntryKind::Directory
            {
                chain = *only;
            }
            layout.tiles.push(Tile {
                rect,
                id: child,
                chain,
                kind,
            });
            let shade = (0.95 + depth as f32 * 0.07).min(1.35);
            if open {
                flat(mesh, rect, style::scale(style::DIR_FILL, shade));
                let header = if rect.height() >= 44.0 && rect.width() >= 64.0 {
                    HEADER
                } else {
                    2.0
                };
                let mut body = rect.shrink(2.0);
                body.min.y = rect.min.y + header;
                subdivide(
                    snapshot,
                    chain as usize,
                    body,
                    depth + 1,
                    allocated,
                    layout,
                    mesh,
                );
            } else {
                cushion(
                    mesh,
                    rect,
                    style::scale(Color32::from_rgb(71, 85, 105), 0.9),
                );
            }
        } else {
            let class = FileClass::of(&style::extension_of(&snapshot.name(child as usize)));
            layout.tiles.push(Tile {
                rect,
                id: child,
                chain: child,
                kind: TileKind::File(class),
            });
            cushion(mesh, rect, class.color());
        }
    }
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

fn flat(mesh: &mut Mesh, rect: Rect, color: Color32) {
    mesh.add_colored_rect(rect, color);
}

/// Four-corner gradient approximating a cushion: lit from the top left.
fn cushion(mesh: &mut Mesh, rect: Rect, color: Color32) {
    let base = mesh.vertices.len() as u32;
    let light = style::scale(color, 1.18);
    let mid = style::scale(color, 0.92);
    let dark = style::scale(color, 0.62);
    for (pos, c) in [
        (rect.left_top(), light),
        (rect.right_top(), mid),
        (rect.right_bottom(), dark),
        (rect.left_bottom(), mid),
    ] {
        mesh.vertices.push(eframe::epaint::Vertex {
            pos,
            uv: WHITE_UV,
            color: c,
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
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
