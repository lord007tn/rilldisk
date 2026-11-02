// SPDX-License-Identifier: AGPL-3.0-only
//! Reading protected folders: the "Administrator" badge, the restart offer and the
//! Issues view, grouped by kind. See docs/proposals/elevation.md for the scope.
//!
//! Nothing here elevates on its own. Every route starts from a click and goes
//! through the OS's own consent UI (UAC, polkit) or settings page (macOS Full Disk
//! Access). The current window closes only after Windows reports that the elevated
//! instance started.
use crate::icons::{self, Icon};
use crate::style::{self, t};
use crate::widgets::{self as w, Kind};
use eframe::egui::{self, Frame, Margin, Rect, RichText, Sense, Ui, pos2, vec2};
use rilldisk_core::{Elevation, IssueKind, ScanIssue, Summary};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Kinds in the order the Issues tab shows them: the actionable one first.
const ORDER: [IssueKind; 6] = [
    IssueKind::AccessDenied,
    IssueKind::InUse,
    IssueKind::NotFound,
    IssueKind::Stalled,
    IssueKind::EntryLimit,
    IssueKind::Other,
];

/// What the app can offer for folders that were refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    None,
    /// Windows UAC, or Linux `pkexec`: restart this app elevated on the same folder.
    Restart,
    /// macOS privacy protection: open the Full Disk Access settings page.
    FullDiskAccess,
    /// No graphical route here; a command the user can copy into a terminal.
    Command(String),
}

/// The result of the last attempt: a few words for the header, the full text in
/// the Issues callout and Settings.
struct Note {
    short: &'static str,
    text: String,
}

impl Note {
    fn new(short: &'static str, text: impl Into<String>) -> Self {
        Self {
            short,
            text: text.into(),
        }
    }

    fn declined() -> Self {
        Self::new(
            "Declined",
            "Administrator access was declined. Nothing changed.",
        )
    }
}

pub struct Elevate {
    state: Elevation,
    /// `pkexec` is installed and runnable (Unix). Checked once, not every frame.
    pkexec: bool,
    note: Option<Note>,
    /// Linux: the `pkexec` launch, watched so a declined prompt can be reported.
    #[cfg(not(windows))]
    child: Option<std::process::Child>,
    /// Scripted captures: show the elevated look without elevating.
    pub preview_elevated: bool,
}

impl Elevate {
    /// Enables the backup privilege when the app already runs elevated (Windows),
    /// then records the privilege state.
    pub fn new() -> Self {
        rilldisk_core::enable_backup_privilege();
        Self {
            state: rilldisk_core::elevation(),
            pkexec: cfg!(unix) && rilldisk_core::executable_on_path("pkexec"),
            note: None,
            #[cfg(not(windows))]
            child: None,
            preview_elevated: false,
        }
    }

    fn elevated(&self) -> bool {
        self.state.is_elevated || self.preview_elevated
    }

    /// A `pkexec` prompt, or the elevated app it started, is still running.
    #[cfg(not(windows))]
    fn pending(&self) -> bool {
        self.child.is_some()
    }

    /// Windows waits for UAC synchronously, so nothing is ever pending.
    #[cfg(windows)]
    fn pending(&self) -> bool {
        false
    }

    pub fn offer(&self, summary: &Summary) -> Offer {
        offer(
            summary,
            self.elevated(),
            self.state.can_elevate,
            self.pkexec,
            t().dark,
        )
    }

