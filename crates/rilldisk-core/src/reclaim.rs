// SPDX-License-Identifier: AGPL-3.0-only
//! Reclaimable-space detection from metadata alone.
//!
//! Rules look at names, entry kinds and neighbouring marker files (for example a
//! `target` directory next to `Cargo.toml`). No file contents are read, nothing is
//! deleted, and every candidate carries a reason and a suggested action for a
//! person or assistant to review.
use crate::{EntryKind, Snapshot, serialize_path};
use serde::Serialize;
use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    GitWorktree,
    BuildOutput,
    Dependencies,
    PackageCache,
    AppCache,
    Temporary,
    RecycleBin,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::GitWorktree,
        Category::BuildOutput,
        Category::Dependencies,
        Category::PackageCache,
        Category::AppCache,
        Category::Temporary,
        Category::RecycleBin,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Category::GitWorktree => "Git worktree",
            Category::BuildOutput => "Build output",
            Category::Dependencies => "Dependencies",
            Category::PackageCache => "Package cache",
            Category::AppCache => "App cache",
            Category::Temporary => "Temporary files",
            Category::RecycleBin => "Recycle Bin",
        }
    }
    /// The snake_case identifier used in JSON and MCP filters.
    pub fn key(self) -> &'static str {
        match self {
            Category::GitWorktree => "git_worktree",
            Category::BuildOutput => "build_output",
            Category::Dependencies => "dependencies",
            Category::PackageCache => "package_cache",
            Category::AppCache => "app_cache",
            Category::Temporary => "temporary",
            Category::RecycleBin => "recycle_bin",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == value)
    }
}

/// How confident RillDisk is that removal loses nothing irreplaceable.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    /// Rebuilt or reinstalled by project tooling from source files.
    Regenerable,
    /// Downloaded or recreated on demand; costs time or bandwidth to restore.
    Cache,
    /// May contain unique work or data. Inspect before removing.
    Review,
}

