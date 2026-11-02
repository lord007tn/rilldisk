// SPDX-License-Identifier: AGPL-3.0-only
//! The Home page's "AI assistants" card: finds AI clients on this machine,
//! connects RillDisk to them after showing the exact change, and checks the
//! connection with a real MCP handshake. Detection, writes and checks run on
//! background threads; see docs/proposals/mcp-setup.md for the scope.
use crate::icons::{self, Icon};
use crate::style::{self, t};
use crate::widgets::{self as w, Kind};
use eframe::egui::{self, Align, Color32, Frame, Layout, Margin, RichText, Sense, Ui, vec2};
use rilldisk_mcp_setup::{
    self as setup, Applied, Change, Check, ClientId, Detection, Env, Plan, Server, Status,
};
use std::{collections::HashMap, path::PathBuf, sync::mpsc, time::Duration};

/// Everything the card remembers between frames.
#[derive(Default)]
pub struct State {
    ready: Option<Ready>,
    detecting: Option<mpsc::Receiver<Vec<Detection>>>,
    detections: Vec<Detection>,
    preview: Option<Result<Plan, (ClientId, String)>>,
    jobs: HashMap<ClientId, Job>,
    results: HashMap<ClientId, Outcome>,
}

struct Ready {
    env: Env,
    /// `None` when the CLI is not next to this app; its expected path otherwise.
    server: Result<Server, PathBuf>,
}

enum Job {
    Applying(mpsc::Receiver<Result<Applied, String>>),
    Testing(mpsc::Receiver<Result<Check, String>>),
}

enum Outcome {
    Connected(Check),
    Failed(String),
    Saved(String),
}

impl State {
    fn ready(&mut self) -> &Ready {
        self.ready.get_or_insert_with(|| {
            let env = Env::from_system();
            let exe = std::env::current_exe()
                .map(|exe| setup::sibling_cli(&exe))
                .unwrap_or_else(|_| PathBuf::from("rilldisk"));
            let server = if exe.is_file() {
                Ok(Server::rilldisk(exe, std::slice::from_ref(&env.home)))
            } else {
                Err(exe)
            };
            Ready { env, server }
        })
    }

    fn detect(&mut self, ctx: &egui::Context) {
        let ready = self.ready();
        let env = ready.env.clone();
        // Without the CLI, detection still shows what is installed.
        let server = ready.server.clone().unwrap_or_else(|exe| Server {
            command: exe,
            args: vec!["mcp".into()],
        });
        self.detecting = Some(spawn(ctx, move || setup::detect(&env, &server)));
    }

    fn test(&mut self, ctx: &egui::Context, client: ClientId) {
        let Some(launch) = self
            .detections
            .iter()
            .find(|d| d.client == client)
            .and_then(|d| d.launch.clone())
        else {
            return;
        };
        let rx = spawn(ctx, move || {
            setup::check(&launch, setup::DEFAULT_TIMEOUT).map_err(|e| e.to_string())
        });
        self.jobs.insert(client, Job::Testing(rx));
    }

