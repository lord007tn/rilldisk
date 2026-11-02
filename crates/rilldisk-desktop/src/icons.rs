// SPDX-License-Identifier: AGPL-3.0-only
//! Small line icons drawn with the painter, so no icon font is needed.
use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, vec2};
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Drive,
    Folder,
    Home,
    Download,
    Document,
    Desktop,
    Code,
    Search,
    Refresh,
    Back,
    Chevron,
    Sun,
    Moon,
    Sparkle,
    Copy,
    Check,
    Grid,
    Rings,
    Warning,
    Agent,
    Stop,
    Eye,
    Terminal,
    Gear,
    Trash,
}

fn arc(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let steps = ((to - from).abs() * radius / 2.0).clamp(8.0, 64.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = from + (to - from) * i as f32 / steps as f32;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect()
}

/// Draws `icon` centered in `rect` (designed on a 16-point grid).
pub fn paint(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height()) / 16.0;
    let o = rect.center() - vec2(8.0, 8.0) * s;
    let at = |x: f32, y: f32| o + vec2(x, y) * s;
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(at(x0, y0), at(x1, y1));
    let line = Stroke::new(1.5 * s.max(0.8), color);
    let poly = |points: Vec<Pos2>| Shape::line(points, line);
    match icon {
        Icon::Drive => {
            p.rect_stroke(
                r(1.5, 4.0, 14.5, 12.0),
                2.0 * s,
                line,
                eframe::egui::StrokeKind::Middle,
            );
            p.line_segment([at(1.5, 9.0), at(14.5, 9.0)], line);
            p.circle_filled(at(12.0, 10.6), 0.9 * s, color);
        }
        Icon::Folder => {
            p.add(poly(vec![
                at(1.5, 12.5),
                at(1.5, 3.5),
                at(6.0, 3.5),
                at(7.5, 5.0),
                at(14.5, 5.0),
                at(14.5, 12.5),
                at(1.5, 12.5),
            ]));
        }
        Icon::Home => {
            p.add(poly(vec![at(2.0, 7.5), at(8.0, 2.5), at(14.0, 7.5)]));
            p.add(poly(vec![
                at(3.5, 6.5),
                at(3.5, 13.5),
                at(12.5, 13.5),
                at(12.5, 6.5),
            ]));
        }
        Icon::Download => {
            p.line_segment([at(8.0, 2.0), at(8.0, 10.0)], line);
            p.add(poly(vec![at(4.5, 6.8), at(8.0, 10.2), at(11.5, 6.8)]));
            p.add(poly(vec![
                at(2.5, 11.0),
                at(2.5, 13.5),
                at(13.5, 13.5),
                at(13.5, 11.0),
            ]));
        }
        Icon::Document => {
            p.add(poly(vec![
                at(3.5, 1.8),
                at(9.5, 1.8),
                at(12.5, 4.8),
                at(12.5, 14.2),
                at(3.5, 14.2),
                at(3.5, 1.8),
            ]));
            p.line_segment([at(5.8, 8.0), at(10.2, 8.0)], line);
            p.line_segment([at(5.8, 11.0), at(10.2, 11.0)], line);
        }
        Icon::Desktop => {
            p.rect_stroke(
                r(1.5, 2.5, 14.5, 11.0),
                1.5 * s,
                line,
                eframe::egui::StrokeKind::Middle,
            );
            p.line_segment([at(8.0, 11.0), at(8.0, 13.5)], line);
            p.line_segment([at(5.0, 13.8), at(11.0, 13.8)], line);
        }
        Icon::Code => {
            p.add(poly(vec![at(5.5, 4.0), at(1.8, 8.0), at(5.5, 12.0)]));
            p.add(poly(vec![at(10.5, 4.0), at(14.2, 8.0), at(10.5, 12.0)]));
        }
        Icon::Search => {
            p.circle_stroke(at(7.0, 7.0), 4.5 * s, line);
            p.line_segment([at(10.4, 10.4), at(14.0, 14.0)], line);
        }
        Icon::Refresh => {
            p.add(poly(arc(at(8.0, 8.0), 5.5 * s, -PI * 0.15, PI * 1.55)));
            let a = -PI * 0.15;
            let tip = at(8.0, 8.0) + vec2(a.cos(), a.sin()) * 5.5 * s;
            p.add(Shape::convex_polygon(
                vec![
                    tip + vec2(-3.2, -2.6) * s,
                    tip + vec2(2.2, -3.6) * s,
                    tip + vec2(1.4, 1.8) * s,
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Back => {
            p.add(poly(vec![at(10.0, 3.0), at(5.0, 8.0), at(10.0, 13.0)]));
        }
        Icon::Chevron => {
            p.add(poly(vec![at(6.0, 3.5), at(10.5, 8.0), at(6.0, 12.5)]));
        }
        Icon::Sun => {
            p.circle_stroke(at(8.0, 8.0), 3.0 * s, line);
            for i in 0..8 {
                let a = i as f32 * TAU / 8.0;
                let d = vec2(a.cos(), a.sin());
                p.line_segment(
                    [at(8.0, 8.0) + d * 5.2 * s, at(8.0, 8.0) + d * 6.8 * s],
                    line,
                );
            }
        }
        Icon::Moon => {
            let mut points = arc(at(8.0, 8.0), 5.8 * s, PI * 0.30, PI * 1.70);
            let mut inner = arc(at(10.2, 6.4), 4.4 * s, PI * 1.62, PI * 0.42);
            points.append(&mut inner);
            points.push(points[0]);
            p.add(Shape::line(points, line));
        }
        Icon::Gear => {
            let c = at(8.0, 8.0);
            p.circle_stroke(c, 2.2 * s, line);
            // Eight rounded teeth around a ring.
            let mut ring = Vec::new();
            for i in 0..=64 {
                let a = i as f32 * TAU / 64.0;
                let tooth = ((a * 8.0).cos() * 0.5 + 0.5).powf(3.0);
                let rad = 4.9 + 1.5 * tooth;
                ring.push(c + vec2(a.cos(), a.sin()) * rad * s);
            }
            p.add(Shape::line(ring, line));
        }
        Icon::Sparkle => {
            let c = at(7.0, 8.5);
            let star = |c: Pos2, big: f32, small: f32| {
                let mut pts = Vec::new();
                for i in 0..8 {
                    let a = i as f32 * TAU / 8.0 - PI / 2.0;
                    let rad = if i % 2 == 0 { big } else { small };
                    pts.push(c + vec2(a.cos(), a.sin()) * rad * s);
                }
                pts
            };
            p.add(Shape::convex_polygon(
                star(c, 6.0, 1.6),
                color,
                Stroke::NONE,
            ));
            p.add(Shape::convex_polygon(
                star(at(13.0, 3.5), 2.6, 0.8),
                color,
                Stroke::NONE,
            ));
        }
        Icon::Copy => {
            p.rect_stroke(
                r(5.0, 5.0, 14.0, 14.0),
                2.0 * s,
                line,
                eframe::egui::StrokeKind::Middle,
            );
            p.add(poly(vec![
                at(3.0, 11.0),
                at(2.0, 11.0),
                at(2.0, 2.0),
                at(11.0, 2.0),
                at(11.0, 3.0),
            ]));
        }
        Icon::Check => {
            p.add(poly(vec![at(3.0, 8.5), at(6.5, 12.0), at(13.0, 4.5)]));
        }
        Icon::Grid => {
            for rect in [
                r(2.0, 2.0, 9.0, 14.0),
                r(10.0, 2.0, 14.0, 8.0),
                r(10.0, 9.0, 14.0, 14.0),
            ] {
                p.rect_stroke(rect, 1.5 * s, line, eframe::egui::StrokeKind::Middle);
            }
        }
        Icon::Rings => {
            p.circle_stroke(at(8.0, 8.0), 2.2 * s, line);
            p.add(poly(arc(at(8.0, 8.0), 5.6 * s, -PI * 0.5, PI * 0.9)));
            p.add(poly(arc(at(8.0, 8.0), 5.6 * s, PI * 1.05, PI * 1.35)));
        }
        Icon::Warning => {
            p.add(poly(vec![
                at(8.0, 2.0),
                at(14.5, 13.5),
                at(1.5, 13.5),
                at(8.0, 2.0),
            ]));
            p.line_segment([at(8.0, 6.5), at(8.0, 9.5)], line);
            p.circle_filled(at(8.0, 11.6), 0.9 * s, color);
        }
        Icon::Agent => {
            p.rect_stroke(
                r(2.5, 5.0, 13.5, 13.5),
                3.0 * s,
                line,
                eframe::egui::StrokeKind::Middle,
            );
            p.line_segment([at(8.0, 5.0), at(8.0, 2.5)], line);
            p.circle_filled(at(8.0, 2.2), 1.0 * s, color);
            p.circle_filled(at(6.0, 9.0), 1.1 * s, color);
            p.circle_filled(at(10.0, 9.0), 1.1 * s, color);
        }
        Icon::Stop => {
            p.rect_filled(r(4.0, 4.0, 12.0, 12.0), 2.0 * s, color);
        }
        Icon::Eye => {
            let mut top = arc(at(8.0, 13.0), 8.0 * s, -PI * 0.82, -PI * 0.18);
            let mut bottom = arc(at(8.0, 3.0), 8.0 * s, PI * 0.18, PI * 0.82);
            top.append(&mut bottom);
            top.push(top[0]);
            p.add(Shape::line(top, line));
            p.circle_filled(at(8.0, 8.0), 2.0 * s, color);
        }
        Icon::Terminal => {
            p.rect_stroke(
                r(1.5, 2.5, 14.5, 13.5),
                2.0 * s,
                line,
                eframe::egui::StrokeKind::Middle,
            );
            p.add(poly(vec![at(4.5, 6.0), at(6.8, 8.0), at(4.5, 10.0)]));
            p.line_segment([at(8.5, 10.2), at(11.5, 10.2)], line);
        }
        Icon::Trash => {
            p.line_segment([at(2.0, 4.0), at(14.0, 4.0)], line);
            p.add(poly(vec![
                at(6.0, 4.0),
                at(6.0, 2.0),
                at(10.0, 2.0),
                at(10.0, 4.0),
            ]));
            p.add(poly(vec![
                at(3.5, 4.0),
                at(4.5, 14.0),
                at(11.5, 14.0),
                at(12.5, 4.0),
            ]));
            p.line_segment([at(6.7, 6.8), at(6.9, 11.4)], line);
            p.line_segment([at(9.3, 6.8), at(9.1, 11.4)], line);
        }
    }
}
