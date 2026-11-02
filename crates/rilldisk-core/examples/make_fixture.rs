// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic synthetic tree for scanner benchmarks and backend comparisons.
//!
//! ```text
//! cargo run --release -p rilldisk-core --example make_fixture -- DIR [--files N] [--seed S]
//! cargo run --release -p rilldisk-core --example make_fixture -- --remove DIR
//! ```
//!
//! The shape follows a developer drive, where scans are slowest: many projects,
//! each with a deep `node_modules` (half in the pnpm `.pnpm/<pkg>@<ver>/node_modules`
//! layout) of small folders holding a few small files, hard links into a shared
//! content store, a handful of large files, a sparse file, a directory link that
//! must not be followed, names with Unicode, trailing dots and spaces, a path longer
//! than `MAX_PATH`, and one folder the current user cannot list.
//!
//! File sizes are set with `set_len`, so no data is written: sizes and allocation
//! are real, contents are zero. The default (1,000,000 files) reserves about
//! 2.5 GB. A manifest with the expected totals is printed to stdout (JSON) and
//! saved next to DIR as `DIR.manifest.json`, outside the scanned tree.
//! Nothing here comes from a real machine.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use rayon::prelude::*;

/// splitmix64: tiny, deterministic and good enough for layout decisions.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    /// Mostly tiny files, as in dependency trees: 45% fit in the MFT record on NTFS.
    fn file_size(&mut self) -> u64 {
        match self.below(100) {
            0..45 => self.below(600),
            45..90 => 600 + self.below(2_400),
            90..99 => 3_000 + self.below(13_000),
            _ => 16_000 + self.below(112_000),
        }
    }
}

const SYLLABLES: &[&str] = &[
    "ba", "ko", "ri", "zu", "mel", "dor", "fin", "ga", "lu", "ne", "pix", "sto", "tra", "vel",
    "wo", "xen", "yo", "qui", "ham", "jet",
];
const FILE_NAMES: &[&str] = &[
    "index.js",
    "index.d.ts",
    "package.json",
    "README.md",
    "LICENSE",
    "CHANGELOG.md",
    "utils.js",
    "types.d.ts",
    "main.cjs",
    "module.mjs",
    "index.js.map",
    "helpers.js",
];

fn word(rng: &mut Rng, parts: u64) -> String {
    (0..1 + rng.below(parts))
        .map(|_| SYLLABLES[rng.below(SYLLABLES.len() as u64) as usize])
        .collect()
}

#[derive(Default)]
struct Plan {
    dirs: Vec<PathBuf>,
    index: std::collections::HashMap<PathBuf, u32>,
    /// (directory index, name, size, hard link target file index)
    files: Vec<(u32, String, u64, Option<u32>)>,
    store: Vec<u32>,
}

impl Plan {
    fn dir(&mut self, path: PathBuf) -> u32 {
        if let Some(&index) = self.index.get(&path) {
            return index;
        }
        self.dirs.push(path.clone());
        let index = (self.dirs.len() - 1) as u32;
        self.index.insert(path, index);
        index
    }
    fn file(&mut self, dir: u32, name: String, size: u64, link: Option<u32>) -> u32 {
        self.files.push((dir, name, size, link));
        (self.files.len() - 1) as u32
    }
    /// One package folder: a few files, often `lib/` or `dist/{esm,cjs}`. Files are
    /// hard links into the store 15% of the time (pnpm, bun).
    fn package(&mut self, rng: &mut Rng, path: PathBuf, budget: &mut i64, depth: u32) {
        let root = self.dir(path.clone());
        let mut folders = vec![root];
        if rng.chance(55) {
            folders.push(self.dir(path.join("lib")));
        }
        if rng.chance(25) {
            let dist = self.dir(path.join("dist"));
            folders.push(dist);
            folders.push(self.dir(path.join("dist").join("esm")));
            folders.push(self.dir(path.join("dist").join("cjs")));
        }
        if rng.chance(15) {
            folders.push(self.dir(path.join("types")));
        }
        for &folder in &folders {
            let count = if folder == root {
                3 + rng.below(11)
            } else {
                2 + rng.below(11)
            };
            for i in 0..count {
                let base = FILE_NAMES[rng.below(FILE_NAMES.len() as u64) as usize];
                let name = if i == 0 {
                    base.to_string()
                } else {
                    format!("{}-{i}.{}", word(rng, 2), base.rsplit('.').next().unwrap())
                };
                let link = (!self.store.is_empty() && rng.chance(15))
                    .then(|| self.store[rng.below(self.store.len() as u64) as usize]);
                let size = link.map_or_else(|| rng.file_size(), |t| self.files[t as usize].2);
                // Names inside one folder must be unique.
                let name = format!("{i:02}-{name}");
                self.file(folder, name, size, link);
                *budget -= 1;
            }
        }
        if depth < 3 && rng.chance(10) {
            let nested = path.join("node_modules");
            self.dir(nested.clone());
            for n in 0..1 + rng.below(3) {
                let name = format!("{}-{n}", word(rng, 3));
                self.package(rng, nested.join(name), budget, depth + 1);
            }
        }
    }
}