    fn preview(&mut self, client: ClientId, change: Change) {
        let ready = self.ready();
        let Ok(server) = &ready.server else { return };
        self.preview = Some(
            setup::plan(&ready.env, client, server, change).map_err(|e| (client, format!("{e:#}"))),
        );
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.detecting {
            match rx.try_recv() {
                Ok(found) => {
                    self.detections = found;
                    self.detecting = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.detecting = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let mut finished = Vec::new();
        for (client, job) in &self.jobs {
            match job {
                Job::Applying(rx) => match rx.try_recv() {
                    Ok(Ok(applied)) => {
                        let note = match (&applied.backup, &self.ready) {
                            (Some(backup), Some(ready)) => {
                                format!("Saved. Backup: {}", ready.env.display(backup))
                            }
                            _ => "Saved.".to_string(),
                        };
                        finished.push((*client, Some(Outcome::Saved(note)), true));
                    }
                    Ok(Err(error)) => finished.push((*client, Some(Outcome::Failed(error)), false)),
                    Err(mpsc::TryRecvError::Disconnected) => finished.push((*client, None, false)),
                    Err(mpsc::TryRecvError::Empty) => {}
                },
                Job::Testing(rx) => match rx.try_recv() {
                    Ok(result) => finished.push((
                        *client,
                        Some(match result {
                            Ok(check) => Outcome::Connected(check),
                            Err(error) => Outcome::Failed(error),
                        }),
                        false,
                    )),
                    Err(mpsc::TryRecvError::Disconnected) => finished.push((*client, None, false)),
                    Err(mpsc::TryRecvError::Empty) => {}
                },
            }
        }
        let mut changed = false;
        for (client, outcome, wrote) in finished {
            self.jobs.remove(&client);
            match outcome {
                Some(outcome) => {
                    self.results.insert(client, outcome);
                }
                None => {
                    self.results.remove(&client);
                }
            }
            changed |= wrote;
        }
        if changed {
            // Re-read the configs, then check what the client will now run.
            let ready = self.ready();
            let (env, server) = (ready.env.clone(), ready.server.clone());
            if let Ok(server) = server {
                self.detections = setup::detect(&env, &server);
                let connected: Vec<_> = self
                    .detections
                    .iter()
                    .filter(|d| {
                        d.status == Status::Configured && !self.jobs.contains_key(&d.client)
                    })
                    .map(|d| d.client)
                    .filter(|c| matches!(self.results.get(c), Some(Outcome::Saved(_))))
                    .collect();
                for client in connected {
                    self.test(ctx, client);
                }
            }
        }
        if self.detecting.is_some() || !self.jobs.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }
}

fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    job: impl FnOnce() -> T + Send + 'static,
) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("rilldisk-assistants".into())
        .spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        })
        .expect("spawn assistants thread");
    rx
}

/// Renders the card at `width` (outer width, including the card's padding).
pub fn panel(ui: &mut Ui, state: &mut State, width: f32) {
    let ctx = ui.ctx().clone();
    if state.ready.is_none() {
        state.detect(&ctx);
    }
    state.poll(&ctx);
    let th = t();
    w::card_frame().show(ui, |ui| {
        ui.set_width(width - 32.0);
        ui.horizontal(|ui| {
            let (tile, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
            ui.painter().rect_filled(tile, 10, th.field);
            icons::paint(ui.painter(), tile.shrink(9.0), Icon::Agent, th.dim);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("AI assistants").font(style::semibold(13.5)));
                ui.label(
                    RichText::new(
                        "Give your coding assistant RillDisk's read-only tools over MCP. Also: rilldisk mcp setup",
                    )
                    .font(style::regular(12.0))
                    .color(th.dim),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if state.detecting.is_some() {
                    ui.add(egui::Spinner::new().size(16.0).color(th.accent));
                } else if w::icon_button(ui, Icon::Refresh, "Detect again").clicked() {
                    state.detect(&ctx);
                }
            });
        });
        if let Some(Err(exe)) = state.ready.as_ref().map(|r| &r.server) {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "The rilldisk program was not found next to this app ({}). Reinstall RillDisk to connect assistants.",
                    exe.display()
                ))
                .font(style::regular(12.0))
                .color(th.warn),
            );
        }
        let installed: Vec<usize> = (0..state.detections.len())
            .filter(|&i| state.detections[i].status != Status::NotInstalled)
            .collect();
        if installed.is_empty() && state.detecting.is_none() {
            ui.add_space(10.0);
            ui.label(
                RichText::new("No supported AI clients found on this computer.")
                    .font(style::regular(12.0))
                    .color(th.dim),
            );
        }
        for i in installed {
            ui.add_space(10.0);
            row(ui, state, i, &ctx);
        }
        let missing: Vec<&str> = state
            .detections
            .iter()
            .filter(|d| d.status == Status::NotInstalled)
            .map(|d| d.name)
            .collect();
        if !missing.is_empty() {
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!("Also supported: {}", missing.join(", ")))
                    .font(style::regular(11.5))
                    .color(th.faint),
            );
        }
    });
}

fn chip(status: &Status) -> (&'static str, Color32) {
    let th = t();
    match status {
        Status::Configured => ("Connected", th.good),
        Status::NotConfigured => ("Not connected", th.dim),
        Status::Stale { missing: true, .. } => ("Path missing", th.warn),
        Status::Stale { .. } => ("Other copy", th.warn),
        Status::Invalid { .. } => ("Config error", th.danger),
        Status::NotInstalled => ("Not installed", th.faint),
    }
}