    /// Header, next to the scan status chip (right-to-left layout): an
    /// "Administrator" badge when elevated, otherwise a compact restart button when
    /// protected folders were skipped. `scanning` hides the restart, which would end
    /// the scan. `show_note` adds the last attempt's result in a few words; the
    /// Issues tab shows it in full instead.
    pub fn header(&mut self, ui: &mut Ui, summary: &Summary, scanning: bool, show_note: bool) {
        self.poll(ui.ctx());
        if self.elevated() {
            badge(ui).on_hover_text(self.badge_tip());
            return;
        }
        if !scanning && self.offer(summary) == Offer::Restart {
            let folders = style::plural(summary.access_denied_count, "protected folder");
            let root = summary.root.clone();
            ui.add_enabled_ui(!self.pending(), |ui| {
                let clicked = w::button(ui, Kind::Soft, Some(Icon::Shield), restart_label())
                    .on_hover_text(format!(
                        "{folders} could not be read. Restart RillDisk with administrator rights and rescan {}.",
                        root.display()
                    ))
                    .on_disabled_hover_text("Waiting for the administrator prompt.")
                    .clicked();
                if clicked {
                    self.restart(ui.ctx(), Some(&root));
                }
            });
        }
        if show_note && let Some(note) = &self.note {
            ui.label(RichText::new(note.short).color(t().faint).size(11.5))
                .on_hover_text(&note.text);
        }
    }

