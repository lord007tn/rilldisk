// SPDX-License-Identifier: AGPL-3.0-only
//! Rings: a sunburst of the current folder. Each ring is one level deeper; angles
//! are proportional to size. Built once per (root, metric, size, theme) into one
//! anti-aliased mesh, then hit-tested in polar coordinates.
use crate::style;
use crate::treemap::{self, ColorBy};
use eframe::egui::{Color32, Mesh, Pos2, epaint::Vertex, epaint::WHITE_UV, vec2};
use rilldisk_core::{EntryKind, Snapshot};
use std::f32::consts::{PI, TAU};
use std::sync::Arc;

pub const LEVELS: u16 = 6;
const MAX_SEGMENTS: usize = 60_000;
/// Start at twelve o'clock and run clockwise.
const START: f32 = -PI / 2.0;

#[derive(Clone, Copy)]
pub struct Segment {
    pub id: u32,
    pub depth: u16,
    pub a0: f32,
    pub a1: f32,
    pub color: Color32,
}

#[derive(Default)]
pub struct Rings {
    pub key: Option<treemap::Key>,
    pub center: Pos2,
    pub inner: f32,
    pub ring: f32,
    pub segments: Vec<Segment>,
    pub mesh: Arc<Mesh>,
}

pub enum Hit {
    Center,
    Segment(Segment),
}

impl Rings {
    pub fn radii(&self, depth: u16) -> (f32, f32) {
        let ri = self.inner + (depth - 1) as f32 * self.ring;
        (ri + 1.0, ri + self.ring - 1.0)
    }

    pub fn outer(&self) -> f32 {
        self.inner + LEVELS as f32 * self.ring
    }

    pub fn hit(&self, pos: Pos2) -> Option<Hit> {
        let d = pos - self.center;
        let r = d.length();
        if r <= self.inner {
            return Some(Hit::Center);
        }
        let depth = ((r - self.inner) / self.ring).floor() as u16 + 1;
        if depth > LEVELS {
            return None;
        }
        let angle = (d.y.atan2(d.x) - START).rem_euclid(TAU);
        self.segments
            .iter()
            .find(|s| s.depth == depth && angle >= s.a0 && angle < s.a1)
            .copied()
            .map(Hit::Segment)
    }

    pub fn segment_of(&self, id: usize) -> Option<&Segment> {
        self.segments.iter().find(|s| s.id as usize == id)
    }

    /// A point in the middle of a segment, for scripted pointer moves.
    pub fn anchor(&self, segment: &Segment) -> Pos2 {
        let (ri, ro) = self.radii(segment.depth);
        let a = START + (segment.a0 + segment.a1) / 2.0;
        self.center + vec2(a.cos(), a.sin()) * (ri + ro) / 2.0
    }

    /// Paints one segment again (for hover) with a different color.
    pub fn highlight(&self, mesh: &mut Mesh, segment: &Segment, color: Color32) {
        let (ri, ro) = self.radii(segment.depth);
        sector(mesh, self.center, ri, ro, segment.a0, segment.a1, color);
    }
}

pub fn build(snapshot: &Snapshot, params: &treemap::Params) -> Rings {
    let rect = params.rect;
    let radius = (rect.width().min(rect.height()) / 2.0 - 6.0).max(40.0);
    let inner = radius * 0.27;
    let mut rings = Rings {
        key: Some(treemap::key(params)),
        center: rect.center(),
        inner,
        ring: (radius - inner) / LEVELS as f32,
        ..Default::default()
    };
    let mut mesh = Mesh::default();
    walk(
        snapshot,
        params,
        params.root,
        1,
        0.0,
        TAU,
        &mut rings,
        &mut mesh,
    );
    rings.mesh = Arc::new(mesh);
    rings
}

