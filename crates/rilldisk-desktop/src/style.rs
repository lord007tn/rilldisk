// SPDX-License-Identifier: AGPL-3.0-only
//! Palette, typography and small formatting helpers.
use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};
use rilldisk_core::Safety;

pub const BG: Color32 = Color32::from_rgb(11, 14, 19);
pub const PANEL: Color32 = Color32::from_rgb(16, 20, 27);
pub const SURFACE: Color32 = Color32::from_rgb(22, 27, 36);
pub const SURFACE_HI: Color32 = Color32::from_rgb(30, 37, 49);
pub const BORDER: Color32 = Color32::from_rgb(38, 46, 60);
pub const TEXT: Color32 = Color32::from_rgb(230, 234, 241);
pub const DIM: Color32 = Color32::from_rgb(140, 150, 167);
pub const FAINT: Color32 = Color32::from_rgb(92, 101, 118);
pub const ACCENT: Color32 = Color32::from_rgb(56, 189, 248);
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(14, 116, 178);
pub const GOOD: Color32 = Color32::from_rgb(52, 211, 153);
pub const INFO: Color32 = Color32::from_rgb(96, 165, 250);
pub const WARN: Color32 = Color32::from_rgb(251, 191, 36);
pub const DANGER: Color32 = Color32::from_rgb(248, 113, 113);
pub const DIR_FILL: Color32 = Color32::from_rgb(24, 30, 40);

pub fn safety_color(safety: Safety) -> Color32 {
    match safety {
        Safety::Regenerable => GOOD,
        Safety::Cache => INFO,
        Safety::Review => WARN,
    }
}

/// Coarse file classes used for treemap colors and the type legend.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FileClass {
    Video,
    Image,
    Audio,
    Archive,
    Binary,
    Code,
    Document,
    Data,
    Other,
}

impl FileClass {
    pub fn of(extension: &str) -> Self {
        match extension {
            "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" | "m4v" | "flv" | "mpg" | "mpeg"
            | "m2ts" | "prproj" => FileClass::Video,
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "bmp" | "tif" | "tiff" | "raw"
            | "cr2" | "nef" | "arw" | "dng" | "psd" | "svg" | "ico" | "exr" | "hdr" | "kra"
            | "blend" => FileClass::Image,
            "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "opus" | "wma" | "aiff" => {
                FileClass::Audio
            }
            "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "iso" | "cab"
            | "dmg" | "nupkg" | "crate" | "whl" | "jar" | "apk" | "appx" | "msix" => {
                FileClass::Archive
            }
            "exe" | "dll" | "msi" | "sys" | "bin" | "so" | "dylib" | "pdb" | "lib" | "obj"
            | "o" | "a" | "rlib" | "rmeta" | "node" | "pak" | "wasm" | "pyd" | "ilk" | "exp" => {
                FileClass::Binary
            }
            "rs" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "py" | "json" | "toml"
            | "yaml" | "yml" | "html" | "css" | "scss" | "c" | "cc" | "cpp" | "h" | "hpp"
            | "go" | "java" | "kt" | "cs" | "swift" | "rb" | "php" | "lua" | "sh" | "ps1"
            | "vue" | "svelte" | "map" | "lock" | "xml" | "sql" | "md" | "pyc" | "class" => {
                FileClass::Code
            }
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "rtf" | "odt"
            | "epub" | "csv" | "pages" | "key" | "numbers" => FileClass::Document,
            "db" | "sqlite" | "sqlite3" | "mdb" | "vhd" | "vhdx" | "vmdk" | "qcow2" | "vdi"
            | "log" | "dat" | "parquet" | "gguf" | "safetensors" | "onnx" | "pt" | "ckpt"
            | "npy" | "h5" | "bak" | "tmp" | "etl" | "dmp" | "cache" | "idx" | "pack" => {
                FileClass::Data
            }
            _ => FileClass::Other,
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            FileClass::Video => Color32::from_rgb(167, 139, 250),
            FileClass::Image => Color32::from_rgb(244, 114, 182),
            FileClass::Audio => Color32::from_rgb(251, 146, 60),
            FileClass::Archive => Color32::from_rgb(234, 179, 8),
            FileClass::Binary => Color32::from_rgb(239, 68, 68),
            FileClass::Code => Color32::from_rgb(45, 212, 191),
            FileClass::Document => Color32::from_rgb(59, 130, 246),
            FileClass::Data => Color32::from_rgb(129, 140, 248),
            FileClass::Other => Color32::from_rgb(100, 116, 139),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FileClass::Video => "Video",
            FileClass::Image => "Images",
            FileClass::Audio => "Audio",
            FileClass::Archive => "Archives",
            FileClass::Binary => "Binaries",
            FileClass::Code => "Code & text",
            FileClass::Document => "Documents",
            FileClass::Data => "Data & VMs",
            FileClass::Other => "Other",
        }
    }
}

