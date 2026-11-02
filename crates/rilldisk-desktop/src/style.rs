// SPDX-License-Identifier: AGPL-3.0-only
//! Design tokens: two themes, fonts, chart palettes and formatting helpers.
//!
//! Surfaces are separated by tone and spacing instead of outlines. Every color the
//! UI paints comes from [`t()`], so switching themes is a single atomic flip.
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Shadow, Stroke,
    TextStyle, Visuals,
    epaint::text::{FontTweak, VariationCoords},
};
use rilldisk_core::Safety;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

pub struct Theme {
    pub dark: bool,
    /// Content background.
    pub bg: Color32,
    pub sidebar: Color32,
    /// Cards sitting on `bg`.
    pub card: Color32,
    pub hover: Color32,
    pub press: Color32,
    /// Text fields and progress tracks.
    pub field: Color32,
    pub popup: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub good: Color32,
    pub info: Color32,
    pub warn: Color32,
    pub danger: Color32,
    /// Ink for labels drawn on chart tiles.
    pub ink: Color32,
    pub shadow: Color32,
    stops: [Color32; 6],
    classes: [Color32; 9],
}

static TIDE_DARK: Theme = Theme {
    dark: true,
    bg: Color32::from_rgb(11, 16, 24),
    sidebar: Color32::from_rgb(8, 12, 19),
    card: Color32::from_rgb(19, 27, 38),
    hover: Color32::from_rgb(28, 39, 54),
    press: Color32::from_rgb(36, 49, 67),
    field: Color32::from_rgb(15, 22, 32),
    popup: Color32::from_rgb(24, 33, 47),
    text: Color32::from_rgb(232, 240, 247),
    dim: Color32::from_rgb(143, 160, 182),
    faint: Color32::from_rgb(88, 104, 126),
    accent: Color32::from_rgb(48, 222, 196),
    on_accent: Color32::from_rgb(3, 28, 26),
    good: Color32::from_rgb(74, 222, 128),
    info: Color32::from_rgb(96, 165, 250),
    warn: Color32::from_rgb(251, 191, 36),
    danger: Color32::from_rgb(248, 113, 113),
    ink: Color32::from_rgb(255, 255, 255),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 110),
    stops: [
        Color32::from_rgb(32, 201, 170),
        Color32::from_rgb(34, 190, 234),
        Color32::from_rgb(59, 141, 247),
        Color32::from_rgb(99, 102, 241),
        Color32::from_rgb(139, 92, 246),
        Color32::from_rgb(192, 132, 252),
    ],
    classes: [
        Color32::from_rgb(139, 110, 246),
        Color32::from_rgb(232, 121, 190),
        Color32::from_rgb(245, 160, 72),
        Color32::from_rgb(230, 192, 52),
        Color32::from_rgb(240, 98, 110),
        Color32::from_rgb(45, 200, 180),
        Color32::from_rgb(64, 150, 250),
        Color32::from_rgb(99, 112, 241),
        Color32::from_rgb(100, 116, 139),
    ],
};

static TIDE_LIGHT: Theme = Theme {
    dark: false,
    bg: Color32::from_rgb(248, 250, 252),
    sidebar: Color32::from_rgb(240, 244, 248),
    card: Color32::from_rgb(235, 241, 246),
    hover: Color32::from_rgb(225, 233, 240),
    press: Color32::from_rgb(213, 223, 232),
    field: Color32::from_rgb(229, 236, 242),
    popup: Color32::from_rgb(255, 255, 255),
    text: Color32::from_rgb(15, 23, 32),
    dim: Color32::from_rgb(84, 100, 120),
    faint: Color32::from_rgb(140, 154, 172),
    accent: Color32::from_rgb(13, 148, 136),
    on_accent: Color32::from_rgb(255, 255, 255),
    good: Color32::from_rgb(22, 150, 74),
    info: Color32::from_rgb(37, 99, 235),
    warn: Color32::from_rgb(190, 128, 6),
    danger: Color32::from_rgb(214, 45, 45),
    ink: Color32::from_rgb(16, 34, 44),
    shadow: Color32::from_rgba_premultiplied(1, 2, 3, 30),
    stops: [
        Color32::from_rgb(153, 230, 210),
        Color32::from_rgb(150, 218, 240),
        Color32::from_rgb(170, 200, 250),
        Color32::from_rgb(192, 190, 248),
        Color32::from_rgb(214, 192, 246),
        Color32::from_rgb(238, 198, 228),
    ],
    classes: [
        Color32::from_rgb(200, 190, 246),
        Color32::from_rgb(244, 190, 222),
        Color32::from_rgb(250, 205, 160),
        Color32::from_rgb(246, 226, 150),
        Color32::from_rgb(246, 176, 182),
        Color32::from_rgb(160, 226, 210),
        Color32::from_rgb(170, 204, 248),
        Color32::from_rgb(186, 194, 246),
        Color32::from_rgb(208, 214, 222),
    ],
};