impl Safety {
    pub fn label(self) -> &'static str {
        match self {
            Safety::Regenerable => "Regenerable",
            Safety::Cache => "Cache",
            Safety::Review => "Review first",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub entry_id: usize,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub category: Category,
    pub safety: Safety,
    pub label: &'static str,
    pub reason: String,
    pub suggested_action: String,
    pub logical_bytes: u64,
    pub unique_logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    pub file_count: u64,
    /// On-disk bytes of files here with no other hard link found in this scan:
    /// removing the folder frees this much unless links exist outside the scanned
    /// root (allocated size, or length when unknown). Scan the whole drive for the
    /// most accurate figure.
    pub exclusive_bytes: u64,
    /// Logical bytes of files that also have a name elsewhere (for example a pnpm
    /// store). Removing this folder alone does not free them.
    pub shared_bytes: u64,
    pub newest_modified_unix_ms: Option<u64>,
    /// Whole days since anything in the folder was written, if known.
    pub idle_days: Option<u64>,
    /// Enclosing candidate. Nested candidates are excluded from totals.
    pub nested_in: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReclaimReport {
    pub scan_complete: bool,
    pub candidate_count: usize,
    /// Outermost candidates only, so nested folders are not double counted.
    pub total_logical_bytes: u64,
    pub total_unique_logical_bytes: u64,
    pub total_allocated_bytes: Option<u64>,
    /// Space freed by removing every outermost candidate, counting hard links found
    /// within this scan.
    pub total_exclusive_bytes: u64,
    pub candidates: Vec<Candidate>,
    pub notice: &'static str,
}

const NOTICE: &str = "Metadata-only suggestions. RillDisk never deletes files. Verify each path, \
prefer the suggested tool command, and treat 'review' candidates as possibly unique work. \
Freed-space figures count hard links found within this scan; scan the whole drive for the \
most accurate numbers.";

struct Rule {
    category: Category,
    safety: Safety,
    label: &'static str,
    reason: &'static str,
    action: &'static str,
}

const fn rule(
    category: Category,
    safety: Safety,
    label: &'static str,
    reason: &'static str,
    action: &'static str,
) -> Rule {
    Rule {
        category,
        safety,
        label,
        reason,
        action,
    }
}

/// Well-known cache locations, matched against trailing path components.
/// `*` matches any single component.
const LOCATIONS: &[(&[&str], Rule)] = {
    use Category::*;
    use Safety::*;
    &[
        (
            &["appdata", "local", "temp"],
            rule(
                Temporary,
                Cache,
                "Windows temp folder",
                "Per-user temporary files.",
                "Close running apps, then clear with Storage Sense or Disk Cleanup",
            ),
        ),
        (
            &["windows", "temp"],
            rule(
                Temporary,
                Cache,
                "System temp folder",
                "System temporary files.",
                "Clear with Storage Sense or Disk Cleanup (administrator)",
            ),
        ),
        (
            &["appdata", "local", "crashdumps"],
            rule(
                Temporary,
                Cache,
                "Crash dumps",
                "Application crash dumps.",
                "Delete after any crash investigation is finished",
            ),
        ),
        (
            &["appdata", "local", "npm-cache"],
            rule(
                PackageCache,
                Cache,
                "npm cache",
                "Downloaded npm packages; re-fetched on demand.",
                "npm cache clean --force",
            ),
        ),
        (
            &["appdata", "local", "pnpm", "store"],
            rule(
                PackageCache,
                Cache,
                "pnpm store",
                "Content-addressed pnpm package store.",
                "pnpm store prune",
            ),
        ),
        (
            &["appdata", "local", "pnpm-cache"],
            rule(
                PackageCache,
                Cache,
                "pnpm cache",
                "pnpm metadata cache.",
                "pnpm cache delete",
            ),
        ),
        (
            &["appdata", "local", "yarn", "cache"],
            rule(
                PackageCache,
                Cache,
                "Yarn cache",
                "Downloaded Yarn packages.",
                "yarn cache clean",
            ),
        ),
        (
            &["appdata", "local", "yarn", "berry", "cache"],
            rule(
                PackageCache,
                Cache,
                "Yarn Berry cache",
                "Downloaded Yarn packages.",
                "yarn cache clean --all",
            ),
        ),
        (
            &[".bun", "install", "cache"],
            rule(
                PackageCache,
                Cache,
                "Bun cache",
                "Downloaded Bun packages.",
                "bun pm cache rm",
            ),
        ),
        (
            &["appdata", "local", "pip", "cache"],
            rule(
                PackageCache,
                Cache,
                "pip cache",
                "Downloaded Python wheels.",
                "pip cache purge",
            ),
        ),
        (
            &["appdata", "local", "uv", "cache"],
            rule(
                PackageCache,
                Cache,
                "uv cache",
                "Downloaded Python packages.",
                "uv cache clean",
            ),
        ),
        (
            &["appdata", "local", "go-build"],
            rule(
                PackageCache,
                Cache,
                "Go build cache",
                "Compiled Go packages.",
                "go clean -cache",
            ),
        ),
        (
            &["go", "pkg", "mod"],
            rule(
                PackageCache,
                Cache,
                "Go module cache",
                "Downloaded Go modules.",
                "go clean -modcache",
            ),
        ),
        (
            &[".cargo", "registry"],
            rule(
                PackageCache,
                Cache,
                "Cargo registry",
                "Downloaded crates; Cargo re-fetches them.",
                "cargo cache --autoclean (cargo-cache), or delete registry\\cache and registry\\src",
            ),
        ),
        (
            &[".cargo", "git"],
            rule(
                PackageCache,
                Cache,
                "Cargo git checkouts",
                "Git dependencies; Cargo re-fetches them.",
                "Delete .cargo\\git\\checkouts; Cargo re-fetches on build",
            ),
        ),
        (
            &[".nuget", "packages"],
            rule(
                PackageCache,
                Cache,
                "NuGet packages",
                "Downloaded NuGet packages.",
                "dotnet nuget locals all --clear",
            ),
        ),
        (
            &[".gradle", "caches"],
            rule(
                PackageCache,
                Cache,
                "Gradle caches",
                "Downloaded Gradle dependencies and transforms.",
                "Stop Gradle daemons, then delete; Gradle re-downloads",
            ),
        ),
        (
            &[".m2", "repository"],
            rule(
                PackageCache,
                Cache,
                "Maven repository",
                "Downloaded Maven artifacts.",
                "Delete; Maven re-downloads on the next build",
            ),
        ),
        (
            &["appdata", "local", "ms-playwright"],
            rule(
                PackageCache,
                Cache,
                "Playwright browsers",
                "Downloaded test browsers.",
                "npx playwright uninstall --all",
            ),
        ),
        (
            &["appdata", "local", "unity", "cache"],
            rule(
                PackageCache,
                Cache,
                "Unity cache",
                "Unity package and asset caches.",
                "Clear from Unity Hub or delete; Unity re-downloads",
            ),
        ),
        (
            &[".cache", "huggingface"],
            rule(
                PackageCache,
                Review,
                "Hugging Face cache",
                "Downloaded models and datasets; large to re-download.",
                "huggingface-cli delete-cache",
            ),
        ),
        (
            &["appdata", "local", "microsoft", "windows", "inetcache"],
            rule(
                AppCache,
                Cache,
                "Internet cache",
                "Windows internet cache.",
                "Clear with Storage Sense or Disk Cleanup",
            ),
        ),
        (
            &["appdata", "local", "d3dscache"],
            rule(
                AppCache,
                Cache,
                "DirectX shader cache",
                "Compiled shaders; rebuilt on demand.",
                "Clear 'DirectX Shader Cache' in Disk Cleanup",
            ),
        ),
        (
            &["appdata", "local", "nvidia", "dxcache"],
            rule(
                AppCache,
                Cache,
                "NVIDIA shader cache",
                "Compiled shaders; rebuilt on demand.",
                "Delete while games are closed",
            ),
        ),
        (
            &["appdata", "local", "nvidia", "glcache"],
            rule(
                AppCache,
                Cache,
                "NVIDIA OpenGL cache",
                "Compiled shaders; rebuilt on demand.",
                "Delete while games are closed",
            ),
        ),
        (
            &["appdata", "local", "amd", "dxcache"],
            rule(
                AppCache,
                Cache,
                "AMD shader cache",
                "Compiled shaders; rebuilt on demand.",
                "Delete while games are closed",
            ),
        ),
        (
            &["google", "chrome", "user data", "*", "cache"],
            rule(
                AppCache,
                Cache,
                "Chrome cache",
                "Browser cache.",
                "Chrome > Delete browsing data > Cached images and files",
            ),
        ),
        (
            &["microsoft", "edge", "user data", "*", "cache"],
            rule(
                AppCache,
                Cache,
                "Edge cache",
                "Browser cache.",
                "Edge > Clear browsing data > Cached images and files",
            ),
        ),
        (
            &["appdata", "roaming", "code", "cache"],
            rule(
                AppCache,
                Cache,
                "VS Code cache",
                "Editor cache.",
                "Delete while VS Code is closed",
            ),
        ),
        (
            &["appdata", "roaming", "code", "cacheddata"],
            rule(
                AppCache,
                Cache,
                "VS Code cached data",
                "Editor compiled-code cache.",
                "Delete while VS Code is closed",
            ),
        ),
        (
            &["appdata", "roaming", "code", "cachedextensionvsixs"],
            rule(
                AppCache,
                Cache,
                "VS Code extension downloads",
                "Cached extension installers.",
                "Delete while VS Code is closed",
            ),
        ),
    ]
};

/// Analyze a snapshot. `now_unix_ms` makes idle-day calculations reproducible.
pub fn find_reclaimable(snapshot: &Snapshot, now_unix_ms: u64) -> ReclaimReport {
    let entries = &snapshot.entries;
    let mut found: Vec<(usize, &Rule, Option<String>)> = Vec::new();
    let mut worktrees = Vec::new();
    for entry in entries.iter().skip(1) {
        let name = snapshot.name(entry.id);
        match entry.kind {
            EntryKind::File if name == ".git" => {
                if let Some(parent) = entry.parent_id {
                    worktrees.push(parent);
                }
            }
            EntryKind::Directory => {
                if let Some((rule, detail)) = directory_rule(snapshot, entry.id, &name) {
                    found.push((entry.id, rule, detail));
                }
            }
            _ => {}
        }
    }
    const WORKTREE: Rule = rule(
        Category::GitWorktree,
        Safety::Review,
        "Git worktree",
        "Linked Git worktree: '.git' is a pointer file, so history lives in another repository.",
        "",
    );
    for dir in worktrees {
        if dir == 0 {
            continue;
        }
        let agent = agent_owner(&snapshot.path(dir));
        // Agent worktrees are never submodules, even inside a repo that has some.
        if agent.is_none() && in_submodule_repository(snapshot, dir) {
            continue;
        }
        found.push((dir, &WORKTREE, agent.map(|a| format!("Created by {a}."))));
    }
    // Agent tools keep worktrees under a `worktrees` folder. A child there with no
    // `.git` (directly or one level down) is no longer a checkout Git knows about.
    const ORPHAN: Rule = rule(
        Category::GitWorktree,
        Safety::Review,
        "Orphaned agent worktree",
        "Inside an AI-agent worktrees folder but has no '.git', so Git no longer tracks it.",
        "Check for unsaved work, then delete the folder and run 'git worktree prune' in the original repository",
    );
    for entry in entries.iter().skip(1) {
        if entry.kind != EntryKind::Directory
            || !snapshot.name(entry.id).eq_ignore_ascii_case("worktrees")
            || !is_agent_worktrees_root(snapshot, entry.id)
        {
            continue;
        }
        for &child in snapshot.children(entry.id) {
            let child = child as usize;
            if entries[child].kind != EntryKind::Directory {
                continue;
            }
            // Only judge folders whose listing (and their children's) was complete;
            // an unread folder is not evidence that `.git` is missing.
            let listed = entries[child].fully_listed
                && snapshot.children(child).iter().all(|&g| {
                    let g = &entries[g as usize];
                    g.kind != EntryKind::Directory || g.fully_listed
                });
            let tracked = has_child(snapshot, child, ".git")
                || snapshot
                    .children(child)
                    .iter()
                    .any(|&g| has_child(snapshot, g as usize, ".git"));
            if listed && !tracked {
                let agent = agent_owner(&snapshot.path(child).join("x")).unwrap_or("an AI agent");
                found.push((child, &ORPHAN, Some(format!("Created by {agent}."))));
            }
        }
    }

    let mut by_id: HashMap<usize, usize> = HashMap::new();
    found.sort_by_key(|(id, ..)| *id);
    found.dedup_by_key(|(id, ..)| *id);
    for (index, (id, ..)) in found.iter().enumerate() {
        by_id.insert(*id, index);
    }
    let mut candidates = Vec::with_capacity(found.len());
    for (id, rule, detail) in &found {
        // Find the nearest enclosing candidate.
        let mut enclosing = None;
        let mut cursor = entries[*id].parent_id;
        while let Some(parent) = cursor {
            if let Some(&index) = by_id.get(&parent) {
                enclosing = Some((parent, found[index].1.safety));
                break;
            }
            cursor = entries[parent].parent_id;
        }
        // A dependency folder inside another regenerable folder adds no new decision.
        if enclosing.is_some_and(|(_, safety)| safety != Safety::Review) {
            continue;
        }
        let entry = &entries[*id];
        let idle_days = entry
            .modified_unix_ms
            .map(|m| now_unix_ms.saturating_sub(m) / 86_400_000);
        let mut reason = rule.reason.to_string();
        if let Some(detail) = detail {
            reason.push(' ');
            reason.push_str(detail);
        }
        if let Some(days) = idle_days.filter(|&d| d >= 30) {
            reason.push_str(&format!(" Nothing inside changed for {days} days."));
        }
        let (exclusive_bytes, shared_bytes) = exclusive(snapshot, *id);
        let path = snapshot.path(*id);
        let suggested_action = if rule.category == Category::GitWorktree && rule.action.is_empty() {
            format!(
                "git worktree remove \"{}\"  (refuses if there are uncommitted changes)",
                path.display()
            )
        } else {
            rule.action.to_string()
        };
        candidates.push(Candidate {
            entry_id: *id,
            path,
            category: rule.category,
            safety: rule.safety,
            label: rule.label,
            reason,
            suggested_action,
            logical_bytes: entry.logical_bytes,
            unique_logical_bytes: entry.unique_logical_bytes,
            allocated_bytes: entry.allocated_bytes,
            file_count: entry.file_count,
            exclusive_bytes,
            shared_bytes,
            newest_modified_unix_ms: entry.modified_unix_ms,
            idle_days,
            nested_in: enclosing.map(|(parent, _)| parent),
        });
    }
    candidates.sort_by(|a, b| {
        b.logical_bytes
            .cmp(&a.logical_bytes)
            .then(a.entry_id.cmp(&b.entry_id))
    });
    let outer = candidates.iter().filter(|c| c.nested_in.is_none());
    let total_allocated_bytes = outer
        .clone()
        .map(|c| c.allocated_bytes)
        .try_fold(0_u64, |sum, value| value.map(|v| sum.saturating_add(v)));
    ReclaimReport {
        scan_complete: snapshot.summary.complete,
        candidate_count: candidates.len(),
        total_logical_bytes: outer.clone().map(|c| c.logical_bytes).sum(),
        total_unique_logical_bytes: outer.map(|c| c.unique_logical_bytes).sum(),
        total_allocated_bytes,
        total_exclusive_bytes: candidates
            .iter()
            .filter(|c| c.nested_in.is_none())
            .map(|c| c.exclusive_bytes)
            .sum(),
        candidates,
        notice: NOTICE,
    }
}

fn directory_rule(
    snapshot: &Snapshot,
    id: usize,
    name: &str,
) -> Option<(&'static Rule, Option<String>)> {
    use Category::*;
    use Safety::*;
    let lower = name.to_ascii_lowercase();
    let parent = snapshot.entries[id].parent_id?;
    let sibling = |marker: &str| has_child(snapshot, parent, marker);
    let sibling_ext = |ext: &str| has_child_with_extension(snapshot, parent, ext);
    static NODE: Rule = rule(
        Dependencies,
        Regenerable,
        "Node.js dependencies",
        "Installed from package.json.",
        "Reinstall when needed: npm install, pnpm install, yarn or bun install",
    );
    static CARGO: Rule = rule(
        BuildOutput,
        Regenerable,
        "Rust build output",
        "Cargo build artifacts next to Cargo.toml.",
        "cargo clean",
    );
    static MAVEN: Rule = rule(
        BuildOutput,
        Regenerable,
        "Maven build output",
        "Maven build artifacts next to pom.xml.",
        "mvn clean",
    );
    static WEB: Rule = rule(
        BuildOutput,
        Regenerable,
        "Web framework build cache",
        "Framework build cache next to package.json.",
        "Delete; recreated by the next dev or build run",
    );
    static GRADLE_BUILD: Rule = rule(
        BuildOutput,
        Regenerable,
        "Gradle build output",
        "Gradle build directory next to a Gradle script.",
        "gradle clean",
    );
    static GRADLE_CACHE: Rule = rule(
        PackageCache,
        Cache,
        "Gradle project cache",
        "Per-project Gradle cache.",
        "Delete while Gradle is stopped",
    );
    static DOTNET: Rule = rule(
        BuildOutput,
        Regenerable,
        ".NET build output",
        "bin/obj next to a .NET project file.",
        "dotnet clean, or delete bin and obj",
    );
    static PYCACHE: Rule = rule(
        BuildOutput,
        Regenerable,
        "Python bytecode cache",
        "Compiled .pyc files; Python recreates them.",
        "Delete; Python recreates it on import",
    );
    static VENV: Rule = rule(
        Dependencies,
        Regenerable,
        "Python virtual environment",
        "Contains pyvenv.cfg.",
        "Recreate from requirements or pyproject (python -m venv, uv sync)",
    );
    static PYTOOLS: Rule = rule(
        PackageCache,
        Cache,
        "Python tool cache",
        "Test, lint or type-checker cache.",
        "Delete; recreated on the next run",
    );
    static TERRAFORM: Rule = rule(
        Dependencies,
        Regenerable,
        "Terraform providers",
        "Provider plugins next to .tf files.",
        "terraform init",
    );
    static CMAKE: Rule = rule(
        BuildOutput,
        Regenerable,
        "CMake build tree",
        "IDE-generated CMake build directory.",
        "Delete; regenerated by the IDE",
    );
    static ZIG: Rule = rule(
        BuildOutput,
        Regenerable,
        "Zig cache",
        "Zig build cache next to build.zig.",
        "Delete; recreated on build",
    );
    static UNITY: Rule = rule(
        BuildOutput,
        Regenerable,
        "Unity Library",
        "Unity re-imports Library from Assets.",
        "Close Unity, then delete Library; the next open re-imports",
    );
    static UNREAL: Rule = rule(
        BuildOutput,
        Regenerable,
        "Unreal intermediate files",
        "Unreal regenerates this next to the .uproject.",
        "Close the editor, then delete",
    );
    static GODOT: Rule = rule(
        PackageCache,
        Cache,
        "Godot import cache",
        "Godot re-imports assets next to project.godot.",
        "Close Godot, then delete .godot",
    );
    static RECYCLE: Rule = rule(
        RecycleBin,
        Review,
        "Recycle Bin",
        "Deleted files waiting in the Recycle Bin.",
        "Review, then empty the Recycle Bin",
    );

    let rule: &'static Rule = match lower.as_str() {
        "node_modules" if sibling("package.json") => &NODE,
        "target" if sibling("cargo.toml") => &CARGO,
        "target" if sibling("pom.xml") => &MAVEN,
        ".next" | ".nuxt" | ".svelte-kit" | ".turbo" | ".parcel-cache" | ".angular"
        | ".docusaurus" | ".expo" | ".vercel" | ".output"
            if sibling("package.json") =>
        {
            &WEB
        }
        "build" if sibling("build.gradle") || sibling("build.gradle.kts") => &GRADLE_BUILD,
        ".gradle"
            if sibling("build.gradle")
                || sibling("build.gradle.kts")
                || sibling("settings.gradle")
                || sibling("settings.gradle.kts") =>
        {
            &GRADLE_CACHE
        }
        "bin" | "obj"
            if sibling_ext(".csproj") || sibling_ext(".fsproj") || sibling_ext(".vbproj") =>
        {
            &DOTNET
        }
        "__pycache__" => &PYCACHE,
        ".venv" | "venv" | ".virtualenv" | "env" if has_child(snapshot, id, "pyvenv.cfg") => &VENV,
        ".pytest_cache" | ".mypy_cache" | ".ruff_cache" | ".tox" | ".nox" | ".hypothesis" => {
            &PYTOOLS
        }
        ".terraform" if sibling_ext(".tf") => &TERRAFORM,
        ".zig-cache" | "zig-cache" if sibling("build.zig") => &ZIG,
        "library" if sibling("assets") && sibling("projectsettings") => &UNITY,
        "intermediate" | "deriveddatacache" if sibling_ext(".uproject") => &UNREAL,
        ".godot" if sibling("project.godot") => &GODOT,
        "$recycle.bin" => &RECYCLE,
        _ if lower.starts_with("cmake-build-") => &CMAKE,
        _ => return location_rule(snapshot, id, &lower).map(|r| (r, None)),
    };
    Some((rule, None))
}

fn location_rule(snapshot: &Snapshot, id: usize, last: &str) -> Option<&'static Rule> {
    let mut components: Option<Vec<String>> = None;
    for (pattern, rule) in LOCATIONS {
        let tail = pattern[pattern.len() - 1];
        if tail != "*" && tail != last {
            continue;
        }
        let components = components.get_or_insert_with(|| {
            snapshot
                .path(id)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
                .collect()
        });
        if components.len() >= pattern.len()
            && pattern
                .iter()
                .rev()
                .zip(components.iter().rev())
                .all(|(p, c)| *p == "*" || p == c)
        {
            return Some(rule);
        }
    }
    None
}