fn plan(files: u64, seed: u64) -> Plan {
    let mut rng = Rng(seed);
    let mut plan = Plan::default();
    plan.dir(PathBuf::new());
    // Content-addressed store, 5% of all files, 256 fan-out folders.
    let store_root = PathBuf::from("store").join("v3").join("files");
    plan.dir("store".into());
    plan.dir(PathBuf::from("store").join("v3"));
    plan.dir(store_root.clone());
    let fan: Vec<u32> = (0..256)
        .map(|i| plan.dir(store_root.join(format!("{i:02x}"))))
        .collect();
    for i in 0..(files / 20).max(1) {
        let dir = fan[(i % 256) as usize];
        let size = rng.file_size();
        let index = plan.file(dir, format!("{:016x}-index.js", rng.next()), size, None);
        plan.store.push(index);
    }
    // Projects with a small source tree and a large dependency tree.
    let projects = (files / 40_000).max(1);
    let per_project = (files.saturating_sub(plan.files.len() as u64) / projects) as i64;
    for p in 0..projects {
        let project = PathBuf::from("projects").join(format!("{}-{p:03}", word(&mut rng, 2)));
        if p == 0 {
            plan.dir("projects".into());
        }
        plan.dir(project.clone());
        let mut budget = per_project;
        for s in 0..(per_project / 50).max(1) {
            let dir = plan.dir(project.join("src").join(format!("module-{s:03}")));
            if s == 0 {
                plan.dir(project.join("src"));
            }
            for f in 0..8 {
                let size = rng.file_size();
                plan.file(dir, format!("{}-{f}.ts", word(&mut rng, 3)), size, None);
                budget -= 1;
            }
        }
        let modules = project.join("node_modules");
        plan.dir(modules.clone());
        let pnpm = p % 2 == 0;
        if pnpm {
            plan.dir(modules.join(".pnpm"));
        }
        let mut n = 0;
        while budget > 0 {
            let name = format!("{}-{n}", word(&mut rng, 3));
            n += 1;
            let scoped = rng.chance(12);
            let package = if pnpm {
                let version = format!("{}.{}.{}", rng.below(9), rng.below(30), rng.below(20));
                let outer = modules.join(".pnpm").join(format!("{name}@{version}"));
                plan.dir(outer.clone());
                plan.dir(outer.join("node_modules"));
                outer.join("node_modules").join(&name)
            } else if scoped {
                let scope = modules.join(format!("@{}", word(&mut rng, 2)));
                plan.dir(scope.clone());
                scope.join(&name)
            } else {
                modules.join(&name)
            };
            plan.package(&mut rng, package, &mut budget, 0);
        }
    }
    plan
}

