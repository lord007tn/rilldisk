// SPDX-License-Identifier: AGPL-3.0-only
//! Borderless components: cards, buttons, segmented controls, tabs and charts bits.
use crate::icons::{self, Icon};
use crate::style::{self, t};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, Frame, Margin, Painter, Pos2, Rect, Response, Sense,
    Shape, Stroke, Ui, pos2, vec2,
};
use std::f32::consts::{PI, TAU};

pub fn card_frame() -> Frame {
    Frame::new()
        .fill(t().card)
        .corner_radius(14)
        .inner_margin(Margin::same(16))
}

pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(style::semibold(11.0))
            .color(t().faint)
            .extra_letter_spacing(0.6),
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Soft,
    Ghost,
    /// White pill on an accent card.
    Inverse,
}

/// Rounded, borderless button with an optional leading icon.
pub fn button(ui: &mut Ui, kind: Kind, icon: Option<Icon>, text: &str) -> Response {
    let th = t();
    let font = style::semibold(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER);
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let pad = if text.is_empty() { 7.0 } else { 14.0 };
    let size = vec2(galley.size().x + icon_w + pad * 2.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let pressed = response.is_pointer_button_down_on();
        let (fill, fg) = match kind {
            Kind::Primary => (
                if pressed {
                    style::scale(th.accent, 0.9)
                } else if hovered {
                    style::mix(th.accent, Color32::WHITE, 0.12)
                } else {
                    th.accent
                },
                th.on_accent,
            ),
            Kind::Soft => (
                if pressed {
                    th.press
                } else if hovered {
                    th.hover
                } else {
                    th.card
                },
                th.text,
            ),
            Kind::Ghost => (
                if pressed {
                    th.press
                } else if hovered {
                    th.hover
                } else {
                    Color32::TRANSPARENT
                },
                th.text,
            ),
            Kind::Inverse => (
                if hovered {
                    style::alpha(Color32::WHITE, 235)
                } else {
                    Color32::WHITE
                },
                Color32::from_rgb(30, 22, 18),
            ),
        };
        let p = ui.painter();
        p.rect_filled(rect, 9, fill);
        let mut x = rect.left() + pad;
        if let Some(icon) = icon {
            icons::paint(
                p,
                Rect::from_center_size(pos2(x + 8.0, rect.center().y), vec2(15.0, 15.0)),
                icon,
                fg,
            );
            x += icon_w;
        }
        p.galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    }
    response
}

/// Square icon-only button.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    let th = t();
    let fill = if response.is_pointer_button_down_on() {
        th.press
    } else if response.hovered() {
        th.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 9, fill);
    let color = if ui.is_enabled() { th.dim } else { th.faint };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
        icon,
        color,
    );
    response.on_hover_text(tooltip)
}

