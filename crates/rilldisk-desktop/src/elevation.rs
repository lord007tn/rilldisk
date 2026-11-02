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
use std::{ffi::OsString, path::Path};

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

pub struct Elevate {
    state: Elevation,
    /// Result of the last attempt, shown quietly under the offer.
    note: Option<String>,
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
            note: None,
            #[cfg(not(windows))]
            child: None,
            preview_elevated: false,
        }
    }

    fn elevated(&self) -> bool {
        self.state.is_elevated || self.preview_elevated
    }

    pub fn offer(&self, summary: &Summary) -> Offer {
        offer(
            summary,
            self.elevated(),
            self.state.can_elevate,
            &|program| on_path(program),
        )
    }

    /// Header: an "Administrator" badge when elevated, otherwise a compact restart
    /// button when protected folders were skipped. For a right-to-left layout.
    pub fn header(&mut self, ui: &mut Ui, summary: &Summary) {
        let th = t();
        if self.elevated() {
            let tip = if self.state.backup_privilege_enabled || self.preview_elevated {
                "Running as administrator. Protected folders are listed with the backup privilege (read-only)."
            } else {
                "Running with administrator rights."
            };
            badge(ui).on_hover_text(tip);
            return;
        }
        if self.offer(summary) == Offer::Restart {
            let count = style::count(summary.access_denied_count);
            if w::button(ui, Kind::Soft, Some(Icon::Shield), restart_label())
                .on_hover_text(format!(
                    "{count} protected folders could not be read. Restart RillDisk with administrator rights and rescan {}.",
                    summary.root.display()
                ))
                .clicked()
            {
                self.restart(ui.ctx(), &summary.root);
            }
        } else if let Some(note) = &self.note {
            ui.label(RichText::new(note).color(th.faint).size(11.5));
        }
    }

    /// The Issues tab: the offer on top, then issues grouped by kind.
    pub fn issues(&mut self, ui: &mut Ui, summary: &Summary) {
        let th = t();
        self.poll();
        let offer = self.offer(summary);
        if offer != Offer::None || self.elevated() {
            self.callout(ui, summary, &offer);
            ui.add_space(12.0);
        }
        ui.label(
            RichText::new(format!(
                "{} issues: folders that could not be read (left out of the totals) or checks that could not be made. {} are listed.",
                style::count(summary.issue_count),
                if summary.issues.len() as u64 == summary.issue_count {
                    "All".to_string()
                } else {
                    format!("The first {}", summary.issues.len())
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
                    let total = if kind == IssueKind::AccessDenied {
                        summary.access_denied_count
                    } else {
                        issues.len() as u64
                    };
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(kind_title(kind)).font(style::semibold(13.5)));
                        w::chip(ui, &style::count(total), kind_color(kind));
                    });
                    ui.label(RichText::new(kind_help(kind)).color(th.faint).size(11.5));
                    ui.add_space(6.0);
                    for issue in issues {
                        row(ui, issue, kind);
                    }
                }
            });
    }

    fn callout(&mut self, ui: &mut Ui, summary: &Summary, offer: &Offer) {
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
                    ui.vertical(|ui| {
                        let count = summary.access_denied_count;
                        let (title, body) = callout_text(offer, count, self.elevated());
                        ui.label(RichText::new(title).font(style::semibold(15.0)));
                        ui.label(RichText::new(body).color(th.dim));
                        if let Offer::Command(command) = offer {
                            ui.label(RichText::new(command).font(style::mono(12.0)));
                        }
                        if let Some(note) = &self.note {
                            ui.add_space(2.0);
                            ui.label(RichText::new(note).color(th.faint).size(11.5));
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        match offer {
                            Offer::Restart => {
                                if w::button(ui, Kind::Primary, Some(Icon::Shield), restart_label())
                                    .clicked()
                                {
                                    self.restart(ui.ctx(), &summary.root);
                                }
                            }
                            Offer::FullDiskAccess => {
                                if w::button(ui, Kind::Primary, None, "Grant Full Disk Access")
                                    .clicked()
                                {
                                    self.note = Some(open_full_disk_access());
                                }
                            }
                            Offer::Command(command) => {
                                if w::button(ui, Kind::Soft, Some(Icon::Copy), "Copy command")
                                    .on_hover_text(command)
                                    .clicked()
                                {
                                    ui.ctx().copy_text(command.clone());
                                    self.note = Some(
                                        "Copied. Run it in a terminal, then close this window."
                                            .into(),
                                    );
                                }
                            }
                            Offer::None => {}
                        }
                    });
                });
            });
    }

    /// Restart elevated on `root`. Windows closes this window once the elevated
    /// instance has started; a declined prompt only leaves a note.
    fn restart(&mut self, ctx: &egui::Context, root: &Path) {
        let Ok(exe) = std::env::current_exe() else {
            self.note = Some("Could not find the RillDisk executable to restart.".into());
            return;
        };
        let root = absolute(root);
        #[cfg(windows)]
        {
            match windows::run_as(&exe, &runas_parameters(&root, t().dark)) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(error) if error.raw_os_error() == Some(windows::ERROR_CANCELLED) => {
                    self.note = Some("Administrator access was declined. Nothing changed.".into());
                }
                Err(error) => {
                    self.note = Some(format!("Could not restart as administrator: {error}"))
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = ctx;
            let args = pkexec_command(&exe, &root, &|key: &str| std::env::var_os(key));
            match std::process::Command::new(&args[0])
                .args(&args[1..])
                .spawn()
            {
                Ok(child) => {
                    self.child = Some(child);
                    self.note = Some("Waiting for authentication. When the administrator window opens, you can close this one.".into());
                }
                Err(error) => self.note = Some(format!("Could not start pkexec: {error}")),
            }
        }
    }

    /// Linux: report a declined or failed `pkexec` prompt.
    fn poll(&mut self) {
        #[cfg(not(windows))]
        if let Some(child) = &mut self.child
            && let Ok(Some(status)) = child.try_wait()
        {
            // pkexec: 126 = not authorized or dismissed, 127 = authentication failed.
            if matches!(status.code(), Some(126 | 127)) {
                self.note = Some("Administrator access was declined. Nothing changed.".into());
            }
            self.child = None;
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
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
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
            "Protected folders the OS refused to list. Their contents are left out of the totals."
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

fn callout_text(offer: &Offer, count: u64, elevated: bool) -> (String, String) {
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
            format!("{folders} are protected by macOS privacy controls"),
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

/// macOS privacy protection refuses with `EPERM` even for root; plain Unix
/// permissions refuse with `EACCES`.
fn privacy_protected(issue: &ScanIssue) -> bool {
    issue.kind == IssueKind::AccessDenied && issue.os_error == Some(1)
}

fn offer(
    summary: &Summary,
    elevated: bool,
    can_elevate: bool,
    on_path: &dyn Fn(&str) -> bool,
) -> Offer {
    if summary.access_denied_count == 0 {
        return Offer::None;
    }
    if cfg!(target_os = "macos") && summary.issues.iter().any(privacy_protected) {
        return Offer::FullDiskAccess;
    }
    if elevated || !can_elevate {
        return Offer::None;
    }
    if cfg!(windows) || on_path("pkexec") {
        return Offer::Restart;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "rilldisk-desktop".into());
    Offer::Command(sudo_command(&exe, &absolute(&summary.root)))
}

fn absolute(root: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(root)
        .map(rilldisk_core::simplify)
        .unwrap_or_else(|_| root.to_path_buf())
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Display variables the elevated app needs to open a window on this session.
const DISPLAY_VARS: [&str; 4] = [
    "DISPLAY",
    "XAUTHORITY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
];

#[cfg_attr(windows, allow(dead_code))]
/// `pkexec env DISPLAY=… <exe> <root>`: polkit clears the environment, so the
/// display variables that are set are passed on explicitly.
pub fn pkexec_command(
    exe: &Path,
    root: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["pkexec".into(), "env".into()];
    for key in DISPLAY_VARS {
        if let Some(value) = env(key) {
            let mut pair = OsString::from(format!("{key}="));
            pair.push(value);
            args.push(pair);
        }
    }
    args.push(exe.as_os_str().into());
    args.push(root.as_os_str().into());
    args
}

/// A copyable fallback when `pkexec` is not installed.
fn sudo_command(exe: &Path, root: &Path) -> String {
    format!(
        "sudo --preserve-env={} {} {}",
        DISPLAY_VARS.join(","),
        shell_quote(&exe.to_string_lossy()),
        shell_quote(&root.to_string_lossy())
    )
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
/// The elevated instance's command line: the scan root, then the theme.
pub fn runas_parameters(root: &Path, dark: bool) -> String {
    format!(
        "{} --theme {}",
        quote_windows_arg(&root.to_string_lossy()),
        if dark { "dark" } else { "light" }
    )
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
fn open_full_disk_access() -> String {
    const URL: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
    match std::process::Command::new("open").arg(URL).spawn() {
        Ok(_) => "Turn on RillDisk in the list (use + to add it if missing), then quit and reopen RillDisk and rescan.".into(),
        Err(error) => format!("Could not open System Settings ({error}). Open Privacy & Security > Full Disk Access yourself."),
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
    use std::path::PathBuf;

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
            runas_parameters(Path::new(r"C:\"), true),
            r#""C:\\" --theme dark"#
        );
    }

    #[test]
    fn pkexec_passes_only_set_display_variables() {
        let env = |key: &str| match key {
            "DISPLAY" => Some(OsString::from(":0")),
            "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/1000")),
            _ => None,
        };
        let args = pkexec_command(
            Path::new("/opt/rilldisk/rilldisk-desktop"),
            Path::new("/"),
            &env,
        );
        let args: Vec<_> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "pkexec",
                "env",
                "DISPLAY=:0",
                "XDG_RUNTIME_DIR=/run/user/1000",
                "/opt/rilldisk/rilldisk-desktop",
                "/"
            ]
        );
        assert_eq!(
            sudo_command(
                Path::new("/usr/bin/rilldisk-desktop"),
                Path::new("/home/a b")
            ),
            "sudo --preserve-env=DISPLAY,XAUTHORITY,WAYLAND_DISPLAY,XDG_RUNTIME_DIR /usr/bin/rilldisk-desktop '/home/a b'"
        );
    }

    fn summary(issues: Vec<ScanIssue>) -> Summary {
        let denied = issues
            .iter()
            .filter(|i| i.kind == IssueKind::AccessDenied)
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
            access_denied_count: denied,
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
        let yes = |_: &str| true;
        let no = |_: &str| false;
        assert_eq!(offer(&busy, false, true, &yes), Offer::None);
        assert_eq!(offer(&denied, true, false, &yes), Offer::None);
        assert_eq!(offer(&denied, false, false, &yes), Offer::None);
        assert_eq!(offer(&denied, false, true, &yes), Offer::Restart);
        let fallback = offer(&denied, false, true, &no);
        if cfg!(windows) {
            assert_eq!(fallback, Offer::Restart);
        } else {
            assert!(matches!(fallback, Offer::Command(c) if c.starts_with("sudo ")));
        }
        // macOS privacy protection (EPERM) needs Full Disk Access, even as root.
        let mut tcc = ScanIssue::new("/Users/a/Library/Mail", IssueKind::AccessDenied, "denied");
        tcc.os_error = Some(1);
        let tcc = summary(vec![tcc]);
        if cfg!(target_os = "macos") {
            assert_eq!(offer(&tcc, true, false, &yes), Offer::FullDiskAccess);
        } else {
            assert_eq!(offer(&tcc, true, false, &yes), Offer::None);
        }
    }
}
