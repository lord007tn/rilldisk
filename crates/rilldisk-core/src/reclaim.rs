// SPDX-License-Identifier: AGPL-3.0-only
//! Reclaimable-space detection from metadata alone.
//!
//! Rules look at names, entry kinds and neighbouring marker files (for example a
//! `target` directory next to `Cargo.toml`). No file contents are read, nothing is
//! deleted here, and every candidate carries a reason and a suggested action for a
//! person or assistant to review. Removing candidates the user selected and
//! confirmed is the separate `cleanup` module's job.
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
    /// AI coding agent that created this worktree, when its location says so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<&'static str>,
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

const NOTICE: &str = "Metadata-only suggestions. RillDisk never deletes anything on its own. Verify each path, \
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
/// `*` matches any single component. A leading `~` matches a user's home folder
/// (`/home/<name>`, `/Users/<name>`, `/root` or `C:\Users\<name>`) and nothing else.
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
                "cargo cache --autoclean (cargo-cache), or delete the cache and src folders inside it",
            ),
        ),
        (
            &[".cargo", "git"],
            rule(
                PackageCache,
                Cache,
                "Cargo git checkouts",
                "Git dependencies; Cargo re-fetches them.",
                "Delete the checkouts folder inside it; Cargo re-fetches on build",
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
        (
            &["~", ".cache"],
            rule(
                AppCache,
                Review,
                "User cache folder",
                "Per-user application caches (XDG). Apps recreate them, but some are slow to restore; known caches inside are listed separately.",
                "Quit apps, then remove what you no longer need; prefer the commands listed for folders inside",
            ),
        ),
        (
            &["~", "library", "caches"],
            rule(
                AppCache,
                Review,
                "macOS user caches",
                "Per-user application caches. Apps recreate them, but some are slow to restore; known caches inside are listed separately.",
                "Quit apps, then remove what you no longer need; prefer the commands listed for folders inside",
            ),
        ),
        (
            &["~", "library", "caches", "homebrew"],
            rule(
                PackageCache,
                Cache,
                "Homebrew cache",
                "Downloaded bottles and source archives.",
                "brew cleanup --prune=all",
            ),
        ),
        (
            &["~", ".cache", "homebrew"],
            rule(
                PackageCache,
                Cache,
                "Homebrew cache",
                "Downloaded bottles and source archives.",
                "brew cleanup --prune=all",
            ),
        ),
        (
            &["~", ".cache", "pip"],
            rule(
                PackageCache,
                Cache,
                "pip cache",
                "Downloaded Python wheels.",
                "pip cache purge",
            ),
        ),
        (
            &["~", "library", "caches", "pip"],
            rule(
                PackageCache,
                Cache,
                "pip cache",
                "Downloaded Python wheels.",
                "pip cache purge",
            ),
        ),
        (
            &["~", ".cache", "uv"],
            rule(
                PackageCache,
                Cache,
                "uv cache",
                "Downloaded Python packages.",
                "uv cache clean",
            ),
        ),
        (
            &["~", "library", "caches", "uv"],
            rule(
                PackageCache,
                Cache,
                "uv cache",
                "Downloaded Python packages.",
                "uv cache clean",
            ),
        ),
        (
            &["~", ".cache", "go-build"],
            rule(
                PackageCache,
                Cache,
                "Go build cache",
                "Compiled Go packages.",
                "go clean -cache",
            ),
        ),
        (
            &["~", "library", "caches", "go-build"],
            rule(
                PackageCache,
                Cache,
                "Go build cache",
                "Compiled Go packages.",
                "go clean -cache",
            ),
        ),
        (
            &["~", ".cache", "yarn"],
            rule(
                PackageCache,
                Cache,
                "Yarn cache",
                "Downloaded Yarn packages.",
                "yarn cache clean",
            ),
        ),
        (
            &["~", "library", "caches", "yarn"],
            rule(
                PackageCache,
                Cache,
                "Yarn cache",
                "Downloaded Yarn packages.",
                "yarn cache clean",
            ),
        ),
        (
            &["~", ".cache", "pnpm"],
            rule(
                PackageCache,
                Cache,
                "pnpm cache",
                "pnpm metadata cache.",
                "pnpm cache delete",
            ),
        ),
        (
            &["~", "library", "caches", "pnpm"],
            rule(
                PackageCache,
                Cache,
                "pnpm cache",
                "pnpm metadata cache.",
                "pnpm cache delete",
            ),
        ),
        (
            &["~", ".cache", "ms-playwright"],
            rule(
                PackageCache,
                Cache,
                "Playwright browsers",
                "Downloaded test browsers.",
                "npx playwright uninstall --all",
            ),
        ),
        (
            &["~", "library", "caches", "ms-playwright"],
            rule(
                PackageCache,
                Cache,
                "Playwright browsers",
                "Downloaded test browsers.",
                "npx playwright uninstall --all",
            ),
        ),
        (
            &["~", ".npm", "_cacache"],
            rule(
                PackageCache,
                Cache,
                "npm cache",
                "Downloaded npm packages; re-fetched on demand.",
                "npm cache clean --force",
            ),
        ),
        (
            &["~", ".pnpm-store"],
            rule(
                PackageCache,
                Cache,
                "pnpm store",
                "Content-addressed pnpm package store.",
                "pnpm store prune",
            ),
        ),
        (
            &["~", ".local", "share", "pnpm", "store"],
            rule(
                PackageCache,
                Cache,
                "pnpm store",
                "Content-addressed pnpm package store.",
                "pnpm store prune",
            ),
        ),
        (
            &["~", "library", "pnpm", "store"],
            rule(
                PackageCache,
                Cache,
                "pnpm store",
                "Content-addressed pnpm package store.",
                "pnpm store prune",
            ),
        ),
        (
            &["~", "library", "developer", "xcode", "deriveddata"],
            rule(
                BuildOutput,
                Regenerable,
                "Xcode DerivedData",
                "Xcode build products and indexes; rebuilt on the next build.",
                "Quit Xcode, then delete; Xcode rebuilds it",
            ),
        ),
        (
            &["~", "library", "developer", "xcode", "ios devicesupport"],
            rule(
                AppCache,
                Cache,
                "iOS device support",
                "Debug symbols copied from connected devices; copied again on the next connection.",
                "Delete versions for devices you no longer use",
            ),
        ),
        (
            &["~", "library", "developer", "coresimulator", "caches"],
            rule(
                AppCache,
                Review,
                "Simulator caches",
                "CoreSimulator caches, including dyld caches for installed runtimes.",
                "Quit Simulator and Xcode, then delete; rebuilt when a simulator boots",
            ),
        ),
        (
            &["~", "library", "developer", "coresimulator", "devices"],
            rule(
                AppCache,
                Review,
                "Simulator devices",
                "Simulator devices with installed apps and their data.",
                "xcrun simctl delete unavailable, or remove devices in Xcode > Devices and Simulators",
            ),
        ),
        (
            &["~", ".trash"],
            rule(
                RecycleBin,
                Review,
                "Trash",
                "Files moved to the Trash.",
                "Review, then empty the Trash in Finder",
            ),
        ),
        (
            &["~", ".local", "share", "trash"],
            rule(
                RecycleBin,
                Review,
                "Trash",
                "Files moved to the Trash.",
                "Review, then empty the Trash from your file manager",
            ),
        ),
    ]
};

