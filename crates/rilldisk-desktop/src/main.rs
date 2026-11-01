// SPDX-License-Identifier: AGPL-3.0-only
//! RillDisk desktop: a native, GPU-rendered disk map built on the shared core.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod capture;
mod icons;
mod style;
mod sunburst;
mod treemap;
// Update API for the UI to call; not wired into the interface yet.
#[allow(dead_code)]
mod update;
mod widgets;

use eframe::egui;
use std::{path::PathBuf, sync::Arc};

fn main() -> eframe::Result {
    let mut path = None;
    let mut capture = None;
    let mut dark = None;
    let mut identity = style::Identity::Tide;
    let mut shell = app::Shell::Dashboard;
    let mut size = [1480.0_f32, 920.0];
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--capture" => capture = args.next().map(PathBuf::from),
            "--theme" => dark = args.next().map(|theme| theme == "dark"),
            "--layout" => {
                if let Some(found) = args.next().as_deref().and_then(app::Shell::parse) {
                    shell = found;
                }
            }
            "--palette" => {
                if let Some(found) = args.next().as_deref().and_then(style::Identity::parse) {
                    identity = found;
                }
            }
            "--size" => {
                if let Some((w, h)) = args.next().as_deref().and_then(|s| s.split_once('x')) {
                    size = [w.parse().unwrap_or(size[0]), h.parse().unwrap_or(size[1])];
                }
            }
            "--help" | "-h" => {
                // Release builds use the GUI subsystem; borrow the launching
                // terminal so the usage text is visible.
                #[cfg(windows)]
                // SAFETY: plain Win32 call with a documented constant; failure is harmless.
                unsafe {
                    windows_sys::Win32::System::Console::AttachConsole(
                        windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
                    );
                }
                println!(
                    "rilldisk-desktop [PATH] [--theme light|dark] [--palette tide|moss|aurora] [--layout dashboard|canvas|triage] [--size WIDTHxHEIGHT] [--capture DIR]\n\nOpens the RillDisk desktop app. PATH starts a scan immediately.\n--capture runs a scripted tour and saves screenshots to DIR (development)."
                );
                return Ok(());
            }
            _ => path = Some(arg),
        }
    }
    let tour = capture.map(|dir| capture::Tour::new(dir, path.is_none()));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RillDisk")
            .with_app_id("rilldisk")
            .with_inner_size(size)
            .with_min_inner_size([1100.0, 680.0])
            .with_icon(Arc::new(style::icon(64))),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "RillDisk",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::RillApp::new(
                cc, path, dark, identity, shell, tour,
            )))
        }),
    )
}