    fn badge_tip(&self) -> &'static str {
        if self.state.backup_privilege_enabled || self.preview_elevated {
            "Running as administrator. Protected folders are listed with the backup privilege (read-only)."
        } else {
            "Running with administrator rights."
        }
    }

    /// The Issues tab: the offer on top, then issues grouped by kind. While
    /// `scanning`, the restart is shown but disabled, since it would end the scan.
    pub fn issues(&mut self, ui: &mut Ui, summary: &Summary, scanning: bool) {
        let th = t();
        self.poll(ui.ctx());
        let offer = self.offer(summary);
        if offer != Offer::None || self.elevated() {
            self.callout(ui, summary, &offer, scanning);
            ui.add_space(12.0);
        }
        let listed = summary.issues.len() as u64;
        ui.label(
            RichText::new(format!(
                "{} issues: folders that could not be read (left out of the totals) or checks that could not be made. {} are listed.",
                style::count(summary.issue_count),
                if listed == summary.issue_count {
                    "All".to_string()
                } else {
                    format!("The first {}", style::count(listed))
                }
            ))
            .color(th.dim),
        );
        ui.add_space(8.0);
        let groups = group(&summary.issues);
        egui::ScrollArea::vertical()
            .id_salt("issues")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (kind, issues) in groups {
                    // Access denied is counted over every issue, not only the listed ones.
                    let total = if kind == IssueKind::AccessDenied {
                        summary.access_denied_count
                    } else {
                        issues.len() as u64
                    };
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(kind_title(kind)).font(style::semibold(13.5)));
                        w::chip(ui, &style::count(total), kind_color(kind));
                        if (issues.len() as u64) < total {
                            ui.label(
                                RichText::new(format!(
                                    "{} listed",
                                    style::count(issues.len() as u64)
                                ))
                                .color(th.faint)
                                .size(11.5),
                            );
                        }
                    });
                    ui.label(RichText::new(kind_help(kind)).color(th.faint).size(11.5));
                    ui.add_space(6.0);
                    for issue in issues {
                        row(ui, issue, kind);
                    }
                }
            });
    }

    fn callout(&mut self, ui: &mut Ui, summary: &Summary, offer: &Offer, scanning: bool) {
        let th = t();
        let tint = if self.elevated() { th.good } else { th.accent };
        Frame::new()
            .fill(style::alpha(tint, if th.dark { 30 } else { 22 }))
            .corner_radius(14)
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (tile, _) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::hover());
                    ui.painter().rect_filled(tile, 11, style::alpha(tint, 48));
                    icons::paint(ui.painter(), tile.shrink(10.0), Icon::Shield, tint);
                    ui.add_space(8.0);
                    // Leave room for the action button on the right.
                    let button = if *offer == Offer::None { 0.0 } else { 270.0 };
                    let text_width = (ui.available_width() - button).max(200.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(text_width);
                        let (title, body) = callout_text(
                            offer,
                            summary.access_denied_count,
                            summary.privacy_denied_count,
                            self.elevated(),
                        );
                        ui.label(RichText::new(title).font(style::semibold(15.0)));
                        ui.label(RichText::new(body).color(th.dim));
                        if let Offer::Command(command) = offer {
                            ui.label(RichText::new(command).font(style::mono(12.0)));
                        }
                        if let Some(note) = &self.note {
                            ui.add_space(2.0);
                            ui.label(RichText::new(&note.text).color(th.faint).size(11.5));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.action(ui, offer, Some(&summary.root), scanning);
                    });
                });
            });
    }

    /// The button for `offer`. A restart is disabled while a scan runs or a prompt
    /// is pending.
    fn action(&mut self, ui: &mut Ui, offer: &Offer, root: Option<&Path>, scanning: bool) {
        match offer {
            Offer::Restart => {
                let pending = self.pending();
                ui.add_enabled_ui(!scanning && !pending, |ui| {
                    let clicked = w::button(ui, Kind::Primary, Some(Icon::Shield), restart_label())
                        .on_disabled_hover_text(if pending {
                            "Waiting for the administrator prompt."
                        } else {
                            "Available when the scan finishes."
                        })
                        .clicked();
                    if clicked {
                        self.restart(ui.ctx(), root);
                    }
                });
            }
            Offer::FullDiskAccess => {
                if w::button(ui, Kind::Primary, None, "Grant Full Disk Access").clicked() {
                    self.note = Some(open_full_disk_access());
                }
            }
            Offer::Command(command) => {
                if w::button(ui, Kind::Soft, Some(Icon::Copy), "Copy command")
                    .on_hover_text(command)
                    .clicked()
                {
                    ui.ctx().copy_text(command.clone());
                    self.note = Some(Note::new(
                        "Copied",
                        "Copied. Run it in a terminal, then close this window.",
                    ));
                }
            }
            Offer::None => {}
        }
    }

    /// Settings > Privacy: the privilege state in words.
    pub fn status(&self) -> String {
        if self.elevated() {
            return self.badge_tip().into();
        }
        if cfg!(windows) {
            "Not running as administrator. Protected system folders are skipped and listed under Issues. Restart as administrator to read them; RillDisk only lists them and never changes permissions or files.".into()
        } else if cfg!(target_os = "macos") {
            "Folders protected by macOS privacy controls need Full Disk Access for RillDisk. Other folders you cannot read are skipped and listed under Issues.".into()
        } else if self.state.can_elevate {
            "Not running as root. Folders you cannot read are skipped and listed under Issues. Restart with administrator rights to read them; RillDisk never changes permissions or files.".into()
        } else {
            "Not running as root. Folders you cannot read are skipped and listed under Issues."
                .into()
        }
    }

    /// Settings > Privacy: the route to more rights, if any, reopening `root`.
    pub fn settings_control(&mut self, ui: &mut Ui, root: Option<&Path>, scanning: bool) {
        self.poll(ui.ctx());
        if self.elevated() {
            badge(ui).on_hover_text(self.badge_tip());
            return;
        }
        let offer = if cfg!(target_os = "macos") {
            Offer::FullDiskAccess
        } else if !self.state.can_elevate {
            Offer::None
        } else if cfg!(windows) || self.pkexec {
            Offer::Restart
        } else {
            let exe = std::env::current_exe().unwrap_or_else(|_| "rilldisk-desktop".into());
            Offer::Command(sudo_command(
                &exe,
                root.map(absolute).as_deref(),
                t().dark,
                &|key| std::env::var_os(key),
            ))
        };
        self.action(ui, &offer, root, scanning);
    }

    /// The last attempt's result, for Settings.
    pub fn note(&self) -> Option<&str> {
        self.note.as_ref().map(|n| n.text.as_str())
    }

    /// Restart elevated, rescanning `root` if given. Windows closes this window
    /// once the elevated instance has started; a declined prompt only leaves a note.
    fn restart(&mut self, ctx: &egui::Context, root: Option<&Path>) {
        if self.pending() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            self.note = Some(Note::new(
                "Restart failed",
                "Could not find the RillDisk executable to restart.",
            ));
            return;
        };
        let root = root.map(absolute);
        let dark = t().dark;
        #[cfg(windows)]
        match windows::run_as(&exe, &runas_parameters(root.as_deref(), dark)) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(error) if error.raw_os_error() == Some(windows::ERROR_CANCELLED) => {
                self.note = Some(Note::declined());
            }
            Err(error) => {
                self.note = Some(Note::new(
                    "Restart failed",
                    format!("Could not restart as administrator: {error}"),
                ));
            }
        }
        #[cfg(not(windows))]
        {
            let args = elevated_command("pkexec", &exe, root.as_deref(), dark, &|key| {
                std::env::var_os(key)
            });
            match std::process::Command::new(&args[0])
                .args(&args[1..])
                .spawn()
            {
                Ok(child) => {
                    self.child = Some(child);
                    self.note = Some(Note::new(
                        "Waiting for authentication…",
                        "Waiting for authentication. When the administrator window opens, you can close this one.",
                    ));
                    ctx.request_repaint();
                }
                Err(error) => {
                    self.note = Some(Note::new(
                        "Restart failed",
                        format!("Could not start pkexec: {error}"),
                    ));
                }
            }
        }
    }

    /// Linux: report how the `pkexec` launch ended. Its exit status is the elevated
    /// app's own once authentication succeeded.
    #[cfg_attr(windows, allow(unused_variables))]
    fn poll(&mut self, ctx: &egui::Context) {
        #[cfg(not(windows))]
        if let Some(child) = &mut self.child {
            let status = match child.try_wait() {
                Ok(None) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(500));
                    return;
                }
                Ok(Some(status)) => Ok(status),
                Err(error) => Err(error),
            };
            self.child = None;
            self.note = match status {
                // The elevated app ran and was closed normally.
                Ok(status) if status.success() => None,
                // pkexec: 126 = not authorized or dismissed, 127 = authentication failed.
                Ok(status) if matches!(status.code(), Some(126 | 127)) => Some(Note::declined()),
                Ok(status) => Some(Note::new(
                    "Restart failed",
                    format!("The administrator instance stopped ({status})."),
                )),
                Err(error) => Some(Note::new(
                    "Restart failed",
                    format!("Could not follow the administrator instance: {error}"),
                )),
            };
        }
    }
}