/// Pill segmented control. Returns true when the value changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, Option<Icon>, &str)],
    tooltip: &str,
) -> bool {
    let th = t();
    let font = style::semibold(12.5);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, icon, label)| {
            let text = ui
                .painter()
                .layout_no_wrap(label.to_string(), font.clone(), Color32::PLACEHOLDER)
                .size()
                .x;
            text + if icon.is_some() { 20.0 } else { 0.0 } + 22.0
        })
        .collect();
    let size = vec2(widths.iter().sum::<f32>() + 6.0, 32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let response = response.on_hover_text(tooltip);
    let _ = response;
    let p = ui.painter().clone();
    p.rect_filled(rect, 10, th.field);
    let mut x = rect.left() + 3.0;
    let mut changed = false;
    for ((option, icon, label), width) in options.iter().zip(widths) {
        let seg = Rect::from_min_size(pos2(x, rect.top() + 3.0), vec2(width, 26.0));
        let id = ui.scope_id().with(("segment", label));
        let hit = ui.interact(seg, id, Sense::click());
        let active = *value == *option;
        if active {
            p.rect_filled(
                seg.translate(vec2(0.0, 1.0)),
                8,
                style::alpha(Color32::BLACK, if th.dark { 60 } else { 14 }),
            );
            p.rect_filled(seg, 8, if th.dark { th.hover } else { th.popup });
        } else if hit.hovered() {
            p.rect_filled(seg, 8, style::alpha(th.hover, 160));
        }
        let color = if active { th.text } else { th.dim };
        let galley = p.layout_no_wrap(label.to_string(), font.clone(), color);
        let content = galley.size().x + if icon.is_some() { 20.0 } else { 0.0 };
        let mut cx = seg.center().x - content / 2.0;
        if let Some(icon) = icon {
            icons::paint(
                &p,
                Rect::from_center_size(pos2(cx + 7.0, seg.center().y), vec2(14.0, 14.0)),
                *icon,
                color,
            );
            cx += 20.0;
        }
        p.galley(
            pos2(cx, seg.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        if hit.clicked() && !active {
            *value = *option;
            changed = true;
        }
        x += width;
    }
    changed
}

/// Top-level view tabs. Returns the clicked index.
pub fn tabs(
    ui: &mut Ui,
    active: usize,
    items: &[(String, Option<(String, Color32)>)],
) -> Option<usize> {
    let th = t();
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, (label, badge)) in items.iter().enumerate() {
            let font = style::semibold(13.0);
            let galley = ui
                .painter()
                .layout_no_wrap(label.clone(), font, Color32::PLACEHOLDER);
            let badge_galley = badge.as_ref().map(|(text, _)| {
                ui.painter().layout_no_wrap(
                    text.clone(),
                    style::semibold(11.0),
                    Color32::PLACEHOLDER,
                )
            });
            let badge_w = badge_galley.as_ref().map_or(0.0, |g| g.size().x + 14.0);
            let size = vec2(galley.size().x + 26.0 + badge_w, 32.0);
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            let is_active = i == active;
            let p = ui.painter();
            if is_active {
                p.rect_filled(rect, 10, th.card);
            } else if response.hovered() {
                p.rect_filled(rect, 10, style::alpha(th.hover, 150));
            }
            let color = if is_active { th.text } else { th.dim };
            p.galley(
                pos2(rect.left() + 13.0, rect.center().y - galley.size().y / 2.0),
                galley.clone(),
                color,
            );
            if let (Some(bg), Some((_, tint))) = (badge_galley, badge) {
                let b = Rect::from_min_size(
                    pos2(
                        rect.left() + 13.0 + galley.size().x + 7.0,
                        rect.center().y - 9.0,
                    ),
                    vec2(bg.size().x + 10.0, 18.0),
                );
                p.rect_filled(b, 9, style::alpha(*tint, if th.dark { 50 } else { 34 }));
                p.galley(
                    pos2(b.left() + 5.0, b.center().y - bg.size().y / 2.0),
                    bg,
                    *tint,
                );
            }
            if response.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Small tinted pill.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), style::semibold(11.0), color);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 14.0, 20.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(
        rect,
        10,
        style::alpha(color, if t().dark { 44 } else { 30 }),
    );
    p.galley(
        pos2(rect.left() + 7.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response
}

pub fn chip_at(p: &Painter, left_center: Pos2, text: &str, color: Color32) -> Rect {
    let galley = p.layout_no_wrap(text.to_string(), style::semibold(10.5), color);
    let rect = Rect::from_min_size(
        left_center - vec2(0.0, 9.0),
        vec2(galley.size().x + 12.0, 18.0),
    );
    p.rect_filled(rect, 9, style::alpha(color, if t().dark { 44 } else { 30 }));
    p.galley(
        pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    rect
}

pub fn progress(p: &Painter, rect: Rect, fraction: f32, color: Color32) {
    let radius = (rect.height() / 2.0).min(255.0) as u8;
    p.rect_filled(rect, radius, t().field);
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        let w = (rect.width() * fraction).max(rect.height());
        p.rect_filled(
            Rect::from_min_size(rect.min, vec2(w, rect.height())),
            radius,
            color,
        );
    }
}

/// Ring segment drawn as a thick polyline (anti-aliased by egui).
pub fn arc(p: &Painter, center: Pos2, radius: f32, width: f32, from: f32, to: f32, color: Color32) {
    if (to - from).abs() < 0.001 {
        return;
    }
    let steps = (((to - from).abs() * radius) / 3.0).clamp(6.0, 160.0) as usize;
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = from + (to - from) * i as f32 / steps as f32;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    p.add(Shape::line(points, Stroke::new(width, color)));
}

pub fn donut(p: &Painter, center: Pos2, radius: f32, width: f32, fraction: f32, color: Color32) {
    arc(p, center, radius, width, 0.0, TAU, t().field);
    let start = -PI / 2.0;
    arc(
        p,
        center,
        radius,
        width,
        start,
        start + TAU * fraction.clamp(0.0, 1.0),
        color,
    );
}

/// A big number with a lighter unit, e.g. "171.6 GB". Returns the drawn width.
pub fn big_bytes(
    p: &Painter,
    pos: Pos2,
    align: Align2,
    bytes: u64,
    size: f32,
    color: Color32,
) -> Rect {
    let (number, unit) = style::bytes_parts(bytes);
    big_value(p, pos, align, &number, unit, size, color)
}

pub fn big_value(
    p: &Painter,
    pos: Pos2,
    align: Align2,
    number: &str,
    unit: &str,
    size: f32,
    color: Color32,
) -> Rect {
    let n = p.layout_no_wrap(number.to_string(), style::semibold(size), color);
    let u = p.layout_no_wrap(
        unit.to_string(),
        style::semibold(size * 0.55),
        style::alpha(color, 140),
    );
    let total = vec2(n.size().x + 4.0 + u.size().x, n.size().y);
    let rect = align.anchor_size(pos, total);
    let baseline_shift = n.size().y - u.size().y - size * 0.12;
    p.galley(rect.min, n.clone(), color);
    p.galley(
        pos2(rect.min.x + n.size().x + 4.0, rect.min.y + baseline_shift),
        u,
        color,
    );
    rect
}

/// Search box with a magnifier and no outline.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let th = t();
    let inner = Frame::new()
        .fill(th.field)
        .corner_radius(10)
        .inner_margin(Margin {
            left: 32,
            right: 10,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.set_height(32.0);
            ui.add(
                egui::TextEdit::singleline(text)
                    .hint_text(egui::RichText::new(hint).color(th.faint))
                    .frame(Frame::NONE)
                    .desired_width(width - 42.0)
                    .align(Align2::LEFT_CENTER)
                    .margin(Margin::symmetric(0, 7)),
            )
        });
    let rect = inner.response.rect;
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(rect.left() + 17.0, rect.center().y), vec2(14.0, 14.0)),
        Icon::Search,
        th.faint,
    );
    inner.inner
}