static MOSS_DARK: Theme = Theme {
    dark: true,
    bg: Color32::from_rgb(16, 18, 16),
    sidebar: Color32::from_rgb(11, 13, 11),
    card: Color32::from_rgb(25, 29, 25),
    hover: Color32::from_rgb(35, 40, 35),
    press: Color32::from_rgb(44, 50, 44),
    field: Color32::from_rgb(21, 24, 21),
    popup: Color32::from_rgb(32, 37, 32),
    text: Color32::from_rgb(236, 240, 230),
    dim: Color32::from_rgb(158, 168, 150),
    faint: Color32::from_rgb(100, 110, 94),
    accent: Color32::from_rgb(190, 242, 100),
    on_accent: Color32::from_rgb(24, 34, 6),
    good: Color32::from_rgb(134, 239, 172),
    info: Color32::from_rgb(125, 211, 252),
    warn: Color32::from_rgb(250, 204, 21),
    danger: Color32::from_rgb(252, 129, 129),
    ink: Color32::from_rgb(255, 255, 255),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 110),
    stops: [
        Color32::from_rgb(160, 220, 80),
        Color32::from_rgb(84, 200, 112),
        Color32::from_rgb(40, 180, 152),
        Color32::from_rgb(48, 150, 186),
        Color32::from_rgb(200, 176, 96),
        Color32::from_rgb(226, 140, 72),
    ],
    classes: [
        Color32::from_rgb(150, 130, 230),
        Color32::from_rgb(226, 120, 170),
        Color32::from_rgb(236, 160, 72),
        Color32::from_rgb(222, 190, 60),
        Color32::from_rgb(232, 100, 96),
        Color32::from_rgb(84, 200, 112),
        Color32::from_rgb(72, 150, 220),
        Color32::from_rgb(120, 130, 220),
        Color32::from_rgb(110, 120, 104),
    ],
};

static MOSS_LIGHT: Theme = Theme {
    dark: false,
    bg: Color32::from_rgb(249, 249, 245),
    sidebar: Color32::from_rgb(241, 242, 236),
    card: Color32::from_rgb(236, 238, 230),
    hover: Color32::from_rgb(226, 229, 219),
    press: Color32::from_rgb(214, 218, 206),
    field: Color32::from_rgb(230, 233, 224),
    popup: Color32::from_rgb(255, 255, 255),
    text: Color32::from_rgb(24, 28, 20),
    dim: Color32::from_rgb(96, 104, 88),
    faint: Color32::from_rgb(150, 156, 140),
    accent: Color32::from_rgb(84, 134, 22),
    on_accent: Color32::from_rgb(255, 255, 255),
    good: Color32::from_rgb(22, 150, 74),
    info: Color32::from_rgb(37, 99, 235),
    warn: Color32::from_rgb(190, 128, 6),
    danger: Color32::from_rgb(214, 45, 45),
    ink: Color32::from_rgb(30, 36, 22),
    shadow: Color32::from_rgba_premultiplied(2, 2, 1, 30),
    stops: [
        Color32::from_rgb(210, 238, 160),
        Color32::from_rgb(176, 226, 180),
        Color32::from_rgb(160, 220, 204),
        Color32::from_rgb(168, 208, 226),
        Color32::from_rgb(234, 220, 170),
        Color32::from_rgb(244, 198, 160),
    ],
    classes: [
        Color32::from_rgb(206, 196, 240),
        Color32::from_rgb(240, 194, 218),
        Color32::from_rgb(248, 208, 164),
        Color32::from_rgb(240, 224, 150),
        Color32::from_rgb(242, 182, 176),
        Color32::from_rgb(176, 226, 180),
        Color32::from_rgb(172, 206, 236),
        Color32::from_rgb(190, 196, 236),
        Color32::from_rgb(214, 216, 206),
    ],
};