fn restart_label() -> &'static str {
    if cfg!(windows) {
        "Restart as administrator"
    } else {
        "Restart with administrator rights"
    }
}

/// The "Administrator" pill.
fn badge(ui: &mut Ui) -> egui::Response {
    let th = t();
    let text = "Administrator";
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), style::semibold(11.0), th.good);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 30.0, 22.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(
        rect,
        11,
        style::alpha(th.good, if th.dark { 44 } else { 30 }),
    );
    icons::paint(
        p,
        Rect::from_center_size(pos2(rect.left() + 13.0, rect.center().y), vec2(12.0, 12.0)),
        Icon::Shield,
        th.good,
    );
    p.galley(
        pos2(rect.left() + 23.0, rect.center().y - galley.size().y / 2.0),
        galley,
        th.good,
    );
    response
}

fn row(ui: &mut Ui, issue: &ScanIssue, kind: IssueKind) {
    let th = t();
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    // The row truncates both lines; the full text is one hover away.
    response.on_hover_text(format!("{}\n{}", issue.path.display(), issue.message));
    let p = ui.painter();
    let card = rect.shrink2(vec2(0.0, 3.0));
    p.rect_filled(card, 12, th.card);
    let tile = Rect::from_min_size(
        pos2(card.left() + 12.0, card.center().y - 15.0),
        vec2(30.0, 30.0),
    );
    let color = kind_color(kind);
    p.rect_filled(tile, 9, style::alpha(color, 40));
    let icon = if kind == IssueKind::AccessDenied {
        Icon::Shield
    } else {
        Icon::Warning
    };
    icons::paint(p, tile.shrink(8.0), icon, color);
    let x = tile.right() + 12.0;
    let g = w::fit(
        p,
        issue.path.display().to_string(),
        style::mono(12.0),
        th.text,
        card.right() - x - 12.0,
    );
    p.galley(pos2(x, card.top() + 9.0), g, th.text);
    let g = w::fit(
        p,
        issue.message.clone(),
        style::regular(11.5),
        th.faint,
        card.right() - x - 12.0,
    );
    p.galley(pos2(x, card.top() + 27.0), g, th.faint);
}