pub fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 17 => name[dot + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

pub fn scale(color: Color32, factor: f32) -> Color32 {
    let f = |c: u8| ((c as f32) * factor).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(color.r()), f(color.g()), f(color.b()))
}

pub fn alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// Explorer-style sizes (1024-based, short unit names).
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if value < 1024 {
        return format!("{value} B");
    }
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if size >= 100.0 {
        format!("{size:.0} {}", UNITS[unit])
    } else if size >= 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.2} {}", UNITS[unit])
    }
}

pub fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn age(now_ms: u64, modified_ms: Option<u64>) -> String {
    let Some(modified) = modified_ms else {
        return "—".into();
    };
    if modified > now_ms + 60_000 {
        return "future".into();
    }
    let days = now_ms.saturating_sub(modified) / 86_400_000;
    match days {
        0 => "today".into(),
        1..=59 => format!("{days} d"),
        60..=729 => format!("{} mo", days / 30),
        _ => format!("{} y", days / 365),
    }
}

pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = SURFACE;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = alpha(ACCENT, 70);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(8);
    let radius = CornerRadius::same(6);
    let w = &mut visuals.widgets;
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.weak_bg_fill = SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, DIM);
    w.inactive.bg_fill = SURFACE_HI;
    w.inactive.weak_bg_fill = SURFACE_HI;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.bg_fill = Color32::from_rgb(40, 49, 64);
    w.hovered.weak_bg_fill = Color32::from_rgb(40, 49, 64);
    w.hovered.bg_stroke = Stroke::new(1.0, alpha(ACCENT, 140));
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.active.bg_fill = ACCENT_DEEP;
    w.active.weak_bg_fill = ACCENT_DEEP;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.open.weak_bg_fill = SURFACE_HI;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.corner_radius = radius;
    }
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.text_styles = [
            (
                TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(13.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Heading,
                FontId::new(20.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}

/// Procedural app icon: a rounded tile with four treemap blocks.
pub fn icon(size: u32) -> egui::IconData {
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    let s = size as f32;
    let blocks: [(f32, f32, f32, f32, Color32); 4] = [
        (0.16, 0.44, 0.58, 0.84, ACCENT),
        (0.62, 0.16, 0.84, 0.50, FileClass::Video.color()),
        (0.62, 0.54, 0.84, 0.84, GOOD),
        (0.16, 0.16, 0.58, 0.40, Color32::from_rgb(125, 211, 252)),
    ];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
            let r = 0.22;
            let (cx, cy) = (fx.clamp(r, 1.0 - r), fy.clamp(r, 1.0 - r));
            let inside = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() <= r;
            if !inside {
                continue;
            }
            let mut color = Color32::from_rgb(15, 23, 36);
            for (x0, y0, x1, y1, c) in blocks {
                if fx >= x0 && fx <= x1 && fy >= y0 && fy <= y1 {
                    color = c;
                }
            }
            let i = ((y * size + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[color.r(), color.g(), color.b(), 255]);
        }
    }
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}
