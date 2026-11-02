// SPDX-License-Identifier: AGPL-3.0-only
//! RillDisk desktop: a native, GPU-rendered disk map built on the shared core.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod capture;
mod cleanup_ui;
mod icons;
mod live;
mod settings;
mod style;
mod sunburst;
mod treemap;
mod update;
mod widgets;

use eframe::egui;
use std::{path::PathBuf, sync::Arc};

/// Wayland app id and X11 class. Matches the Linux .desktop file, icon and
/// AppStream metadata in packaging/linux so docks show the right icon.
const APP_ID: &str = "io.github.lord007tn.RillDisk";

/// Which scripted capture `--capture*` runs (development only).
#[derive(Clone, Copy)]
enum Capture {
    Tour,
    Live,
    Cleanup,
}

fn main() -> eframe::Result {
    let mut path = None;
    let mut capture = None;
    let mut dark = None;
    let mut identity = None;
    let mut size = [1480.0_f32, 920.0];
    let mut args = std::env::args().skip(1).peekable();
    // An option's value, unless the next token is another option.
    let value = |args: &mut std::iter::Peekable<_>| -> Option<String> {
        let next: Option<&String> = args.peek();
        next.filter(|v| !v.starts_with('-'))?;
        args.next()
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--capture" => capture = value(&mut args).map(|d| (PathBuf::from(d), Capture::Tour)),
            "--capture-live" => capture = value(&mut args).map(|d| (PathBuf::from(d), Capture::Live)),
            "--capture-cleanup" => {
                capture = value(&mut args).map(|d| (PathBuf::from(d), Capture::Cleanup))
            }
            // Unknown values keep the saved or system theme.
            "--theme" => match value(&mut args).as_deref() {
                Some("dark") => dark = Some(true),
                Some("light") => dark = Some(false),
                _ => {}
            },
            "--palette" => {
                if let Some(found) = value(&mut args).as_deref().and_then(style::Identity::parse) {
                    identity = Some(found);
                }
            }
            "--size" => {
                if let Some((w, h)) = value(&mut args).as_deref().and_then(|s| s.split_once('x')) {
                    size = [w.parse().unwrap_or(size[0]), h.parse().unwrap_or(size[1])];
                }
            }
            "--help" | "-h" => {
                attach_console();
                println!(
                    "rilldisk-desktop [PATH] [--theme light|dark] [--palette tide|moss|aurora] [--size WIDTHxHEIGHT] [--capture DIR] [--capture-cleanup DIR]\n\nOpens the RillDisk desktop app. PATH starts a scan immediately.\n--capture runs a scripted tour and saves screenshots to DIR (development).\n--capture-cleanup captures the cleanup dialogs; it only removes folders from a synthetic fixture.\n--version prints the version."
                );
                return Ok(());
            }
            "--version" | "-V" => {
                attach_console();
                println!("rilldisk-desktop {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => path = Some(arg),
        }
    }
    let tour = capture.map(|(dir, mode)| {
        let kind = match (path.is_none(), mode) {
            (_, Capture::Cleanup) => return capture::Tour::cleanup(dir),
            (true, _) => capture::TourKind::Welcome,
            (false, Capture::Live) => capture::TourKind::Live,
            (false, Capture::Tour) => capture::TourKind::Full,
        };
        capture::Tour::new(dir, kind)
    });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RillDisk")
            .with_app_id(APP_ID)
            .with_inner_size(size)
            .with_min_inner_size([1100.0, 680.0])
            .with_icon(Arc::new(style::icon())),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "RillDisk",
        options,
        Box::new(move |cc| Ok(Box::new(app::RillApp::new(cc, path, dark, identity, tour)))),
    )
}

/// Release builds use the GUI subsystem; borrow the launching terminal so
/// `--help` and `--version` output is visible.
fn attach_console() {
    #[cfg(windows)]
    // SAFETY: plain Win32 call with a documented constant; failure is harmless.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}