#[allow(clippy::too_many_arguments)]
fn walk(
    snapshot: &Snapshot,
    params: &treemap::Params,
    id: usize,
    depth: u16,
    a0: f32,
    a1: f32,
    rings: &mut Rings,
    mesh: &mut Mesh,
) {
    if depth > LEVELS || rings.segments.len() >= MAX_SEGMENTS {
        return;
    }
    let parent = treemap::size_of(snapshot, id, params.allocated).max(1) as f64;
    let (ri, ro) = rings.radii(depth);
    let mut angle = a0 as f64;
    let span = (a1 - a0) as f64;
    for (child, size) in treemap::sized_children(snapshot, id, params.allocated) {
        let sweep = span * size / parent;
        // Too thin to see at this radius; the rest are smaller still.
        if sweep * (ri as f64) < 1.2 {
            break;
        }
        let (s0, s1) = (angle as f32, (angle + sweep) as f32);
        angle += sweep;
        let entry = &snapshot.entries[child as usize];
        let is_dir = entry.kind == EntryKind::Directory;
        let color = match params.color_by {
            ColorBy::Folder => style::chart((s0 + s1) / 2.0 / TAU, depth - 1),
            ColorBy::Kind if is_dir => treemap::neutral(depth),
            ColorBy::Kind => treemap::file_color(snapshot, child as usize),
        };
        sector(mesh, rings.center, ri, ro, s0, s1, color);
        rings.segments.push(Segment {
            id: child,
            depth,
            a0: s0,
            a1: s1,
            color,
        });
        if is_dir {
            walk(
                snapshot,
                params,
                child as usize,
                depth + 1,
                s0,
                s1,
                rings,
                mesh,
            );
        }
    }
}

/// Anti-aliased annular sector: a 4 x n vertex grid whose outer rows and columns
/// fade to transparent over one pixel. A small gap separates neighbours.
pub fn sector(mesh: &mut Mesh, center: Pos2, ri: f32, ro: f32, a0: f32, a1: f32, color: Color32) {
    const FEATHER: f32 = 1.0;
    const GAP: f32 = 1.2;
    let radii = [
        ri - FEATHER / 2.0,
        ri + FEATHER / 2.0,
        ro - FEATHER / 2.0,
        ro + FEATHER / 2.0,
    ];
    let mid = (ri + ro) / 2.0;
    let span = a1 - a0 - GAP / mid;
    if span <= 0.0 {
        return;
    }
    // Very thin slivers fade instead of collapsing.
    let thin = (span * mid / FEATHER).min(1.0);
    let color = if thin < 1.0 {
        color.gamma_multiply(thin)
    } else {
        color
    };
    let steps = ((span * ro) / 3.0).ceil().clamp(1.0, 240.0) as usize;
    let base = mesh.vertices.len() as u32;
    let columns = steps + 3;
    for (row, &r) in radii.iter().enumerate() {
        let edge_row = row == 0 || row == 3;
        let gap = GAP / 2.0 / r.max(1.0);
        let feather = FEATHER / 2.0 / r.max(1.0);
        let (lo, hi) = (a0 + gap, a1 - gap);
        let core_lo = (lo + feather).min((lo + hi) / 2.0);
        let core_hi = (hi - feather).max((lo + hi) / 2.0);
        for column in 0..columns {
            let (angle, edge_column) = match column {
                0 => (lo - feather, true),
                c if c == columns - 1 => (hi + feather, true),
                c => (
                    core_lo + (core_hi - core_lo) * (c - 1) as f32 / steps as f32,
                    false,
                ),
            };
            let a = START + angle;
            let pos = center + vec2(a.cos(), a.sin()) * r;
            let c = if edge_row || edge_column {
                Color32::TRANSPARENT
            } else {
                color
            };
            mesh.vertices.push(Vertex {
                pos,
                uv: WHITE_UV,
                color: c,
            });
        }
    }
    let w = columns as u32;
    for row in 0..3_u32 {
        for column in 0..(w - 1) {
            let i = base + row * w + column;
            mesh.indices
                .extend_from_slice(&[i, i + 1, i + w, i + 1, i + w + 1, i + w]);
        }
    }
}
