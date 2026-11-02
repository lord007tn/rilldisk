// SPDX-License-Identifier: AGPL-3.0-only
//! Where am I, and how do I get back: the location bar and folder panel shared by
//! finished results and running scans, so both navigate the same way.
use crate::icons::{self, Icon};
use crate::style::{self, t};
use crate::widgets::{self as w, Kind};
use eframe::egui::{
    self, Align, Align2, Color32, Layout, Popup, Rect, Response, RichText, ScrollArea, Sense,
    Stroke, Ui, UiBuilder, pos2, vec2,
};
use std::ops::Range;

/// Entry ids and screen rects of the visible folder panel rows, for scripted tours.
pub const PANEL_HITS: &str = "folder-panel-hits";
/// The same for breadcrumbs.
pub const CRUMB_HITS: &str = "breadcrumb-hits";
/// Screen rect of the Parent button.
pub const PARENT_BUTTON: &str = "parent-button";
/// Hit id of the control that opens the hidden levels.
pub const MORE_LEVELS: usize = usize::MAX;

const ROW: f32 = 34.0;
const TRAIL_ROW: f32 = 30.0;
const CURRENT_ROW: f32 = 48.0;
const INDENT: f32 = 14.0;
const CRUMB_MAX: f32 = 200.0;
const CRUMB_GAP: f32 = 12.0;
const CRUMB_PAD: f32 = 14.0;

/// One level of the current location, scan root first.
#[derive(Clone)]
pub struct Place {
    pub id: usize,
    pub name: String,
    /// Full path, shown on hover.
    pub detail: String,
    /// Empty where the size is not known yet (ancestors during a scan).
    pub size: String,
}

/// A navigation request from the location bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Go {
    Parent,
    To(usize),
}

/// One child of the current folder.
pub struct Row {
    pub id: usize,
    pub name: String,
    pub size: String,
    pub folder: bool,
    pub dot: Color32,
    /// Cleanup safety color, for folders that can be reclaimed.
    pub mark: Option<Color32>,
    /// This folder was not fully read.
    pub partial: bool,
    pub tip: String,
}

/// A warning about the current folder; `action` makes it a link.
pub struct Notice {
    pub text: String,
    pub action: bool,
}

pub struct Panel<'a> {
    /// Scan root to current folder.
    pub places: &'a [Place],
    pub summary: String,
    pub notice: Option<Notice>,
    pub rows: usize,
    pub empty: &'a str,
    pub selected: Option<usize>,
    /// Highlighted from the chart.
    pub hovered: Option<usize>,
    /// Row index to bring into view.
    pub reveal: Option<usize>,
}

#[derive(Default)]
pub struct PanelResult {
    pub go: Option<usize>,
    /// Clicked row: its id and whether it is a folder.
    pub clicked: Option<(usize, bool)>,
    pub hovered: Option<usize>,
    pub notice_clicked: bool,
}

/// A scan root's short name: its last component, or the drive for a drive root.
pub fn root_name(root: &str) -> String {
    let trimmed = root.trim_end_matches(['\\', '/']);
    match trimmed.rsplit(['\\', '/']).next() {
        Some(name) if !name.is_empty() && !name.ends_with(':') => name.to_string(),
        _ => root.to_string(),
    }
}

/// Keep the end of `text`, which tells paths apart, cutting at a folder boundary
/// when one fits.
pub fn elide_start(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= width {
        return text.to_string();
    }
    let with_mark = |from: usize| format!("…{}", &text[from..]);
    let folders = text
        .char_indices()
        .filter(|(_, c)| matches!(c, '\\' | '/'))
        .map(|(i, _)| i)
        .find(|&i| measure(&with_mark(i)) <= width);
    if let Some(from) = folders {
        return with_mark(from);
    }
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let keep = starts.partition_point(|&i| measure(&with_mark(i)) > width);
    starts
        .get(keep)
        .map_or_else(|| "…".to_string(), |&i| with_mark(i))
}