fn kind_color(kind: IssueKind) -> egui::Color32 {
    let th = t();
    match kind {
        IssueKind::AccessDenied => th.accent,
        IssueKind::InUse | IssueKind::NotFound => th.info,
        _ => th.warn,
    }
}

fn kind_title(kind: IssueKind) -> &'static str {
    match kind {
        IssueKind::AccessDenied => "Access denied",
        IssueKind::InUse => "In use",
        IssueKind::NotFound => "Gone during the scan",
        IssueKind::Stalled => "Not responding",
        IssueKind::EntryLimit => "Entry limit",
        IssueKind::Other => "Other",
    }
}

fn kind_help(kind: IssueKind) -> &'static str {
    match kind {
        IssueKind::AccessDenied => {
            "Protected folders the OS refused to list or examine. Their contents are left out of the totals."
        }
        IssueKind::InUse => {
            "Another program had these open or locked. A rescan usually reads them."
        }
        IssueKind::NotFound => "Deleted or moved while the scan ran.",
        IssueKind::Stalled => {
            "No response before the timeout, such as a hung network or cloud folder."
        }
        IssueKind::EntryLimit => "The scan stopped at its memory bound.",
        IssueKind::Other => "Each message says what could not be read or checked.",
    }
}

fn callout_text(offer: &Offer, count: u64, privacy: u64, elevated: bool) -> (String, String) {
    let folders = style::plural(count, "protected folder");
    match offer {
        Offer::Restart if cfg!(windows) => (
            format!("{folders} could not be read"),
            "Windows protects these folders from normal accounts. Restart as administrator to read them; the scan reruns on the same folder. RillDisk only lists them and never changes permissions or files.".into(),
        ),
        Offer::Restart => (
            format!("{folders} could not be read"),
            "Restart with administrator rights (you will be asked to authenticate) to read them; the scan reruns on the same folder. RillDisk only lists them and never changes permissions or files.".into(),
        ),
        Offer::FullDiskAccess => (
            if privacy == 1 {
                "1 folder is protected by macOS privacy controls".to_string()
            } else {
                format!(
                    "{} folders are protected by macOS privacy controls",
                    style::count(privacy)
                )
            },
            "Grant RillDisk Full Disk Access in System Settings > Privacy & Security, then quit and reopen RillDisk and rescan. Administrator rights alone do not lift this protection.".into(),
        ),
        Offer::Command(_) => (
            format!("{folders} could not be read"),
            "No graphical prompt (pkexec) is installed. Run this in a terminal to read them:".into(),
        ),
        Offer::None if elevated && count > 0 => (
            "Running as administrator".into(),
            format!("{folders} still could not be read. These are usually held exclusively by the system."),
        ),
        Offer::None => (
            "Running as administrator".into(),
            "Protected folders are listed read-only with administrator rights.".into(),
        ),
    }
}