/// Round checkbox painted at `center`. Returns its rect for hit testing.
pub fn check(p: &Painter, center: Pos2, checked: bool, hovered: bool) -> Rect {
    let th = t();
    let rect = Rect::from_center_size(center, vec2(18.0, 18.0));
    if checked {
        p.rect_filled(rect, 6, th.accent);
        icons::paint(p, rect.shrink(2.0), Icon::Check, th.on_accent);
    } else {
        p.rect_filled(rect, 6, if hovered { th.press } else { th.field });
    }
    rect
}

/// Soft drop shadow under a floating rounded rect.
pub fn shadow(p: &Painter, rect: Rect, radius: u8) {
    p.add(
        egui::epaint::Shadow {
            offset: [0, 8],
            blur: 26,
            spread: 0,
            color: t().shadow,
        }
        .as_shape(rect, CornerRadius::same(radius)),
    );
}

pub fn row_fill(p: &Painter, rect: Rect, hovered: bool, selected: bool) {
    let th = t();
    if selected {
        p.rect_filled(
            rect,
            10,
            style::alpha(th.accent, if th.dark { 46 } else { 30 }),
        );
    } else if hovered {
        p.rect_filled(rect, 10, th.hover);
    }
}

/// Single-line galley truncated with an ellipsis to `width`.
pub fn fit(
    painter: &Painter,
    text: String,
    font: egui::FontId,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(8.0));
    painter.layout_job(job)
}

/// A floating glass panel: soft shadow and the popup surface.
pub fn floating(p: &Painter, rect: Rect, radius: u8) {
    shadow(p, rect, radius);
    p.rect_filled(rect, radius, t().popup);
}

/// Ambient radial glow: opaque-ish center fading to nothing at `radius`.
pub fn glow(p: &Painter, center: Pos2, radius: f32, color: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color);
    let steps = 96;
    for i in 0..=steps {
        let a = i as f32 / steps as f32 * TAU;
        mesh.colored_vertex(
            center + vec2(a.cos(), a.sin()) * radius,
            Color32::TRANSPARENT,
        );
    }
    for i in 1..=steps as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    p.add(Shape::mesh(mesh));
}