/// Middle crumbs to hide so the rest fits in `available`. The first and last always
/// stay; the levels nearest the current folder are kept in preference.
pub fn hidden_crumbs(widths: &[f32], available: f32, gap: f32, ellipsis: f32) -> Range<usize> {
    let n = widths.len();
    let total = |list: &[f32]| list.iter().sum::<f32>() + gap * list.len() as f32;
    if n < 3 || total(widths) <= available {
        return 1..1;
    }
    let mut used = total(&[widths[0], widths[n - 1]]) + ellipsis + gap;
    let mut first_shown = n - 1;
    while first_shown > 1 && used + widths[first_shown - 1] + gap <= available {
        first_shown -= 1;
        used += widths[first_shown] + gap;
    }
    1..first_shown
}

/// Trail levels to hide in the panel: root, parent and current always stay.
pub fn hidden_trail(levels: usize) -> Range<usize> {
    if levels > 4 { 1..levels - 2 } else { 1..1 }
}

/// Scroll offset that brings a row into view, if it is outside the viewport.
pub fn reveal_offset(index: usize, row: f32, viewport: f32, current: f32) -> Option<f32> {
    let top = index as f32 * row;
    if top < current {
        Some(top)
    } else if top + row > current + viewport {
        Some(top + row - viewport)
    } else {
        None
    }
}

/// The direct child of `folder` that contains `id`, if `id` is below `folder`.
pub fn child_under(
    parent_of: impl Fn(usize) -> Option<usize>,
    id: usize,
    folder: usize,
) -> Option<usize> {
    let mut at = id;
    while let Some(parent) = parent_of(at) {
        if parent == folder {
            return Some(at);
        }
        at = parent;
    }
    None
}

/// Text ending in the part of a path that identifies it, fitted to `width`.
pub fn fit_path(ui: &Ui, path: &str, font: egui::FontId, width: f32) -> String {
    let painter = ui.painter();
    elide_start(path, width, |text| {
        painter
            .layout_no_wrap(text.to_string(), font.clone(), Color32::PLACEHOLDER)
            .size()
            .x
    })
}

/// Parent button, breadcrumbs and a status text on one line of their own.
pub fn location_bar(ui: &mut Ui, places: &[Place], can_go_up: bool, status: &str) -> Option<Go> {
    let mut go = None;
    let th = t();
    ui.horizontal(|ui| {
        ui.set_height(34.0);
        ui.spacing_mut().item_spacing.x = 0.0;
        let parent = places.iter().rev().nth(1);
        let button = ui
            .add_enabled_ui(can_go_up, |ui| {
                w::button(ui, Kind::Soft, Some(Icon::Back), "Parent")
            })
            .inner;
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::unique(PARENT_BUTTON), button.rect));
        let tip = parent.map_or_else(String::new, |p| format!("Up to {} (Backspace)", p.name));
        if button
            .on_hover_text(tip)
            .on_disabled_hover_text(if places.len() > 1 {
                "Opening a folder…"
            } else {
                "This is the top of the scan"
            })
            .clicked()
        {
            go = Some(Go::Parent);
        }
        ui.add_space(8.0);
        let status = ui.painter().layout_no_wrap(
            status.to_string(),
            style::regular(13.0),
            Color32::PLACEHOLDER,
        );
        let room = (ui.available_width() - status.size().x - 16.0).max(80.0);
        if let Some(id) = breadcrumbs(ui, places, room) {
            go = Some(Go::To(id));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (rect, _) = ui.allocate_exact_size(status.size(), Sense::hover());
            ui.painter().galley(rect.min, status, th.dim);
        });
    });
    go
}