fn row(ui: &mut Ui, state: &mut State, index: usize, ctx: &egui::Context) {
    let th = t();
    let d = &state.detections[index];
    let client = d.client;
    let status = d.status.clone();
    let config = state.ready.as_ref().map_or_else(
        || d.config.display().to_string(),
        |r| r.env.display(&d.config),
    );
    let can_write = matches!(state.ready.as_ref().map(|r| &r.server), Some(Ok(_)));
    let busy = state.jobs.get(&client).map(|job| match job {
        Job::Applying(_) => "Saving…",
        Job::Testing(_) => "Testing…",
    });
    let mut action = None;
    ui.horizontal(|ui| {
        let (tile, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
        let hue = ClientId::ALL.iter().position(|c| *c == client).unwrap_or(0) as f32 / 10.0;
        ui.painter().rect_filled(tile, 9, style::chart(hue, 0));
        ui.painter().text(
            tile.center(),
            egui::Align2::CENTER_CENTER,
            monogram(client),
            style::semibold(12.0),
            th.ink,
        );
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(d.name).font(style::semibold(13.0)));
            let detail = match &status {
                Status::Stale { command, .. } => format!("{config}  ·  runs {command}"),
                _ => config.clone(),
            };
            ui.label(
                RichText::new(detail)
                    .font(style::regular(11.5))
                    .color(th.faint),
            )
            .on_hover_text(d.config.display().to_string());
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(busy) = busy {
                ui.label(RichText::new(busy).font(style::regular(12.0)).color(th.dim));
                ui.add(egui::Spinner::new().size(14.0).color(th.accent));
            } else {
                ui.add_enabled_ui(can_write, |ui| match &status {
                    Status::NotConfigured => {
                        if w::button(ui, Kind::Primary, None, "Connect").clicked() {
                            action = Some(Some(Change::Add));
                        }
                    }
                    Status::Stale { .. } => {
                        if w::button(ui, Kind::Ghost, None, "Remove").clicked() {
                            action = Some(Some(Change::Remove));
                        }
                        if w::button(ui, Kind::Primary, None, "Repair")
                            .on_hover_text("Point the entry at this RillDisk")
                            .clicked()
                        {
                            action = Some(Some(Change::Add));
                        }
                    }
                    Status::Configured => {
                        if w::button(ui, Kind::Ghost, None, "Remove").clicked() {
                            action = Some(Some(Change::Remove));
                        }
                        if w::button(ui, Kind::Soft, None, "Test")
                            .on_hover_text(
                                "Start the server as this client would and list its tools",
                            )
                            .clicked()
                        {
                            action = Some(None);
                        }
                    }
                    _ => {}
                });
            }
            ui.add_space(4.0);
            let (text, color) = chip(&status);
            let chip = w::chip(ui, text, color);
            if let Status::Invalid { error } = &status {
                chip.on_hover_text(error.as_str());
            }
        });
    });
    match action {
        Some(Some(change)) => {
            state.results.remove(&client);
            state.preview(client, change);
        }
        Some(None) => {
            state.results.remove(&client);
            state.test(ctx, client);
        }
        None => {}
    }
    outcome(ui, state, client);
    preview(ui, state, client, ctx);
}

/// Two letters for the client tile (no brand artwork is bundled).
fn monogram(client: ClientId) -> &'static str {
    match client {
        ClientId::ClaudeCode => "CC",
        ClientId::ClaudeDesktop => "CD",
        ClientId::Codex => "Cx",
        ClientId::Cursor => "Cu",
        ClientId::VsCode => "VS",
        ClientId::Windsurf => "Ws",
        ClientId::Gemini => "Ge",
        ClientId::Zed => "Ze",
        ClientId::OpenCode => "OC",
        ClientId::Cline => "Cl",
    }
}

/// Extra height the card adds over a one-line card, for page centering.
pub fn extra_height(state: &State) -> f32 {
    let rows = state
        .detections
        .iter()
        .filter(|d| d.status != Status::NotInstalled)
        .count();
    let preview = if matches!(state.preview, Some(Ok(_))) {
        300.0
    } else {
        0.0
    };
    rows as f32 * 50.0 + 30.0 + preview
}