struct Manifest {
    files: u64,
    directories: u64,
    logical: u64,
    unique: u64,
    hard_link_duplicates: u64,
    skipped_links: u64,
    locked_files: u64,
    locked_directories: u64,
    locked_logical: u64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--remove") {
        let dir = PathBuf::from(args.get(1).expect("--remove DIR"));
        remove(&dir);
        return;
    }
    let mut dir = None;
    let mut files = 1_000_000_u64;
    let mut seed = 20_261_102_u64;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--files" => files = iter.next().and_then(|v| v.parse().ok()).expect("--files N"),
            "--seed" => seed = iter.next().and_then(|v| v.parse().ok()).expect("--seed S"),
            other => dir = Some(PathBuf::from(other)),
        }
    }
    let dir = dir.expect("usage: make_fixture DIR [--files N] [--seed S] | --remove DIR");
    assert!(!dir.exists(), "{} already exists", dir.display());
    let start = Instant::now();
    let plan = plan(files.max(100), seed);
    fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    // Parents first, one depth level at a time, in parallel.
    let mut by_depth: Vec<&PathBuf> = plan.dirs.iter().collect();
    by_depth.sort_by_key(|p| p.components().count());
    for level in by_depth.chunk_by(|a, b| a.components().count() == b.components().count()) {
        level.par_iter().for_each(|path| {
            let path = dir.join(path);
            if !path.exists() {
                fs::create_dir(&path).unwrap();
            }
        });
    }
    let paths: Vec<PathBuf> = plan.dirs.iter().map(|p| dir.join(p)).collect();
    // Targets first, so every hard link has something to point at.
    let done = AtomicU64::new(0);
    let (targets, rest): (Vec<_>, Vec<_>) = plan.files.iter().partition(|f| f.3.is_none());
    for group in [targets, rest] {
        group.par_iter().for_each(|(d, name, size, link)| {
            let path = paths[*d as usize].join(name);
            match link {
                None => fs::File::create(&path).unwrap().set_len(*size).unwrap(),
                Some(target) => {
                    let (td, tn, ..) = &plan.files[*target as usize];
                    fs::hard_link(paths[*td as usize].join(tn), &path).unwrap();
                }
            }
            let n = done.fetch_add(1, Ordering::Relaxed);
            if n.is_multiple_of(100_000) {
                eprintln!("{n} files, {:.0} s", start.elapsed().as_secs_f64());
            }
        });
    }
    let mut manifest = Manifest {
        files: plan.files.len() as u64,
        directories: plan.dirs.len() as u64,
        logical: plan.files.iter().map(|f| f.2).sum(),
        unique: plan
            .files
            .iter()
            .filter(|f| f.3.is_none())
            .map(|f| f.2)
            .sum(),
        hard_link_duplicates: plan.files.iter().filter(|f| f.3.is_some()).count() as u64,
        skipped_links: 0,
        locked_files: 0,
        locked_directories: 0,
        locked_logical: 0,
    };
    extras(&dir, &mut manifest);
    let json = format!(
        concat!(
            "{{\n  \"seed\": {}, \"requested_files\": {},\n",
            "  \"files\": {}, \"directories\": {}, \"logical_bytes\": {}, \"unique_logical_bytes\": {},\n",
            "  \"hard_link_duplicates\": {}, \"skipped_links\": {},\n",
            "  \"unreadable\": {{\"folders\": 1, \"files\": {}, \"directories\": {}, \"logical_bytes\": {}}}\n}}"
        ),
        seed,
        files,
        manifest.files,
        manifest.directories,
        manifest.logical,
        manifest.unique,
        manifest.hard_link_duplicates,
        manifest.skipped_links,
        manifest.locked_files,
        manifest.locked_directories,
        manifest.locked_logical,
    );
    println!("{json}");
    let mut sidecar = dir.clone().into_os_string();
    sidecar.push(".manifest.json");
    fs::write(&sidecar, &json).unwrap();
    eprintln!(
        "{} files, {} directories in {:.1} s",
        manifest.files,
        manifest.directories,
        start.elapsed().as_secs_f64()
    );
}