fn breadcrumbs(ui: &mut Ui, places: &[Place], room: f32) -> Option<usize> {
    let th = t();
    let mut go = None;
    let last = places.len().checked_sub(1)?;
    let font = |i: usize| {
        if i == last {
            style::semibold(14.0)
        } else {
            style::regular(14.0)
        }
    };
    let widths: Vec<f32> = places
        .iter()
        .enumerate()
        .map(|(i, place)| {
            let text =
                ui.painter()
                    .layout_no_wrap(place.name.clone(), font(i), Color32::PLACEHOLDER);
            text.size().x.min(CRUMB_MAX) + CRUMB_PAD
        })
        .collect();
    let hidden = hidden_crumbs(&widths, room, CRUMB_GAP, 24.0);
    // When even the first, the last and "…" do not fit, the last one gives way.
    let others: f32 = widths[..last.min(1)].iter().sum::<f32>()
        + widths[hidden.end.min(last)..last].iter().sum::<f32>()
        + CRUMB_GAP * last as f32
        + if hidden.is_empty() { 0.0 } else { 24.0 };
    let last_room = (room - others).clamp(60.0, CRUMB_MAX + CRUMB_PAD);
    let mut hits = Vec::with_capacity(places.len());
    for (i, place) in places.iter().enumerate() {
        if hidden.contains(&i) {
            if i == hidden.start {
                separator(ui);
                ui.scope(|ui| {
                    ui.spacing_mut().button_padding = vec2(6.0, 4.0);
                    let menu = ui.menu_button(RichText::new("…").color(th.dim), |ui| {
                        ui.set_max_width(320.0);
                        ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            for level in &places[hidden.clone()] {
                                let button = egui::Button::new(level.name.as_str()).truncate();
                                let item = ui.add(button).on_hover_text(&level.detail);
                                hits.push((level.id, item.rect));
                                if item.clicked() {
                                    go = Some(level.id);
                                    ui.close();
                                }
                            }
                        });
                    });
                    hits.push((MORE_LEVELS, menu.response.rect));
                    menu.response
                        .on_hover_text(format!("{} more levels", hidden.len()));
                });
            }
            continue;
        }
        if i > 0 {
            separator(ui);
        }
        let current = i == last;
        let width = if current {
            last_room
        } else {
            CRUMB_MAX + CRUMB_PAD
        };
        let color = if current { th.text } else { th.dim };
        let galley = w::fit(
            ui.painter(),
            place.name.clone(),
            font(i),
            color,
            width - CRUMB_PAD,
        );
        let sense = if current {
            Sense::hover()
        } else {
            Sense::click()
        };
        let (rect, response) =
            ui.allocate_exact_size(vec2(galley.size().x + CRUMB_PAD, 30.0), sense);
        if response.hovered() && !current {
            ui.painter().rect_filled(rect, 8, th.hover);
        }
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &place.name));
        ui.painter().galley(
            pos2(
                rect.left() + CRUMB_PAD / 2.0,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            color,
        );
        hits.push((place.id, rect));
        if response.on_hover_text(&place.detail).clicked() {
            go = Some(place.id);
        }
    }
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::unique(CRUMB_HITS), hits));
    go
}

fn separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(CRUMB_GAP, 30.0), Sense::hover());
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(10.0, 10.0)),
        Icon::Chevron,
        t().faint,
    );
}