static AURORA_DARK: Theme = Theme {
    dark: true,
    bg: Color32::from_rgb(12, 12, 15),
    sidebar: Color32::from_rgb(9, 9, 11),
    card: Color32::from_rgb(23, 23, 28),
    hover: Color32::from_rgb(33, 33, 40),
    press: Color32::from_rgb(42, 42, 51),
    field: Color32::from_rgb(19, 19, 23),
    popup: Color32::from_rgb(30, 30, 36),
    text: Color32::from_rgb(240, 240, 245),
    dim: Color32::from_rgb(161, 161, 176),
    faint: Color32::from_rgb(103, 103, 118),
    accent: Color32::from_rgb(129, 140, 248),
    on_accent: Color32::from_rgb(10, 10, 30),
    good: Color32::from_rgb(74, 222, 128),
    info: Color32::from_rgb(96, 165, 250),
    warn: Color32::from_rgb(251, 191, 36),
    danger: Color32::from_rgb(248, 113, 113),
    ink: Color32::from_rgb(255, 255, 255),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 110),
    stops: [
        Color32::from_rgb(52, 211, 153),
        Color32::from_rgb(34, 211, 238),
        Color32::from_rgb(96, 165, 250),
        Color32::from_rgb(129, 140, 248),
        Color32::from_rgb(192, 132, 252),
        Color32::from_rgb(240, 150, 240),
    ],
    classes: [
        Color32::from_rgb(139, 110, 246),
        Color32::from_rgb(232, 121, 190),
        Color32::from_rgb(245, 160, 72),
        Color32::from_rgb(230, 192, 52),
        Color32::from_rgb(240, 98, 110),
        Color32::from_rgb(45, 200, 180),
        Color32::from_rgb(64, 150, 250),
        Color32::from_rgb(99, 112, 241),
        Color32::from_rgb(100, 116, 139),
    ],
};

static AURORA_LIGHT: Theme = Theme {
    dark: false,
    bg: Color32::from_rgb(255, 255, 255),
    sidebar: Color32::from_rgb(247, 247, 249),
    card: Color32::from_rgb(243, 243, 246),
    hover: Color32::from_rgb(234, 234, 239),
    press: Color32::from_rgb(222, 222, 229),
    field: Color32::from_rgb(238, 238, 242),
    popup: Color32::from_rgb(255, 255, 255),
    text: Color32::from_rgb(17, 17, 22),
    dim: Color32::from_rgb(99, 99, 112),
    faint: Color32::from_rgb(156, 156, 168),
    accent: Color32::from_rgb(79, 70, 229),
    on_accent: Color32::from_rgb(255, 255, 255),
    good: Color32::from_rgb(22, 150, 74),
    info: Color32::from_rgb(37, 99, 235),
    warn: Color32::from_rgb(190, 128, 6),
    danger: Color32::from_rgb(214, 45, 45),
    ink: Color32::from_rgb(24, 24, 40),
    shadow: Color32::from_rgba_premultiplied(1, 1, 3, 30),
    stops: [
        Color32::from_rgb(167, 236, 206),
        Color32::from_rgb(165, 228, 244),
        Color32::from_rgb(180, 208, 252),
        Color32::from_rgb(198, 200, 252),
        Color32::from_rgb(222, 198, 252),
        Color32::from_rgb(246, 204, 246),
    ],
    classes: [
        Color32::from_rgb(200, 190, 246),
        Color32::from_rgb(244, 190, 222),
        Color32::from_rgb(250, 205, 160),
        Color32::from_rgb(246, 226, 150),
        Color32::from_rgb(246, 176, 182),
        Color32::from_rgb(160, 226, 210),
        Color32::from_rgb(170, 204, 248),
        Color32::from_rgb(186, 194, 246),
        Color32::from_rgb(208, 214, 222),
    ],
};