/// Split a subtree's files into space only this folder holds and hard-linked space.
fn exclusive(snapshot: &Snapshot, id: usize) -> (u64, u64) {
    let (mut exclusive, mut shared) = (0_u64, 0_u64);
    let mut stack = vec![id];
    while let Some(current) = stack.pop() {
        let entry = &snapshot.entries[current];
        match entry.kind {
            EntryKind::File if entry.multiple_links => shared += entry.logical_bytes,
            EntryKind::File => {
                exclusive += entry.allocated_bytes.unwrap_or(entry.logical_bytes);
            }
            EntryKind::Directory => {
                stack.extend(snapshot.children(current).iter().map(|&c| c as usize));
            }
            _ => {}
        }
    }
    (exclusive, shared)
}

/// `<tool>\worktrees` folders that AI coding agents create: `.claude`, `.codex`,
/// `.t3`, `.cursor`, profile variants like `.claude-work`, and `OpenAI\Codex`.
/// Never inside a `.git` directory.
fn is_agent_worktrees_root(snapshot: &Snapshot, id: usize) -> bool {
    let Some(parent) = snapshot.entries[id].parent_id else {
        return false;
    };
    let mut cursor = Some(parent);
    while let Some(current) = cursor {
        if snapshot.name(current).eq_ignore_ascii_case(".git") {
            return false;
        }
        cursor = snapshot.entries[current].parent_id;
    }
    let name = snapshot.name(parent).to_lowercase();
    let grandparent = snapshot.entries[parent]
        .parent_id
        .map(|g| snapshot.name(g).to_lowercase());
    matches!(name.as_str(), ".claude" | ".codex" | ".t3" | ".cursor")
        || name.starts_with(".claude-")
        || (name == "codex" && grandparent.as_deref() == Some("openai"))
}