/// The trail from the scan root to the current folder, with the folder's children
/// below it. `row` builds only the rows in view, so huge folders stay cheap;
/// `on_row` lets the caller attach a context menu.
pub fn panel(
    ui: &mut Ui,
    rect: Rect,
    spec: Panel<'_>,
    row: impl Fn(usize) -> Row,
    mut on_row: impl FnMut(&Response, &Row),
) -> PanelResult {
    let th = t();
    let mut result = PanelResult::default();
    let mut ui = ui.new_child(UiBuilder::new().max_rect(rect));
    ui.spacing_mut().item_spacing.y = 0.0;
    let levels = spec.places.len();
    let hidden = hidden_trail(levels);
    let mut hits = Vec::new();
    let mut depth = 0.0;
    for (i, place) in spec.places.iter().enumerate() {
        if hidden.contains(&i) {
            if i == hidden.start {
                let more = format!("{} more levels", hidden.len());
                let response = trail_row(&mut ui, depth, Icon::Folder, &more, "", true);
                hits.push((MORE_LEVELS, response.rect));
                Popup::menu(&response).show(|ui| {
                    ui.set_max_width(300.0);
                    ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for level in &spec.places[hidden.clone()] {
                            let button = egui::Button::new(level.name.as_str()).truncate();
                            let item = ui.add(button).on_hover_text(&level.detail);
                            hits.push((level.id, item.rect));
                            if item.clicked() {
                                result.go = Some(level.id);
                                ui.close();
                            }
                        }
                    });
                });
                depth += 1.0;
            }
            continue;
        }
        if i + 1 == levels {
            current_row(&mut ui, depth, place, &spec.summary);
        } else {
            let icon = if i == 0 && place.name.ends_with(['\\', '/']) {
                Icon::Drive
            } else {
                Icon::Folder
            };
            let response = trail_row(&mut ui, depth, icon, &place.name, &place.size, false);
            hits.push((place.id, response.rect));
            if response.on_hover_text(&place.detail).clicked() {
                result.go = Some(place.id);
            }
            depth += 1.0;
        }
    }
    if let Some(notice) = &spec.notice {
        let color = if notice.action { th.accent } else { th.warn };
        let (space, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), 24.0),
            if notice.action {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let galley = w::fit(
            ui.painter(),
            notice.text.clone(),
            style::regular(12.0),
            color,
            space.width() - 16.0,
        );
        ui.painter().galley(
            pos2(space.left() + 8.0, space.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        result.notice_clicked = response.on_hover_text(&notice.text).clicked();
    }
    ui.add_space(2.0);
    let indent = 8.0 + (depth + 1.0) * INDENT;
    let list = ui.available_rect_before_wrap();
    let folder = spec.places.last().map_or(0, |p| p.id);
    let offset_id = egui::Id::unique(("folder-panel-offset", folder));
    let mut area = ScrollArea::vertical()
        .id_salt(("folder-panel", folder))
        .auto_shrink(false);
    if let Some(index) = spec.reveal {
        let current = ui.data(|d| d.get_temp::<f32>(offset_id)).unwrap_or(0.0);
        if let Some(offset) = reveal_offset(index, ROW, list.height(), current) {
            area = area.vertical_scroll_offset(offset);
        }
    }
    let output = area.show_rows(&mut ui, ROW, spec.rows, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for index in range {
            let entry = row(index);
            let response = child_row(
                ui,
                &entry,
                indent,
                spec.selected == Some(entry.id),
                spec.hovered == Some(entry.id),
            );
            if response.hovered() {
                result.hovered = Some(entry.id);
            }
            if ui.clip_rect().contains(response.rect.center()) {
                hits.push((entry.id, response.rect));
            }
            if response.clicked() {
                result.clicked = Some((entry.id, entry.folder));
            }
            on_row(&response, &entry);
        }
    });
    ui.data_mut(|d| {
        d.insert_temp(offset_id, output.state.clamped_offset().y);
        d.insert_temp(egui::Id::unique(PANEL_HITS), hits);
    });
    if spec.rows == 0 {
        ui.painter().text(
            list.center_top() + vec2(0.0, 26.0),
            Align2::CENTER_TOP,
            spec.empty,
            style::regular(13.0),
            th.faint,
        );
    }
    result
}

fn elbow(ui: &Ui, rect: Rect, x: f32) {
    let line = Stroke::new(1.0, style::alpha(t().faint, 110));
    let y = rect.center().y;
    ui.painter()
        .line_segment([pos2(x, rect.top()), pos2(x, y)], line);
    ui.painter()
        .line_segment([pos2(x, y), pos2(x + 7.0, y)], line);
}