/// A color identity. Each has a paper (light) and a night (dark) variant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Identity {
    /// Water: aqua on deep ink. The RillDisk brand.
    Tide,
    /// Lichen green on graphite.
    Moss,
    /// Neutral zinc with an indigo accent and aurora charts.
    Aurora,
}

impl Identity {
    pub const ALL: [Identity; 3] = [Identity::Tide, Identity::Moss, Identity::Aurora];

    pub fn name(self) -> &'static str {
        match self {
            Identity::Tide => "Tide",
            Identity::Moss => "Moss",
            Identity::Aurora => "Aurora",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|i| i.name().eq_ignore_ascii_case(text))
    }

    pub fn theme(self, dark: bool) -> &'static Theme {
        match (self, dark) {
            (Identity::Tide, false) => &TIDE_LIGHT,
            (Identity::Tide, true) => &TIDE_DARK,
            (Identity::Moss, false) => &MOSS_LIGHT,
            (Identity::Moss, true) => &MOSS_DARK,
            (Identity::Aurora, false) => &AURORA_LIGHT,
            (Identity::Aurora, true) => &AURORA_DARK,
        }
    }
}

static IS_DARK: AtomicBool = AtomicBool::new(false);
static IDENTITY: AtomicU8 = AtomicU8::new(0);

/// The active color identity.
pub fn identity() -> Identity {
    Identity::ALL[IDENTITY.load(Ordering::Relaxed) as usize]
}

/// The active theme.
pub fn t() -> &'static Theme {
    Identity::ALL[IDENTITY.load(Ordering::Relaxed) as usize].theme(IS_DARK.load(Ordering::Relaxed))
}

/// Chart gradient stops of the active theme (for the logo and swatches).
pub fn stops() -> &'static [Color32; 6] {
    &t().stops
}

pub fn safety_color(safety: Safety) -> Color32 {
    match safety {
        Safety::Regenerable => t().good,
        Safety::Cache => t().info,
        Safety::Review => t().warn,
    }
}

/// Continuous chart palette: `x` in 0..1 sweeps the gradient, `depth` shades it.
pub fn chart(x: f32, depth: u16) -> Color32 {
    let stops = &t().stops;
    let x = x.clamp(0.0, 0.9999) * (stops.len() - 1) as f32;
    let i = x as usize;
    let base = mix(stops[i], stops[(i + 1).min(stops.len() - 1)], x - i as f32);
    shade(base, depth)
}

/// Deeper levels: slightly richer on paper, slightly lighter at night.
pub fn shade(base: Color32, depth: u16) -> Color32 {
    let d = depth as f32;
    if t().dark {
        mix(base, Color32::WHITE, (d * 0.07).min(0.4))
    } else {
        mix(base, scale(base, 0.80), (d * 0.24).min(0.9))
    }
}