/// Issues grouped by kind in [`ORDER`]; empty groups are left out.
pub fn group(issues: &[ScanIssue]) -> Vec<(IssueKind, Vec<&ScanIssue>)> {
    ORDER
        .iter()
        .map(|&kind| (kind, issues.iter().filter(|i| i.kind == kind).collect()))
        .filter(|(_, list): &(IssueKind, Vec<&ScanIssue>)| !list.is_empty())
        .collect()
}

/// What to offer for `summary`. `privacy_denied_count` is counted by the scanner
/// before the issue list is capped, so the Full Disk Access offer holds even when
/// none of those folders is among the listed issues.
fn offer(summary: &Summary, elevated: bool, can_elevate: bool, pkexec: bool, dark: bool) -> Offer {
    if summary.access_denied_count == 0 {
        return Offer::None;
    }
    // macOS privacy protection refuses with EPERM even for root.
    if summary.privacy_denied_count > 0 {
        return Offer::FullDiskAccess;
    }
    if elevated || !can_elevate {
        return Offer::None;
    }
    if cfg!(windows) || pkexec {
        return Offer::Restart;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "rilldisk-desktop".into());
    Offer::Command(sudo_command(
        &exe,
        Some(&absolute(&summary.root)),
        dark,
        &|key| std::env::var_os(key),
    ))
}

fn absolute(root: &Path) -> PathBuf {
    std::fs::canonicalize(root)
        .map(rilldisk_core::simplify)
        .unwrap_or_else(|_| root.to_path_buf())
}

/// Display variables the elevated app needs to open a window on this session.
const DISPLAY_VARS: [&str; 4] = [
    "DISPLAY",
    "XAUTHORITY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
];

fn theme_name(dark: bool) -> &'static str {
    if dark { "dark" } else { "light" }
}

#[cfg_attr(windows, allow(dead_code))]
/// `<launcher> env DISPLAY=… <exe> [root] --theme <theme>`. pkexec clears the
/// environment and sudo resets it, so the display variables that are set are passed
/// on explicitly. Both also make `HOME` root's: an X session without `XAUTHORITY`
/// uses `$HOME/.Xauthority`, so the user's file is passed in that case.
pub fn elevated_command(
    launcher: &str,
    exe: &Path,
    root: Option<&Path>,
    dark: bool,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![launcher.into(), "env".into()];
    for key in DISPLAY_VARS {
        let value = env(key).or_else(|| {
            let default_cookie = key == "XAUTHORITY" && env("DISPLAY").is_some();
            default_cookie
                .then(|| env("HOME"))
                .flatten()
                .map(|mut home| {
                    // A Unix path, so `/` rather than the build platform's separator.
                    home.push("/.Xauthority");
                    home
                })
        });
        if let Some(value) = value {
            let mut pair = OsString::from(format!("{key}="));
            pair.push(value);
            args.push(pair);
        }
    }
    args.push(exe.as_os_str().into());
    if let Some(root) = root {
        args.push(root.as_os_str().into());
    }
    args.extend(["--theme", theme_name(dark)].map(OsString::from));
    args
}