fn has_child(snapshot: &Snapshot, id: usize, name: &str) -> bool {
    snapshot
        .children(id)
        .iter()
        .any(|&c| snapshot.name(c as usize).eq_ignore_ascii_case(name))
}

fn has_child_with_extension(snapshot: &Snapshot, id: usize, extension: &str) -> bool {
    snapshot.children(id).iter().any(|&c| {
        let name = snapshot.name(c as usize);
        name.len() > extension.len()
            && name.is_char_boundary(name.len() - extension.len())
            && name[name.len() - extension.len()..].eq_ignore_ascii_case(extension)
    })
}

/// Submodule checkouts also use a `.git` file; skip them when the enclosing
/// repository declares submodules.
fn in_submodule_repository(snapshot: &Snapshot, dir: usize) -> bool {
    let mut cursor = snapshot.entries[dir].parent_id;
    while let Some(parent) = cursor {
        if has_child(snapshot, parent, ".git") {
            return has_child(snapshot, parent, ".gitmodules");
        }
        cursor = snapshot.entries[parent].parent_id;
    }
    false
}

fn agent_owner(path: &std::path::Path) -> Option<&'static str> {
    let text = path.to_string_lossy().to_lowercase().replace('/', "\\");
    let owners = [
        ("\\.codex\\worktrees\\", "Codex"),
        ("\\openai\\codex\\worktrees\\", "Codex"),
        ("\\.claude\\worktrees\\", "Claude Code"),
        ("\\.t3\\worktrees\\", "T3 Code"),
        ("\\.cursor\\worktrees\\", "Cursor"),
        ("\\conductor\\workspaces\\", "Conductor"),
    ];
    if let Some((_, owner)) = owners.iter().find(|(needle, _)| text.contains(needle)) {
        return Some(owner);
    }
    // Profile-specific agent homes such as `.claude-work\worktrees`.
    (text.contains("\\.claude-") && text.contains("\\worktrees\\")).then_some("Claude Code")
}