fn outcome(ui: &mut Ui, state: &State, client: ClientId) {
    let th = t();
    let Some(result) = state.results.get(&client) else {
        return;
    };
    let (icon, color, text) = match result {
        Outcome::Connected(check) => (
            Icon::Check,
            th.good,
            format!(
                "Answered in {} ms with {} tools ({}). Restart {} to load it.",
                check.latency_ms,
                check.tools.len(),
                check.server,
                client.name()
            ),
        ),
        Outcome::Failed(error) => (Icon::Warning, th.danger, error.clone()),
        Outcome::Saved(note) => (Icon::Check, th.dim, note.clone()),
    };
    ui.horizontal(|ui| {
        ui.add_space(42.0);
        let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
        icons::paint(ui.painter(), rect, icon, color);
        ui.label(RichText::new(text).font(style::regular(11.5)).color(color));
    });
}

fn preview(ui: &mut Ui, state: &mut State, client: ClientId, ctx: &egui::Context) {
    let th = t();
    let mut close = false;
    let mut confirm = None;
    match &state.preview {
        Some(Err((c, error))) if *c == client => {
            ui.horizontal(|ui| {
                ui.add_space(42.0);
                ui.label(
                    RichText::new(error)
                        .font(style::regular(11.5))
                        .color(th.danger),
                );
                if w::button(ui, Kind::Ghost, None, "Dismiss").clicked() {
                    close = true;
                }
            });
        }
        Some(Ok(plan)) if plan.client == client => {
            ui.add_space(6.0);
            Frame::new()
                .fill(th.field)
                .corner_radius(10)
                .inner_margin(Margin::same(12))
                .outer_margin(Margin {
                    left: 42,
                    ..Default::default()
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let path = state
                        .ready
                        .as_ref()
                        .map_or_else(|| plan.path.display().to_string(), |r| r.env.display(&plan.path));
                    let what = match (plan.change, plan.changes_anything()) {
                        (_, false) => format!("Nothing to change in {path}."),
                        (Change::Add, true) => format!("RillDisk will add one entry to {path}:"),
                        (Change::Remove, true) => {
                            format!("RillDisk will remove its entry from {path}:")
                        }
                    };
                    ui.label(RichText::new(what).font(style::semibold(12.0)));
                    if let Some(command) = &plan.command {
                        ui.label(
                            RichText::new(format!("Using {}'s own command: {}", client.name(), command[1..].join(" ")))
                                .font(style::regular(11.5))
                                .color(th.dim),
                        );
                    }
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .id_salt(("assistant-diff", client))
                        .max_height(190.0)
                        // Inside the page's scroll area it would shrink to the minimum.
                        .min_scrolled_height((plan.diff.lines().count() as f32 * 16.0).min(190.0))
                        .show(ui, |ui| {
                            for line in plan.diff.lines().skip(2) {
                                let color = match line.chars().next() {
                                    Some('+') => th.good,
                                    Some('-') => th.danger,
                                    Some('@') => th.faint,
                                    _ => th.dim,
                                };
                                ui.label(RichText::new(line).font(style::mono(11.5)).color(color));
                            }
                        });
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Other entries, comments and settings stay as they are. A backup of the current file is kept next to it.",
                        )
                        .font(style::regular(11.5))
                        .color(th.faint),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if plan.changes_anything() {
                            let label = match plan.change {
                                Change::Add => "Apply",
                                Change::Remove => "Remove entry",
                            };
                            if w::button(ui, Kind::Primary, None, label).clicked() {
                                confirm = Some(plan.clone());
                            }
                        }
                        if w::button(ui, Kind::Ghost, None, "Cancel").clicked() {
                            close = true;
                        }
                    });
                });
        }
        _ => {}
    }
    if let Some(plan) = confirm {
        close = true;
        let rx = spawn(ctx, move || {
            setup::apply(&plan).map_err(|e| format!("{e:#}"))
        });
        state.jobs.insert(client, Job::Applying(rx));
    }
    if close {
        state.preview = None;
    }
}