fn trail_row(
    ui: &mut Ui,
    depth: f32,
    icon: Icon,
    name: &str,
    size: &str,
    collapsed: bool,
) -> Response {
    let th = t();
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), TRAIL_ROW), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    let p = ui.painter();
    w::row_fill(p, rect.shrink2(vec2(2.0, 1.0)), response.hovered(), false);
    let left = rect.left() + 8.0 + depth * INDENT;
    if depth > 0.0 {
        elbow(ui, rect, left - INDENT + 7.0);
    }
    let tile = Rect::from_center_size(pos2(left + 8.0, rect.center().y), vec2(15.0, 15.0));
    icons::paint(p, tile, icon, th.dim);
    let size = p.layout_no_wrap(size.to_string(), style::regular(12.0), th.dim);
    let right = rect.right() - 12.0;
    p.galley(
        pos2(right - size.size().x, rect.center().y - size.size().y / 2.0),
        size.clone(),
        th.dim,
    );
    let color = if collapsed { th.faint } else { th.dim };
    let name = w::fit(
        p,
        name.to_string(),
        style::regular(13.0),
        color,
        right - size.size().x - tile.right() - 16.0,
    );
    p.galley(
        pos2(tile.right() + 8.0, rect.center().y - name.size().y / 2.0),
        name,
        color,
    );
    response
}

/// The folder being shown: unmistakable, and not a button (it is already open).
fn current_row(ui: &mut Ui, depth: f32, place: &Place, summary: &str) {
    let th = t();
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), CURRENT_ROW), Sense::hover());
    let p = ui.painter();
    let fill = rect.shrink2(vec2(2.0, 1.0));
    p.rect_filled(
        fill,
        10,
        style::alpha(th.accent, if th.dark { 46 } else { 30 }),
    );
    p.rect_filled(
        Rect::from_min_size(
            fill.left_top() + vec2(0.0, 9.0),
            vec2(3.0, fill.height() - 18.0),
        ),
        2,
        th.accent,
    );
    let left = rect.left() + 8.0 + depth * INDENT;
    if depth > 0.0 {
        elbow(ui, rect, left - INDENT + 7.0);
    }
    icons::paint(
        p,
        Rect::from_center_size(pos2(left + 8.0, rect.top() + 16.0), vec2(16.0, 16.0)),
        Icon::Folder,
        th.accent,
    );
    let right = rect.right() - 12.0;
    let size = p.layout_no_wrap(place.size.clone(), style::semibold(14.0), th.text);
    p.galley(
        pos2(right - size.size().x, rect.top() + 7.0),
        size.clone(),
        th.text,
    );
    let text_left = left + 24.0;
    let name = w::fit(
        p,
        place.name.clone(),
        style::semibold(14.0),
        th.text,
        right - size.size().x - text_left - 8.0,
    );
    p.galley(pos2(text_left, rect.top() + 6.0), name, th.text);
    let line = w::fit(
        p,
        summary.to_string(),
        style::regular(11.5),
        th.dim,
        right - text_left,
    );
    p.galley(pos2(text_left, rect.top() + 26.0), line, th.dim);
    response.on_hover_text(&place.detail);
}