impl ReclaimReport {
    /// Keep matching candidates and recompute every total from what remains.
    pub fn retain(&mut self, keep: impl Fn(&Candidate) -> bool) {
        self.candidates.retain(|c| keep(c));
        let kept: std::collections::HashSet<usize> =
            self.candidates.iter().map(|c| c.entry_id).collect();
        let outer = self
            .candidates
            .iter()
            .filter(|c| c.nested_in.is_none_or(|n| !kept.contains(&n)));
        self.candidate_count = self.candidates.len();
        self.total_logical_bytes = outer.clone().map(|c| c.logical_bytes).sum();
        self.total_unique_logical_bytes = outer.clone().map(|c| c.unique_logical_bytes).sum();
        self.total_exclusive_bytes = outer.clone().map(|c| c.exclusive_bytes).sum();
        self.total_allocated_bytes = outer
            .map(|c| c.allocated_bytes)
            .try_fold(0_u64, |sum, value| value.map(|v| sum.saturating_add(v)));
    }
}

/// Analyze a snapshot. `now_unix_ms` makes idle-day calculations reproducible.
pub fn find_reclaimable(snapshot: &Snapshot, now_unix_ms: u64) -> ReclaimReport {
    let entries = &snapshot.entries;
    let mut found: Vec<(usize, &Rule, Option<String>, Safety)> = Vec::new();
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
                if let Some((rule, detail, safety)) = directory_rule(snapshot, entry.id, &name) {
                    found.push((entry.id, rule, detail, safety));
                }
            }
            _ => {}
        }
    }
    const WORKTREE: Rule = rule(
        Category::GitWorktree,
        Safety::Review,
        "Git worktree",
        "Linked Git worktree: '.git' is a pointer file, so history lives in another repository. Git refuses to remove it while it has uncommitted changes.",
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
        found.push((
            dir,
            &WORKTREE,
            agent.map(|a| format!("Created by {a}.")),
            Safety::Review,
        ));
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
                found.push((
                    child,
                    &ORPHAN,
                    Some(format!("Created by {agent}.")),
                    Safety::Review,
                ));
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
    for (id, rule, detail, safety) in &found {
        // Find the nearest enclosing candidate.
        let mut enclosing = None;
        let mut cursor = entries[*id].parent_id;
        while let Some(parent) = cursor {
            if let Some(&index) = by_id.get(&parent) {
                enclosing = Some((parent, found[index].3));
                break;
            }
            cursor = entries[parent].parent_id;
        }
        // A regenerable folder inside another regenerable folder adds no new decision;
        // anything that needs review is always kept.
        if *safety != Safety::Review && enclosing.is_some_and(|(_, outer)| outer != Safety::Review)
        {
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
            match quoted(&path) {
                Some(path) => format!("git worktree remove {path}"),
                None => "git worktree remove <this folder>".to_string(),
            }
        } else if std::ptr::eq(*rule, &CARGO) {
            match path
                .parent()
                .map(|p| p.join("Cargo.toml"))
                .as_deref()
                .and_then(quoted)
            {
                Some(manifest) => format!("cargo clean --manifest-path {manifest}"),
                // A bare `cargo clean` would clean whatever project the shell is in.
                None => "cargo clean --manifest-path <this project's Cargo.toml>".to_string(),
            }
        } else {
            rule.action.to_string()
        };
        let agent = (rule.category == Category::GitWorktree)
            .then(|| agent_owner(&path.join("x")))
            .flatten();
        candidates.push(Candidate {
            entry_id: *id,
            path,
            category: rule.category,
            safety: *safety,
            label: rule.label,
            agent,
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

static CARGO: Rule = rule(
    Category::BuildOutput,
    Safety::Regenerable,
    "Rust build output",
    "Cargo build artifacts next to Cargo.toml.",
    "cargo clean",
);

fn directory_rule(
    snapshot: &Snapshot,
    id: usize,
    name: &str,
) -> Option<(&'static Rule, Option<String>, Safety)> {
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
    static VOLUME_TRASH: Rule = rule(
        RecycleBin,
        Review,
        "Trash",
        "Files moved to the Trash on this volume.",
        "Review, then empty the Trash from your file manager or Finder",
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
        // macOS `.Trashes` and freedesktop `.Trash-<uid>` folders live at a volume
        // root. Scans stay on one filesystem, so that is the top of the scan.
        ".trashes" if parent == 0 && is_volume_root(snapshot) => &VOLUME_TRASH,
        _ if parent == 0
            && is_volume_root(snapshot)
            && lower
                .strip_prefix(".trash-")
                .is_some_and(|uid| !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit())) =>
        {
            &VOLUME_TRASH
        }
        _ if lower.starts_with("cmake-build-") && sibling("cmakelists.txt") => &CMAKE,
        _ => return location_rule(snapshot, id, &lower),
    };
    Some((rule, None, rule.safety))
}

/// Known locations match trailing components, whatever the separator. Profile
/// (`AppData`) and system (`Windows`) rules must also sit where Windows puts them
/// (`...\Users\<name>\AppData`, `<drive>:\Windows`); a look-alike elsewhere may be a
/// copy or backup, so it is downgraded to Review. Home (`~`) rules only match a real
/// home folder, because names like `.cache` are common inside projects.
fn location_rule(
    snapshot: &Snapshot,
    id: usize,
    last: &str,
) -> Option<(&'static Rule, Option<String>, Safety)> {
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
                .all(|(p, c)| matches!(*p, "*" | "~") || p == c)
        {
            let start = components.len() - pattern.len();
            let anchored = match pattern[0] {
                "~" if !is_home(components, start, root_depth(snapshot)) => continue,
                "appdata" => start >= 2 && components[start - 2] == "users",
                // Drive prefix (`c:`), root, then `windows`. Unix paths start at `/`.
                "windows" => start == 2 && !components[0].starts_with(['/', '\\']),
                _ => true,
            };
            if anchored {
                return Some((rule, None, rule.safety));
            }
            return Some((
                rule,
                Some("Not in the standard profile or system location, so it may be a copy or backup.".into()),
                Safety::Review,
            ));
        }
    }
    None
}

/// Whether `components[at]` is a home folder: `/home/<name>`, `/Users/<name>`,
/// `C:\Users\<name>` or `/root`. The `home`/`Users` folder must sit at the
/// filesystem or drive root, or directly inside the scanned folder; a copy of a
/// profile deeper down (for example in a backup) is not a home.
fn is_home(components: &[String], at: usize, scan_root: usize) -> bool {
    let top = |index: usize| {
        index == scan_root
            || (index == 1 && components[0] == "/")
            || (index == 2 && components[1] == "\\" && components[0].ends_with(':'))
    };
    (at >= 1 && matches!(components[at - 1].as_str(), "home" | "users") && top(at - 1))
        || (at == 1 && components[1] == "root" && components[0] == "/")
}

/// Whether the scanned folder is the top of a volume: `/`, a drive root, or a
/// standard mount location (`/Volumes/<disk>`, `/mnt/<disk>`, `/media/<disk>`,
/// `/media/<user>/<disk>`, `/run/media/<user>/<disk>`).
fn is_volume_root(snapshot: &Snapshot) -> bool {
    volume_root_path(&snapshot.summary.root)
}

pub(crate) fn volume_root_path(root: &std::path::Path) -> bool {
    let parts: Vec<String> = root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let names: Vec<&str> = parts.iter().map(String::as_str).collect();
    match names.as_slice() {
        ["/"] => true,
        [drive, "\\"] if drive.ends_with(':') => true,
        ["/", "volumes" | "mnt" | "media", _] => true,
        ["/", "media", _, _] | ["/", "run", "media", _, _] => true,
        _ => false,
    }
}

/// Number of path components of the scanned folder.
fn root_depth(snapshot: &Snapshot) -> usize {
    snapshot.path(0).components().count()
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

/// Submodule checkouts also use a `.git` file. Git keeps a submodule's repository
/// at `<repo>/.git/modules/<path>`, so a matching folder there identifies one
/// without reading any file contents.
fn in_submodule_repository(snapshot: &Snapshot, dir: usize) -> bool {
    let mut relative = vec![dir];
    let mut cursor = snapshot.entries[dir].parent_id;
    while let Some(parent) = cursor {
        let git = snapshot
            .children(parent)
            .iter()
            .map(|&c| c as usize)
            .find(|&c| snapshot.name(c).eq_ignore_ascii_case(".git"));
        if let Some(git) = git {
            if snapshot.entries[git].kind != EntryKind::Directory {
                return false;
            }
            let mut node = child_named(snapshot, git, "modules");
            for &component in relative.iter().rev() {
                node = node.and_then(|n| child_named(snapshot, n, &snapshot.name(component)));
            }
            return node.is_some();
        }
        relative.push(parent);
        cursor = snapshot.entries[parent].parent_id;
    }
    false
}

fn child_named(snapshot: &Snapshot, id: usize, name: &str) -> Option<usize> {
    snapshot
        .children(id)
        .iter()
        .map(|&c| c as usize)
        .find(|&c| snapshot.name(c).eq_ignore_ascii_case(name))
}

/// Double-quote a path for a copied command when that is safe in both PowerShell
/// and POSIX shells; otherwise return None so callers omit it. On Unix a backslash
/// is an ordinary filename byte that POSIX double quotes may still interpret.
fn quoted(path: &std::path::Path) -> Option<String> {
    let text = path.to_str()?;
    let special: &[char] = if cfg!(windows) {
        &['"', '$', '`', '\'', '%', '!']
    } else {
        &['"', '$', '`', '\'', '%', '!', '\\']
    };
    (!text.contains(special) && !text.contains(char::is_control)).then(|| format!("\"{text}\""))
}

/// The AI agent whose worktree folder holds `path`. Matches whole path components,
/// so `/` and `\` separators behave the same.
fn agent_owner(path: &std::path::Path) -> Option<&'static str> {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    // `<tool>/worktrees/<something>`: only paths inside the folder count.
    // Innermost first: a Codex worktree inside a Claude Code worktree is Codex's.
    (0..parts.len().saturating_sub(2)).rev().find_map(|i| {
        match (parts[i].as_str(), parts[i + 1].as_str()) {
            (".codex", "worktrees") => Some("Codex"),
            ("codex", "worktrees") if i > 0 && parts[i - 1] == "openai" => Some("Codex"),
            (".claude", "worktrees") => Some("Claude Code"),
            // Profile-specific agent homes such as `.claude-work/worktrees`.
            (tool, "worktrees") if tool.starts_with(".claude-") => Some("Claude Code"),
            (".t3", "worktrees") => Some("T3 Code"),
            (".cursor", "worktrees") => Some("Cursor"),
            ("conductor", "workspaces") => Some("Conductor"),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::volume_root_path;
    use std::path::Path;

    #[cfg(unix)]
    #[test]
    fn recognizes_unix_volume_roots_only() {
        for root in [
            "/",
            "/Volumes/USB",
            "/mnt/data",
            "/media/disk",
            "/media/me/USB",
            "/run/media/me/USB",
        ] {
            assert!(volume_root_path(Path::new(root)), "{root}");
        }
        for root in ["/home/me/project", "/Volumes/USB/backup", "/mnt"] {
            assert!(!volume_root_path(Path::new(root)), "{root}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn recognizes_drive_roots_only() {
        assert!(volume_root_path(Path::new(r"D:\")));
        assert!(!volume_root_path(Path::new(r"D:\Projects")));
    }
}