/// Edge cases the size and structure accounting must get right. Counts in the
/// manifest include everything (as seen with full read access); the locked
/// folder's contents are listed separately because an ordinary user cannot see them.
fn extras(dir: &Path, m: &mut Manifest) {
    let add = |path: PathBuf, size: u64, m: &mut Manifest| {
        fs::File::create(&path).unwrap().set_len(size).unwrap();
        m.files += 1;
        m.logical += size;
        m.unique += size;
    };
    let media = dir.join("media");
    fs::create_dir(&media).unwrap();
    m.directories += 1;
    for (i, mib) in [8_u64, 16, 24, 32, 64].iter().enumerate() {
        add(media.join(format!("clip-{i}.mov")), mib << 20, m);
    }
    // A sparse file: large logical size, nothing allocated.
    let sparse = media.join("disk-image.sparse");
    let file = fs::File::create(&sparse).unwrap();
    set_sparse(&file);
    file.set_len(1 << 30).unwrap();
    m.files += 1;
    m.logical += 1 << 30;
    m.unique += 1 << 30;
    // Unicode, a trailing dot and a trailing space (Windows needs verbatim paths).
    let odd = dir.join("names");
    fs::create_dir(&odd).unwrap();
    m.directories += 1;
    add(odd.join("مرحبا-世界-😀.txt"), 1234, m);
    for name in ["trail.", "space "] {
        let path = odd.join(name);
        fs::create_dir(&path).unwrap();
        m.directories += 1;
        add(path.join("inside.bin"), 5000, m);
    }
    // A path well beyond MAX_PATH (260 characters).
    let mut deep = odd.clone();
    for i in 0..12 {
        deep = deep.join(format!("{i:02}-{}", "d".repeat(28)));
        fs::create_dir(&deep).unwrap();
        m.directories += 1;
    }
    add(deep.join("deep-file.txt"), 777, m);
    // A directory link, never followed: a junction on Windows, a symlink on Unix.
    let link = dir.join("link-to-media");
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&media)
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&media, &link).unwrap();
    m.skipped_links += 1;
    // A folder the current user cannot list. It is created readable, then locked.
    let locked = dir.join("locked");
    fs::create_dir_all(locked.join("inner")).unwrap();
    m.directories += 2;
    m.locked_directories = 1;
    for (i, size) in [100_u64, 20_000, 300_000].iter().enumerate() {
        add(locked.join(format!("secret-{i}.dat")), *size, m);
        m.locked_files += 1;
        m.locked_logical += size;
    }
    add(locked.join("inner").join("more.dat"), 4096, m);
    m.locked_files += 1;
    m.locked_logical += 4096;
    lock(&locked);
}

#[cfg(windows)]
fn set_sparse(file: &fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE};
    let mut returned = 0;
    // SAFETY: the handle is open for writing; no buffers are passed.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert!(ok != 0, "FSCTL_SET_SPARSE failed");
}

#[cfg(unix)]
fn set_sparse(_file: &fs::File) {
    // Extending with set_len leaves a hole on Unix filesystems.
}

fn user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap()
}

#[cfg(windows)]
fn lock(path: &Path) {
    let status = std::process::Command::new("icacls")
        .arg(path)
        .args(["/deny", &format!("{}:(OI)(CI)(RD)", user())])
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
}

#[cfg(unix)]
fn lock(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = user();
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
}

/// Unlock the locked folder and delete the fixture and its manifest.
fn remove(dir: &Path) {
    let locked = dir.join("locked");
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("icacls")
            .arg(&locked)
            .args(["/remove:d", &user()])
            .output();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));
    }
    // Remove the junction itself first so its target is never touched twice.
    let _ = fs::remove_dir(dir.join("link-to-media"));
    let verbatim = dir.canonicalize().unwrap();
    fs::remove_dir_all(&verbatim).unwrap();
    let mut sidecar = dir.as_os_str().to_owned();
    sidecar.push(".manifest.json");
    let _ = fs::remove_file(sidecar);
}