fn child_row(ui: &mut Ui, row: &Row, indent: f32, selected: bool, highlighted: bool) -> Response {
    let th = t();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    // Live views reorder rows as sizes grow: identity follows the folder.
    let response = ui.interact(rect, ui.scope_id().with(("row", row.id)), Sense::click());
    response.widget_info(|| {
        let label = format!("{}, {}", row.name, row.size);
        egui::WidgetInfo::labeled(egui::Role::Button, true, label)
    });
    let p = ui.painter();
    w::row_fill(
        p,
        rect.shrink2(vec2(2.0, 1.0)),
        response.hovered() || highlighted,
        selected,
    );
    let guide = rect.left() + indent - INDENT + 7.0;
    p.line_segment(
        [pos2(guide, rect.top()), pos2(guide, rect.bottom())],
        Stroke::new(1.0, style::alpha(th.faint, 70)),
    );
    let dot = pos2(rect.left() + indent + 8.0, rect.center().y);
    if row.folder {
        p.circle_filled(dot, 5.0, row.dot);
    } else {
        p.circle_stroke(dot, 4.0, Stroke::new(2.0, row.dot));
    }
    let mut right = rect.right() - 10.0;
    if row.folder {
        let chevron = Rect::from_center_size(pos2(right - 5.0, rect.center().y), vec2(10.0, 10.0));
        icons::paint(p, chevron, Icon::Chevron, th.faint);
    }
    right -= 18.0;
    let size = p.layout_no_wrap(row.size.clone(), style::semibold(12.5), th.text);
    p.galley(
        pos2(right - size.size().x, rect.center().y - size.size().y / 2.0),
        size.clone(),
        th.text,
    );
    right -= size.size().x + 10.0;
    if row.partial {
        let badge = p.layout_no_wrap("partial".to_string(), style::regular(11.0), th.warn);
        p.galley(
            pos2(
                right - badge.size().x,
                rect.center().y - badge.size().y / 2.0,
            ),
            badge.clone(),
            th.warn,
        );
        right -= badge.size().x + 8.0;
    }
    if let Some(color) = row.mark {
        p.circle_filled(pos2(right - 3.0, rect.center().y), 3.0, color);
        right -= 14.0;
    }
    let left = dot.x + 14.0;
    let name = w::fit(
        p,
        row.name.clone(),
        style::regular(13.0),
        th.text,
        right - left,
    );
    p.galley(
        pos2(left, rect.center().y - name.size().y / 2.0),
        name,
        th.text,
    );
    response.on_hover_text(&row.tip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> f32 {
        text.chars().count() as f32
    }

    #[test]
    fn root_names_are_short_but_drives_stay_whole() {
        assert_eq!(root_name(r"C:\RillDemo\home"), "home");
        assert_eq!(root_name(r"C:\RillDemo\home\"), "home");
        assert_eq!(root_name(r"C:\"), r"C:\");
        assert_eq!(root_name("/data/home"), "home");
        assert_eq!(root_name("/"), "/");
    }

    #[test]
    fn long_paths_keep_their_end() {
        let path = r"C:\Volumes\Team\Projects\atlas-web";
        assert_eq!(elide_start(path, 100.0, chars), path);
        assert_eq!(elide_start(path, 20.0, chars), r"…\Projects\atlas-web");
        assert_eq!(elide_start(path, 11.0, chars), r"…\atlas-web");
        // No separator fits: cut inside the name, keeping as much as possible.
        assert_eq!(elide_start(path, 6.0, chars), "…s-web");
        assert_eq!(elide_start("نصوص-طويلة", 4.0, chars).chars().count(), 4);
    }

    #[test]
    fn breadcrumbs_keep_the_ends_and_the_levels_nearest_the_current_folder() {
        let widths = [60.0; 8];
        // Everything fits.
        assert!(hidden_crumbs(&widths, 576.0, 12.0, 24.0).is_empty());
        // Root, "…" and the last three.
        assert_eq!(hidden_crumbs(&widths, 330.0, 12.0, 24.0), 1..5);
        // Far too small: only the first and the last remain.
        assert_eq!(hidden_crumbs(&widths, 100.0, 12.0, 24.0), 1..7);
        assert!(hidden_crumbs(&[900.0, 900.0], 100.0, 12.0, 24.0).is_empty());
    }

    #[test]
    fn trail_keeps_root_parent_and_current() {
        assert!(hidden_trail(1).is_empty());
        assert!(hidden_trail(4).is_empty());
        assert_eq!(hidden_trail(5), 1..3);
        assert_eq!(hidden_trail(9), 1..7);
    }

    #[test]
    fn a_row_is_scrolled_only_when_it_is_out_of_view() {
        assert_eq!(reveal_offset(0, 34.0, 340.0, 0.0), None);
        assert_eq!(reveal_offset(9, 34.0, 340.0, 0.0), None);
        assert_eq!(reveal_offset(10, 34.0, 340.0, 0.0), Some(34.0));
        assert_eq!(reveal_offset(2, 34.0, 340.0, 170.0), Some(68.0));
    }

    #[test]
    fn hover_inside_a_nested_tile_maps_to_the_listed_child() {
        // 0 > 1 > 2 > 3
        let parents = |id: usize| id.checked_sub(1);
        assert_eq!(child_under(parents, 3, 0), Some(1));
        assert_eq!(child_under(parents, 3, 2), Some(3));
        assert_eq!(child_under(parents, 1, 1), None);
        assert_eq!(child_under(parents, 1, 3), None);
    }
}