/// Coarse file classes used for chart colors and the type views.
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
        t().classes[self as usize]
    }

    pub fn label(self) -> &'static str {
        match self {
            FileClass::Video => "Video",
            FileClass::Image => "Images",
            FileClass::Audio => "Audio",
            FileClass::Archive => "Archives",
            FileClass::Binary => "Programs",
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

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

pub fn alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

// ------------------------------------------------------------------ typography

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// The platform UI typeface as a regular and a semibold face. Windows uses Segoe UI
/// Variable, macOS San Francisco and Linux the first of Inter, Cantarell, Ubuntu Sans,
/// Noto Sans and DejaVu Sans that is installed. egui's bundled fonts stay as the fallback for
/// missing glyphs and for systems without any of them; a file egui cannot parse is
/// skipped with a warning by egui itself.
fn fonts() -> FontDefinitions {
    use std::path::{Path, PathBuf};
    let mut defs = FontDefinitions::default();
    let fallback = defs.families[&FontFamily::Proportional].clone();
    // One variable font serves both weights through its `wght` axis.
    let variable = |bytes: Vec<u8>, opsz: Option<f32>| {
        let blob: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
        let face = |weight: f32| {
            let mut coords = VariationCoords::new([(*b"wght", weight)]);
            if let Some(size) = opsz {
                coords.push(*b"opsz", size);
            }
            FontData::from_blob(blob.clone(), 0).tweak(FontTweak {
                coords,
                ..Default::default()
            })
        };
        vec![("ui", face(400.0)), ("ui-semibold", face(600.0))]
    };
    let pair = |regular: Vec<u8>, semibold: Vec<u8>| {
        vec![
            ("ui", FontData::from_owned(regular)),
            ("ui-semibold", FontData::from_owned(semibold)),
        ]
    };
    let read = |path: &Path| std::fs::read(path).ok();
    let mut faces: Vec<(&str, FontData)> = Vec::new();
    if cfg!(windows) {
        let dir = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()))
            .join("Fonts");
        if let Some(bytes) = read(&dir.join("SegUIVar.ttf")) {
            faces = variable(bytes, Some(10.5));
        } else if let (Some(regular), Some(bold)) = (
            read(&dir.join("segoeui.ttf")),
            read(&dir.join("seguisb.ttf")),
        ) {
            faces = pair(regular, bold);
        }
    } else if cfg!(target_os = "macos") {
        // San Francisco ships as one variable TrueType font; text optical size.
        if let Some(bytes) = read(Path::new("/System/Library/Fonts/SFNS.ttf")) {
            faces = variable(bytes, Some(13.0));
        }
    } else {
        // Index font files by lowercase name under the usual fontconfig directories.
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        let mut stack: Vec<(PathBuf, u8)> = ["/usr/share/fonts", "/usr/local/share/fonts"]
            .into_iter()
            .map(PathBuf::from)
            .chain(data.map(|d| d.join("fonts")))
            .chain(home.map(|h| h.join(".fonts")))
            .map(|dir| (dir, 0))
            .collect();
        let mut files = std::collections::HashMap::new();
        while let Some((dir, depth)) = stack.pop() {
            for item in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                match item.file_type() {
                    Ok(kind) if kind.is_dir() => {
                        if depth < 4 {
                            stack.push((item.path(), depth + 1));
                        }
                    }
                    Ok(_) => {
                        let name = item.file_name().to_string_lossy().to_lowercase();
                        files.entry(name).or_insert_with(|| item.path());
                    }
                    Err(_) => {}
                }
            }
        }
        // (variable TrueType files, static regular files, static semibold/bold files),
        // in order of preference. A variable file wins over a static pair of the same
        // family. CFF2 variable fonts (`*-VF.otf`) are left out on purpose.
        const FAMILIES: [(&[&str], &[&str], &[&str]); 5] = [
            (
                &[
                    "intervariable.ttf",
                    "inter[opsz,wght].ttf",
                    "inter-variable.ttf",
                ],
                &["inter-regular.ttf", "inter-regular.otf"],
                &["inter-semibold.ttf", "inter-semibold.otf"],
            ),
            (
                &[],
                &["cantarell-regular.otf", "cantarell-regular.ttf"],
                &["cantarell-bold.otf", "cantarell-bold.ttf"],
            ),
            // Ubuntu's UI font since 24.04, then the classic Ubuntu font.
            (
                &["ubuntusans[wdth,wght].ttf", "ubuntu[wdth,wght].ttf"],
                &["ubuntu-r.ttf"],
                &["ubuntu-m.ttf"],
            ),
            (
                &["notosans[wght].ttf", "notosans[wdth,wght].ttf"],
                &["notosans-regular.ttf"],
                &["notosans-semibold.ttf", "notosans-bold.ttf"],
            ),
            (&[], &["dejavusans.ttf"], &["dejavusans-bold.ttf"]),
        ];
        let load = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| files.get(*name).and_then(|path| read(path)))
        };
        for (variable_names, regular, semibold) in FAMILIES {
            if let Some(bytes) = load(variable_names) {
                faces = variable(bytes, None);
                break;
            }
            if let Some(semibold) = load(semibold)
                && let Some(regular) = load(regular)
            {
                faces = pair(regular, semibold);
                break;
            }
        }
    }
    let mut semibold = fallback.clone();
    for (name, data) in faces {
        defs.font_data.insert(name.into(), Arc::new(data));
        if name == "ui" {
            defs.families
                .get_mut(&FontFamily::Proportional)
                .unwrap()
                .insert(0, name.into());
        } else {
            semibold.insert(0, name.into());
        }
    }
    defs.families
        .insert(FontFamily::Name("semibold".into()), semibold);
    defs
}

pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
}

pub fn apply(ctx: &egui::Context, identity: Identity, dark: bool) {
    IS_DARK.store(dark, Ordering::Relaxed);
    IDENTITY.store(
        Identity::ALL
            .iter()
            .position(|&i| i == identity)
            .unwrap_or(0) as u8,
        Ordering::Relaxed,
    );
    let th = t();
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = th.bg;
    v.window_fill = th.popup;
    v.extreme_bg_color = th.field;
    v.text_edit_bg_color = Some(th.field);
    v.faint_bg_color = th.card;
    v.code_bg_color = th.field;
    v.override_text_color = Some(th.text);
    v.weak_text_color = Some(th.dim);
    v.hyperlink_color = th.accent;
    v.warn_fg_color = th.warn;
    v.error_fg_color = th.danger;
    v.selection.bg_fill = alpha(th.accent, 70);
    v.selection.stroke = Stroke::new(1.5, th.accent);
    v.window_stroke = Stroke::NONE;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: th.shadow,
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 22,
        spread: 0,
        color: th.shadow,
    };
    v.indent_has_left_vline = false;
    v.striped = false;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    let radius = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = th.card;
    w.noninteractive.weak_bg_fill = th.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, alpha(th.faint, 40));
    w.noninteractive.fg_stroke = Stroke::new(1.0, th.dim);
    w.inactive.bg_fill = th.field;
    w.inactive.weak_bg_fill = th.card;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, th.text);
    w.hovered.bg_fill = th.hover;
    w.hovered.weak_bg_fill = th.hover;
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.0, th.text);
    w.hovered.expansion = 0.0;
    w.active.bg_fill = th.press;
    w.active.weak_bg_fill = th.press;
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.0, th.text);
    w.active.expansion = 0.0;
    w.open.bg_fill = th.hover;
    w.open.weak_bg_fill = th.hover;
    w.open.bg_stroke = Stroke::NONE;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.corner_radius = radius;
    }
    ctx.set_visuals(v);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.scroll = egui::style::ScrollStyle::floating();
        style.spacing.scroll.bar_width = 8.0;
        style.text_styles = [
            (TextStyle::Small, regular(11.5)),
            (TextStyle::Body, regular(13.5)),
            (TextStyle::Button, regular(13.5)),
            (TextStyle::Heading, semibold(22.0)),
            (TextStyle::Monospace, mono(12.5)),
        ]
        .into();
    });
}

// ------------------------------------------------------------------ formatting

/// Explorer-style sizes (1024-based, short unit names).
pub fn bytes(value: u64) -> String {
    let (number, unit) = bytes_parts(value);
    format!("{number} {unit}")
}

/// Number and unit separately, for big figures with a lighter unit.
pub fn bytes_parts(value: u64) -> (String, &'static str) {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if value < 1024 {
        return (value.to_string(), "B");
    }
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    let number = if size >= 100.0 {
        format!("{size:.0}")
    } else if size >= 10.0 {
        format!("{size:.1}")
    } else {
        format!("{size:.2}")
    };
    (number, UNITS[unit])
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

/// Compact counts for tight places: 1.2M, 34k.
pub fn short_count(value: u64) -> String {
    match value {
        0..=9_999 => count(value),
        10_000..=999_999 => format!("{}k", value / 1000),
        _ => format!("{:.1}M", value as f64 / 1e6),
    }
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
        1 => "yesterday".into(),
        2..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

pub fn plural(n: u64, word: &str) -> String {
    format!("{} {word}{}", count(n), if n == 1 { "" } else { "s" })
}

/// The app icon (packaging/icons, rendered by scripts/make_icons.py), shown in
/// the title bar and taskbar.
pub fn icon() -> egui::IconData {
    let png = include_bytes!("../../../packaging/icons/rilldisk-256.png");
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("bundled icon is a valid PNG")
        .into_rgba8();
    egui::IconData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    }
}