/// A copyable fallback when `pkexec` is not installed.
fn sudo_command(
    exe: &Path,
    root: Option<&Path>,
    dark: bool,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> String {
    elevated_command("sudo", exe, root, dark, env)
        .iter()
        .map(|arg| shell_quote(&arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+%".contains(c))
    {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
/// The elevated instance's command line: the scan root if any, then the theme.
pub fn runas_parameters(root: Option<&Path>, dark: bool) -> String {
    let theme = format!("--theme {}", theme_name(dark));
    match root {
        Some(root) => format!("{} {theme}", quote_windows_arg(&root.to_string_lossy())),
        None => theme,
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Quote one argument so `CommandLineToArgvW` (and Rust's `std::env::args`) reads
/// it back unchanged: backslashes only double before a quote or the closing quote,
/// so `C:\` becomes `"C:\\"` rather than an escaped quote.
pub fn quote_windows_arg(arg: &str) -> String {
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// Opens System Settings at Privacy & Security > Full Disk Access.
fn open_full_disk_access() -> Note {
    const URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
    match std::process::Command::new("open").arg(URL).spawn() {
        Ok(_) => Note::new(
            "Settings opened",
            "Turn on RillDisk in the list (use + to add it if missing), then quit and reopen RillDisk and rescan.",
        ),
        Err(error) => Note::new(
            "Settings not opened",
            format!(
                "Could not open System Settings ({error}). Open Privacy & Security > Full Disk Access yourself."
            ),
        ),
    }
}

#[cfg(windows)]
mod windows {
    use std::{ffi::OsStr, io, os::windows::ffi::OsStrExt, path::Path};
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};

    /// The user dismissed the UAC prompt.
    pub const ERROR_CANCELLED: i32 = windows_sys::Win32::Foundation::ERROR_CANCELLED as i32;
    /// `SW_SHOWNORMAL`.
    const SHOW_NORMAL: i32 = 1;

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(Some(0)).collect()
    }

    /// Start `exe` through UAC ("runas"). Returns once the user accepted and the
    /// process started, or with the error (`ERROR_CANCELLED` when declined).
    pub fn run_as(exe: &Path, parameters: &str) -> io::Result<()> {
        let verb = wide(OsStr::new("runas"));
        let file = wide(exe.as_os_str());
        let parameters = wide(OsStr::new(parameters));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: verb.as_ptr(),
            lpFile: file.as_ptr(),
            lpParameters: parameters.as_ptr(),
            nShow: SHOW_NORMAL,
            ..Default::default()
        };
        // SAFETY: every string is NUL-terminated and outlives this synchronous call;
        // unused fields are zero as documented.
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_arguments_round_trip() {
        assert_eq!(quote_windows_arg(r"C:\"), r#""C:\\""#);
        assert_eq!(
            quote_windows_arg(r"C:\Program Files\App"),
            r#""C:\Program Files\App""#
        );
        assert_eq!(quote_windows_arg(r"D:\a\\"), r#""D:\a\\\\""#);
        assert_eq!(quote_windows_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_windows_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(
            runas_parameters(Some(Path::new(r"C:\")), true),
            r#""C:\\" --theme dark"#
        );
        assert_eq!(runas_parameters(None, false), "--theme light");
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn elevated_commands_pass_the_display_and_theme() {
        // Wayland only: just the variables that are set, no X cookie.
        let wayland = |key: &str| match key {
            "WAYLAND_DISPLAY" => Some(OsString::from("wayland-0")),
            "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/1000")),
            "HOME" => Some(OsString::from("/home/a")),
            _ => None,
        };
        let exe = Path::new("/opt/rilldisk/rilldisk-desktop");
        assert_eq!(
            strings(elevated_command(
                "pkexec",
                exe,
                Some(Path::new("/")),
                true,
                &wayland
            )),
            [
                "pkexec",
                "env",
                "WAYLAND_DISPLAY=wayland-0",
                "XDG_RUNTIME_DIR=/run/user/1000",
                "/opt/rilldisk/rilldisk-desktop",
                "/",
                "--theme",
                "dark"
            ]
        );
        // X11 without XAUTHORITY: the user's default cookie, not root's.
        let x11 = |key: &str| match key {
            "DISPLAY" => Some(OsString::from(":0")),
            "HOME" => Some(OsString::from("/home/a b")),
            _ => None,
        };
        assert_eq!(
            strings(elevated_command("pkexec", exe, None, false, &x11)),
            [
                "pkexec",
                "env",
                "DISPLAY=:0",
                "XAUTHORITY=/home/a b/.Xauthority",
                "/opt/rilldisk/rilldisk-desktop",
                "--theme",
                "light"
            ]
        );
        assert_eq!(
            sudo_command(
                Path::new("/usr/bin/rilldisk-desktop"),
                Some(Path::new("/home/a b")),
                false,
                &x11
            ),
            "sudo env DISPLAY=:0 'XAUTHORITY=/home/a b/.Xauthority' /usr/bin/rilldisk-desktop '/home/a b' --theme light"
        );
    }

    fn summary(issues: Vec<ScanIssue>) -> Summary {
        let denied = |i: &&ScanIssue| i.kind == IssueKind::AccessDenied;
        let access_denied_count = issues.iter().filter(denied).count() as u64;
        // As the scanner counts it: EPERM denials, on macOS only.
        let privacy_denied_count = issues
            .iter()
            .filter(denied)
            .filter(|i| cfg!(target_os = "macos") && i.os_error == Some(1))
            .count() as u64;
        Summary {
            schema_version: 2,
            root: PathBuf::from("/"),
            backend: "test",
            started_unix_ms: 0,
            elapsed_ms: 0,
            files: 0,
            directories: 1,
            skipped_links: 0,
            skipped_mounts: 0,
            hard_link_duplicates: 0,
            logical_bytes: 0,
            unique_logical_bytes: 0,
            allocated_bytes: None,
            issue_count: issues.len() as u64,
            access_denied_count,
            privacy_denied_count,
            issues,
            cancelled: false,
            complete: false,
            elevated: false,
        }
    }

    #[test]
    fn issues_group_access_denied_first() {
        let issues = vec![
            ScanIssue::new("/a", IssueKind::Other, "x"),
            ScanIssue::new("/b", IssueKind::AccessDenied, "denied"),
            ScanIssue::new("/c", IssueKind::InUse, "busy"),
            ScanIssue::new("/d", IssueKind::AccessDenied, "denied"),
        ];
        let groups = group(&issues);
        let kinds: Vec<_> = groups.iter().map(|g| g.0).collect();
        assert_eq!(
            kinds,
            [IssueKind::AccessDenied, IssueKind::InUse, IssueKind::Other]
        );
        assert_eq!(groups[0].1.len(), 2);
    }

    #[test]
    fn restart_is_offered_only_when_it_can_help() {
        let denied = summary(vec![ScanIssue::new(
            "/b",
            IssueKind::AccessDenied,
            "denied",
        )]);
        let busy = summary(vec![ScanIssue::new("/c", IssueKind::InUse, "busy")]);
        assert_eq!(offer(&busy, false, true, true, false), Offer::None);
        assert_eq!(offer(&denied, true, false, true, false), Offer::None);
        assert_eq!(offer(&denied, false, false, true, false), Offer::None);
        assert_eq!(offer(&denied, false, true, true, false), Offer::Restart);
        let fallback = offer(&denied, false, true, false, false);
        if cfg!(windows) {
            assert_eq!(fallback, Offer::Restart);
        } else {
            assert!(
                matches!(&fallback, Offer::Command(c) if c.starts_with("sudo env ") && c.ends_with("--theme light")),
                "{fallback:?}"
            );
        }
        // macOS privacy protection (EPERM) needs Full Disk Access, even as root.
        let mut tcc = ScanIssue::new("/Users/a/Library/Mail", IssueKind::AccessDenied, "denied");
        tcc.os_error = Some(1);
        let tcc = summary(vec![tcc]);
        if cfg!(target_os = "macos") {
            assert_eq!(
                offer(&tcc, true, false, false, false),
                Offer::FullDiskAccess
            );
        } else {
            assert_eq!(offer(&tcc, true, false, false, false), Offer::None);
        }
    }

    #[test]
    fn full_disk_access_holds_when_the_denial_is_not_listed() {
        // 150 denials: the listed ones are other folders, the privacy one was cut.
        let mut capped = summary(Vec::new());
        capped.access_denied_count = 150;
        capped.privacy_denied_count = 1;
        assert_eq!(
            offer(&capped, true, false, false, false),
            Offer::FullDiskAccess
        );
    }
}
