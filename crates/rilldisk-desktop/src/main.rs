// SPDX-License-Identifier: AGPL-3.0-only
//! RillDisk desktop: a native, GPU-rendered disk map built on the shared core.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assistants;
mod capture;
mod cleanup_ui;
mod elevation;
mod icons;
mod live;
mod reveal;
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
    #[cfg(windows)]
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(rilldisk_update::msi::HELPER_ARG))
    {
        let result = std::env::args_os()
            .nth(2)
            .map(PathBuf::from)
            .ok_or_else(|| "Installer request is missing".to_string())
            .and_then(|path| rilldisk_update::msi::run_helper(&path).map_err(|e| format!("{e:#}")));
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let mut path = None;
    let mut capture = None;
    let mut tour_name = None;
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
            "--capture-live" => {
                capture = value(&mut args).map(|d| (PathBuf::from(d), Capture::Live))
            }
            "--capture-cleanup" => {
                capture = value(&mut args).map(|d| (PathBuf::from(d), Capture::Cleanup))
            }
            "--tour" => tour_name = value(&mut args),
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
                    "rilldisk-desktop [PATH] [--theme light|dark] [--palette tide|moss|aurora] [--size WIDTHxHEIGHT] [--capture DIR [--tour issues|navigation|updates]] [--capture-cleanup DIR]\n\nOpens the RillDisk desktop app. PATH starts a scan immediately.\n--capture runs a scripted tour and saves screenshots to DIR (development).\n--capture-cleanup captures the cleanup dialogs; it only removes folders from a synthetic fixture.\n--version prints the version."
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
    let navigation_tour = tour_name.as_deref() == Some("navigation");
    let updates_tour = tour_name.as_deref() == Some("updates");
    let issues_tour = match tour_name.as_deref() {
        None => false,
        Some("updates") if path.is_some() => {
            attach_console();
            eprintln!("--tour updates runs with --capture DIR, without PATH.");
            return Ok(());
        }
        Some("updates") if matches!(capture, Some((_, Capture::Tour))) => false,
        Some("navigation") if path.is_some() && matches!(capture, Some((_, Capture::Tour))) => {
            false
        }
        // Named tours replace the --capture tour only; --capture-live and
        // --capture-cleanup run their own steps.
        Some("issues") if !matches!(capture, Some((_, Capture::Tour))) => {
            attach_console();
            eprintln!(
                "--tour issues runs with --capture DIR, not alone or with --capture-live or --capture-cleanup."
            );
            return Ok(());
        }
        Some("issues") if path.is_some() => true,
        Some("issues") => {
            // The issues tour waits for scan data, so without a PATH it would never end.
            attach_console();
            eprintln!("--tour issues needs a PATH to scan.");
            return Ok(());
        }
        Some(other) => {
            attach_console();
            eprintln!(
                "invalid --tour {other}; use `issues` or `navigation` with PATH --capture DIR, or `updates` with --capture DIR."
            );
            return Ok(());
        }
    };
    let tour = capture.map(|(dir, mode)| {
        if updates_tour {
            return capture::Tour::updates(dir);
        }
        let kind = match (path.is_none(), mode) {
            (false, Capture::Tour) if navigation_tour => return capture::Tour::navigation(dir),
            (_, Capture::Cleanup) => return capture::Tour::cleanup(dir),
            (true, _) => capture::TourKind::Welcome,
            (false, _) if issues_tour => capture::TourKind::Issues,
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
