// SPDX-License-Identifier: AGPL-3.0-only
//! RillDisk desktop: a native, GPU-rendered disk map built on the shared core.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod capture;
mod style;
mod treemap;

use eframe::egui;
use std::{path::PathBuf, sync::Arc};

fn main() -> eframe::Result {
    let mut path = None;
    let mut capture = None;
    let mut size = [1480.0_f32, 920.0];
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--capture" => capture = args.next().map(PathBuf::from),
            "--size" => {
                if let Some((w, h)) = args.next().as_deref().and_then(|s| s.split_once('x')) {
                    size = [w.parse().unwrap_or(size[0]), h.parse().unwrap_or(size[1])];
                }
            }
            "--help" | "-h" => {
                println!(
                    "rilldisk-desktop [PATH] [--size WIDTHxHEIGHT] [--capture DIR]\n\nOpens the RillDisk desktop app. PATH starts a scan immediately.\n--capture runs a scripted tour and saves screenshots to DIR (development)."
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
            .with_min_inner_size([960.0, 600.0])
            .with_icon(Arc::new(style::icon(64))),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "RillDisk",
        options,
        Box::new(move |cc| Ok(Box::new(app::RillApp::new(cc, path, tour)))),
    )
}
