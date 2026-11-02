// SPDX-License-Identifier: AGPL-3.0-only
//! Removal of reclaim candidates the user selected and confirmed.
//!
//! This is the only code in RillDisk that changes the filesystem. Scope and
//! threat model: `docs/proposals/cleanup.md`. In short:
//!
//! * Only paths that are reclaim candidates of the given report and snapshot
//!   can be removed; anything else is refused.
//! * Every safety check runs again immediately before each removal: the path
//!   must still be a real directory at the same canonical location (no link,
//!   junction or other reparse point at the path or above it), must not be a
//!   volume root or mount point, the scan root, the home folder, a system or
//!   temp folder, or hold the running program.
//! * The default mode moves items to the Recycle Bin / Trash. Permanent
//!   deletion is an explicit opt-in.
//! * Callers must ask the user first; this module does no prompting.
use crate::{Candidate, Category, EntryKind, ReclaimReport, Safety, Snapshot, serialize_path};
use serde::Serialize;
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

/// Where removed items go.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    /// Windows Recycle Bin, macOS Trash or the freedesktop.org Trash. Restorable.
    #[default]
    Trash,
    /// Deleted immediately. Not restorable.
    Permanent,
}

impl CleanupMode {
    /// The platform's name for the Trash.
    pub fn trash_name() -> &'static str {
        if cfg!(windows) {
            "Recycle Bin"
        } else {
            "Trash"
        }
    }
}

/// One folder a cleanup would remove.
#[derive(Debug, Clone, Serialize)]
pub struct PlanItem {
    pub entry_id: usize,
    #[serde(serialize_with = "serialize_path")]
    pub path: PathBuf,
    pub category: Category,
    pub safety: Safety,
    pub label: &'static str,
    pub logical_bytes: u64,
    /// Space removal frees, counting hard links found in the scan.
    pub exclusive_bytes: u64,
    pub file_count: u64,
}

/// A requested item that will not be touched, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Refusal {
    pub entry_id: usize,
    #[serde(
        serialize_with = "serialize_path",
        skip_serializing_if = "is_empty_path"
    )]
    pub path: PathBuf,
    pub reason: String,
}

fn is_empty_path(path: &Path) -> bool {
    path.as_os_str().is_empty()
}

/// What a cleanup would do, for the confirmation step. Building it touches
/// nothing; [`execute`] rebuilds it and re-checks every item before removing.
#[derive(Debug, Clone, Serialize)]
pub struct CleanupPlan {
    pub mode: CleanupMode,
    /// Outermost selected candidates, largest first. A selected candidate inside
    /// another selected one is removed with it and not listed separately.
    pub items: Vec<PlanItem>,
    pub refused: Vec<Refusal>,
    /// Selected candidates already covered by an enclosing selected candidate.
    pub covered: usize,
    pub total_logical_bytes: u64,
    pub total_exclusive_bytes: u64,
    pub total_files: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// Moved to the Trash or deleted.
    Removed { freed_bytes: u64 },
    /// A safety check failed; nothing was touched.
    Refused { reason: String },
    /// The operating system reported an error. With `maybe_partial`, some
    /// content may already be gone; rescan to see what is left.
    Failed { error: String, maybe_partial: bool },
    /// Cancelled before this item started.
    NotRun,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemResult {
    #[serde(flatten)]
    pub item: PlanItem,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanupReport {
    pub mode: CleanupMode,
    pub items: Vec<ItemResult>,
    /// Requested entries refused while planning (not candidates, protected, ...).
    pub refused: Vec<Refusal>,
    pub removed: usize,
    pub failed: usize,
    pub freed_bytes: u64,
    pub cancelled: bool,
    /// Steps RillDisk does not take itself, such as `git worktree prune`.
    pub follow_ups: Vec<String>,
}

impl CleanupReport {
    /// Entry IDs that are gone, for [`Snapshot::without`].
    pub fn removed_entry_ids(&self) -> Vec<usize> {
        self.items
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Removed { .. }))
            .map(|r| r.item.entry_id)
            .collect()
    }

    /// Whether some content may have been removed without being reported as
    /// removed, so figures are only exact after a rescan.
    pub fn needs_rescan(&self) -> bool {
        self.items.iter().any(|r| {
            matches!(
                r.outcome,
                Outcome::Failed {
                    maybe_partial: true,
                    ..
                }
            )
        })
    }
}

/// Progress and cancellation shared with a running [`execute`].
#[derive(Debug, Default)]
pub struct CleanupControl {
    cancel: AtomicBool,
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub freed: AtomicU64,
    current: Mutex<PathBuf>,
}

impl CleanupControl {
    /// Stop after the item in progress. A started removal is not interrupted.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// The item being removed right now.
    pub fn current(&self) -> PathBuf {
        self.current.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

/// Paths a cleanup must never remove, contain or descend into.
#[derive(Debug, Clone)]
pub struct Guard {
    /// Refused, along with every ancestor.
    exact: Vec<(PathBuf, &'static str)>,
    /// Refused, along with every ancestor and everything inside.
    trees: Vec<(PathBuf, &'static str)>,
    /// The running executable: no folder containing it is removed.
    exe: Option<PathBuf>,
}

impl Guard {
    /// The protected locations of this machine and user.
    pub fn system() -> Self {
        let mut guard = Self {
            exact: Vec::new(),
            trees: Vec::new(),
            exe: std::env::current_exe()
                .ok()
                .map(|p| canonical(&p).unwrap_or(p)),
        };
        let env = |key: &str| std::env::var_os(key).filter(|v| !v.is_empty());
        for home in [env("USERPROFILE"), env("HOME")].into_iter().flatten() {
            let home = PathBuf::from(home);
            guard.protect(&home, "your home folder");
            #[cfg(target_os = "macos")]
            guard.protect(
                &home.join("Library").join("Caches"),
                "a system-managed folder",
            );
        }
        guard.protect(&std::env::temp_dir(), "the system temp folder");
        #[cfg(windows)]
        {
            for key in [
                "SystemRoot",
                "windir",
                "ProgramFiles",
                "ProgramFiles(x86)",
                "ProgramW6432",
                "ProgramData",
            ] {
                if let Some(value) = env(key) {
                    guard.protect_tree(Path::new(&value), "a system folder");
                }
            }
            // `C:` joined with a name is drive-relative; start from the drive root.
            let drive = env("SystemDrive")
                .map_or_else(|| "C:".into(), |d| d.to_string_lossy().into_owned());
            let drive = PathBuf::from(format!("{drive}\\"));
            for name in [
                "Windows",
                "Program Files",
                "Program Files (x86)",
                "ProgramData",
            ] {
                guard.protect_tree(&drive.join(name), "a system folder");
            }
            guard.protect(&drive.join("Users"), "the folder of user profiles");
        }
        #[cfg(unix)]
        {
            for path in [
                "/bin",
                "/boot",
                "/dev",
                "/etc",
                "/lib",
                "/lib32",
                "/lib64",
                "/libx32",
                "/proc",
                "/sbin",
                "/snap",
                "/sys",
                "/usr",
                "/Applications",
                "/Library",
                "/System",
                "/private/etc",
            ] {
                guard.protect_tree(Path::new(path), "a system folder");
            }
            for path in [
                "/home",
                "/Users",
                "/root",
                "/opt",
                "/var",
                "/private",
                "/private/var",
                "/tmp",
                "/Volumes",
                "/mnt",
                "/media",
                "/run",
            ] {
                guard.protect(Path::new(path), "a system folder");
            }
        }
        guard
    }

    /// Refuse `path` and its ancestors.
    pub fn protect(&mut self, path: &Path, why: &'static str) {
        self.exact
            .push((canonical(path).unwrap_or_else(|| path.into()), why));
    }

    /// Refuse `path`, its ancestors and everything inside it.
    pub fn protect_tree(&mut self, path: &Path, why: &'static str) {
        self.trees
            .push((canonical(path).unwrap_or_else(|| path.into()), why));
    }

    /// Why `path` (canonical) is protected, if it is.
    fn refuses(&self, path: &Path) -> Option<String> {
        if is_volume_root(path) {
            return Some("is the top of a drive or volume".into());
        }
        for (protected, why) in self.exact.iter().chain(&self.trees) {
            if within(protected, path) {
                return Some(if same(protected, path) {
                    format!("is {why}")
                } else {
                    format!("contains {why}")
                });
            }
        }
        for (protected, why) in &self.trees {
            if within(path, protected) {
                return Some(format!("is inside {why}"));
            }
        }
        if self.exe.as_deref().is_some_and(|exe| within(exe, path)) {
            return Some("contains the running RillDisk program".into());
        }
        None
    }
}

/// Reasons that do not depend on the filesystem's current state. Used to keep
/// such candidates out of "select all" and to explain why they cannot be removed.
pub fn static_refusal(snapshot: &Snapshot, candidate: &Candidate, guard: &Guard) -> Option<String> {
    if candidate.entry_id == 0 {
        return Some("is the scanned folder itself".into());
    }
    if candidate.category == Category::RecycleBin {
        return Some(format!(
            "is a Trash folder; empty the {} from your file manager",
            CleanupMode::trash_name()
        ));
    }
    let Some(entry) = snapshot.entries.get(candidate.entry_id) else {
        return Some("is not part of this scan".into());
    };
    if snapshot.path(candidate.entry_id) != candidate.path {
        return Some("does not match the current scan; rescan first".into());
    }
    if entry.kind != EntryKind::Directory {
        return Some("is not a folder in this scan".into());
    }
    guard.refuses(&candidate.path)
}

/// Entry IDs that "Select all safe" picks: outermost Regenerable and Cache
/// candidates that pass the static checks. Review candidates are never included.
pub fn select_safe(snapshot: &Snapshot, report: &ReclaimReport, guard: &Guard) -> Vec<usize> {
    let ok: Vec<&Candidate> = report
        .candidates
        .iter()
        .filter(|c| matches!(c.safety, Safety::Regenerable | Safety::Cache))
        .filter(|c| static_refusal(snapshot, c, guard).is_none())
        .collect();
    let ids: HashSet<usize> = ok.iter().map(|c| c.entry_id).collect();
    ok.iter()
        .filter(|c| !has_selected_ancestor(snapshot, c.entry_id, &ids))
        .map(|c| c.entry_id)
        .collect()
}

fn has_selected_ancestor(snapshot: &Snapshot, id: usize, selected: &HashSet<usize>) -> bool {
    let mut cursor = snapshot.entries[id].parent_id;
    while let Some(parent) = cursor {
        if selected.contains(&parent) {
            return true;
        }
        cursor = snapshot.entries[parent].parent_id;
    }
    false
}

/// Turn selected entry IDs into a plan. Nothing is touched.
pub fn plan(
    snapshot: &Snapshot,
    report: &ReclaimReport,
    selected: &[usize],
    mode: CleanupMode,
    guard: &Guard,
) -> CleanupPlan {
    let mut refused = Vec::new();
    let mut accepted: Vec<&Candidate> = Vec::new();
    let mut seen = HashSet::new();
    for &id in selected {
        if !seen.insert(id) {
            continue;
        }
        let Some(candidate) = report.candidates.iter().find(|c| c.entry_id == id) else {
            refused.push(Refusal {
                entry_id: id,
                path: PathBuf::new(),
                reason: "is not a reclaim candidate of this scan".into(),
            });
            continue;
        };
        match static_refusal(snapshot, candidate, guard) {
            Some(reason) => refused.push(Refusal {
                entry_id: id,
                path: candidate.path.clone(),
                reason,
            }),
            None => accepted.push(candidate),
        }
    }
    let ids: HashSet<usize> = accepted.iter().map(|c| c.entry_id).collect();
    let before = accepted.len();
    accepted.retain(|c| !has_selected_ancestor(snapshot, c.entry_id, &ids));
    let covered = before - accepted.len();
    accepted.sort_by(|a, b| {
        b.exclusive_bytes
            .cmp(&a.exclusive_bytes)
            .then(a.entry_id.cmp(&b.entry_id))
    });
    let items: Vec<PlanItem> = accepted
        .into_iter()
        .map(|c| PlanItem {
            entry_id: c.entry_id,
            path: c.path.clone(),
            category: c.category,
            safety: c.safety,
            label: c.label,
            logical_bytes: c.logical_bytes,
            exclusive_bytes: c.exclusive_bytes,
            file_count: c.file_count,
        })
        .collect();
    CleanupPlan {
        mode,
        total_logical_bytes: items.iter().map(|i| i.logical_bytes).sum(),
        total_exclusive_bytes: items.iter().map(|i| i.exclusive_bytes).sum(),
        total_files: items.iter().map(|i| i.file_count).sum(),
        items,
        refused,
        covered,
    }
}

/// Remove the selected candidates. Call only after the user confirmed the plan
/// built from the same arguments. Continues past errors; stops between items
/// when cancelled.
pub fn execute(
    snapshot: &Snapshot,
    report: &ReclaimReport,
    selected: &[usize],
    mode: CleanupMode,
    guard: &Guard,
    control: &CleanupControl,
) -> CleanupReport {
    execute_with(
        snapshot,
        report,
        selected,
        mode,
        guard,
        control,
        &mut remove,
    )
}

/// [`execute`] with a replaceable removal step, so tests can inject failures.
#[doc(hidden)]
pub fn execute_with(
    snapshot: &Snapshot,
    report: &ReclaimReport,
    selected: &[usize],
    mode: CleanupMode,
    guard: &Guard,
    control: &CleanupControl,
    remover: &mut dyn FnMut(&Path, CleanupMode) -> Result<(), String>,
) -> CleanupReport {
    let plan = plan(snapshot, report, selected, mode, guard);
    control.total.store(plan.items.len(), Ordering::Relaxed);
    control.done.store(0, Ordering::Relaxed);
    control.freed.store(0, Ordering::Relaxed);
    let mut items = Vec::with_capacity(plan.items.len());
    let mut cancelled = false;
    for item in plan.items {
        if control.is_cancelled() {
            cancelled = true;
            items.push(ItemResult {
                item,
                outcome: Outcome::NotRun,
            });
            continue;
        }
        if let Ok(mut current) = control.current.lock() {
            current.clone_from(&item.path);
        }
        let outcome = match live_refusal(&item.path, guard) {
            Some(reason) => Outcome::Refused { reason },
            None => match remover(&item.path, mode) {
                Ok(()) => Outcome::Removed {
                    freed_bytes: item.exclusive_bytes,
                },
                // The OS may report an error after the item is already gone.
                Err(_) if matches!(std::fs::symlink_metadata(&item.path), Err(e) if e.kind() == io::ErrorKind::NotFound) => {
                    Outcome::Removed {
                        freed_bytes: item.exclusive_bytes,
                    }
                }
                Err(error) => Outcome::Failed {
                    error,
                    // Trash moves are one rename on most systems, but Windows
                    // may recycle part of a folder before failing.
                    maybe_partial: mode == CleanupMode::Permanent || cfg!(windows),
                },
            },
        };
        if let Outcome::Removed { freed_bytes } = outcome {
            control.freed.fetch_add(freed_bytes, Ordering::Relaxed);
        }
        control.done.fetch_add(1, Ordering::Relaxed);
        items.push(ItemResult { item, outcome });
    }
    if let Ok(mut current) = control.current.lock() {
        *current = PathBuf::new();
    }
    let removed: Vec<&ItemResult> = items
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Removed { .. }))
        .collect();
    let mut follow_ups = Vec::new();
    let worktrees = removed
        .iter()
        .filter(|r| r.item.category == Category::GitWorktree)
        .count();
    if worktrees > 0 {
        follow_ups.push(format!(
            "{worktrees} Git worktree folder(s) removed. Their repositories still list them: once you are sure you will not restore them{}, run `git worktree prune` in each original repository. RillDisk does not run Git.",
            if mode == CleanupMode::Trash {
                format!(" from the {}", CleanupMode::trash_name())
            } else {
                String::new()
            }
        ));
    }
    if mode == CleanupMode::Trash && !removed.is_empty() {
        follow_ups.push(format!(
            "Space is reclaimed when you empty the {}.",
            CleanupMode::trash_name()
        ));
    }
    CleanupReport {
        mode,
        removed: removed.len(),
        freed_bytes: removed
            .iter()
            .map(|r| match r.outcome {
                Outcome::Removed { freed_bytes } => freed_bytes,
                _ => 0,
            })
            .sum(),
        failed: items
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Failed { .. }))
            .count(),
        items,
        refused: plan.refused,
        cancelled,
        follow_ups,
    }
}

/// Checks against the filesystem as it is now, right before removal.
fn live_refusal(path: &Path, guard: &Guard) -> Option<String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Some("no longer exists".into());
        }
        Err(e) => return Some(format!("cannot be inspected: {}", describe_io(&e))),
    };
    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Some("is now a link, junction or mount point; links are never followed".into());
    }
    if !metadata.is_dir() {
        return Some("is no longer a folder".into());
    }
    // A link anywhere above the path would redirect the removal elsewhere.
    match canonical(path) {
        Some(real) if same(&real, path) => {}
        Some(_) => {
            return Some(
                "now resolves to a different location (a link or junction above it)".into(),
            );
        }
        None => return Some("cannot be resolved".into()),
    }
    if is_mount_point(path, &metadata) {
        return Some("is a mount point of another filesystem".into());
    }
    guard.refuses(path)
}

/// Move to the Trash or delete. Never follows links inside the folder.
fn remove(path: &Path, mode: CleanupMode) -> Result<(), String> {
    match mode {
        CleanupMode::Permanent => std::fs::remove_dir_all(path).map_err(|e| describe_io(&e)),
        CleanupMode::Trash => {
            #[allow(unused_mut)]
            let mut context = trash::TrashContext::default();
            // The default asks Finder through `osascript`, which would run another
            // program. NSFileManager does the same move in-process.
            #[cfg(target_os = "macos")]
            {
                use trash::macos::{DeleteMethod, TrashContextExtMacos};
                context.set_delete_method(DeleteMethod::NsFileManager);
            }
            // The platform layer can panic on unexpected system state; report it
            // as a failure of this item instead of ending the whole cleanup.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| context.delete(path)))
                .map_err(|_| format!("the {} rejected this item", CleanupMode::trash_name()))?
                .map_err(|e| describe_trash(&e))
        }
    }
}

fn describe_trash(error: &trash::Error) -> String {
    match error {
        #[cfg(windows)]
        trash::Error::Os { code, .. }
            if (*code as u32 & 0xFFFF) == 32 || (*code as u32 & 0xFFFF) == 33 =>
        {
            "in use by another program; close it and try again".into()
        }
        #[cfg(windows)]
        trash::Error::Os { code, .. } if (*code as u32 & 0xFFFF) == 5 => "access denied".into(),
        trash::Error::Unknown { description } if description.contains("aborted") => {
            "the system stopped the move (a file may be in use or protected)".into()
        }
        other => format!(
            "could not move to the {}: {other}",
            CleanupMode::trash_name()
        ),
    }
}

fn describe_io(error: &io::Error) -> String {
    #[cfg(windows)]
    match error.raw_os_error() {
        Some(32 | 33) => return "in use by another program; close it and try again".into(),
        Some(5) => return "access denied".into(),
        _ => {}
    }
    match error.kind() {
        io::ErrorKind::PermissionDenied => "access denied".into(),
        io::ErrorKind::NotFound => "no longer exists".into(),
        io::ErrorKind::ReadOnlyFilesystem => "the drive is read-only".into(),
        io::ErrorKind::ResourceBusy => "in use by another program".into(),
        _ => error.to_string(),
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn is_mount_point(path: &Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    match path.parent().map(std::fs::symlink_metadata) {
        Some(Ok(parent)) => parent.dev() != metadata.dev(),
        _ => true,
    }
}

#[cfg(not(unix))]
fn is_mount_point(_: &Path, _: &std::fs::Metadata) -> bool {
    // Windows mount points are reparse points, refused above.
    false
}

fn canonical(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok().map(crate::simplify)
}

fn is_volume_root(path: &Path) -> bool {
    path.parent().is_none() || crate::reclaim::volume_root_path(path)
}

/// Path components compared the way the platform's default filesystem does.
fn parts(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| {
            let text = c.as_os_str().to_string_lossy();
            if cfg!(any(windows, target_os = "macos")) {
                text.to_lowercase()
            } else {
                text.into_owned()
            }
        })
        .collect()
}

fn same(a: &Path, b: &Path) -> bool {
    parts(a) == parts(b)
}

/// `inner` is `outer` or below it.
fn within(inner: &Path, outer: &Path) -> bool {
    let (inner, outer) = (parts(inner), parts(outer));
    inner.len() >= outer.len() && inner[..outer.len()] == outer[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_refuses_protected_paths_their_ancestors_and_system_trees() {
        let base = tempfile::tempdir().unwrap();
        let home = base.path().join("home");
        let system = base.path().join("sys");
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::create_dir_all(system.join("cache")).unwrap();
        let mut guard = Guard {
            exact: Vec::new(),
            trees: Vec::new(),
            exe: Some(
                canonical(base.path())
                    .unwrap()
                    .join("tools")
                    .join("rilldisk"),
            ),
        };
        guard.protect(&home, "your home folder");
        guard.protect_tree(&system, "a system folder");
        let c = |p: &Path| canonical(p).unwrap();
        assert!(guard.refuses(&c(&home)).unwrap().contains("home"));
        assert!(guard.refuses(&c(base.path())).is_some());
        assert!(guard.refuses(&c(&home.join("projects"))).is_none());
        assert!(
            guard
                .refuses(&c(&system.join("cache")))
                .unwrap()
                .contains("inside")
        );
        std::fs::create_dir_all(base.path().join("tools")).unwrap();
        assert!(
            guard
                .refuses(&c(&base.path().join("tools")))
                .unwrap()
                .contains("running")
        );
    }

    #[test]
    fn volume_roots_are_refused() {
        let guard = Guard {
            exact: Vec::new(),
            trees: Vec::new(),
            exe: None,
        };
        #[cfg(windows)]
        for root in [r"C:\", r"D:\", r"\\server\share\"] {
            assert!(guard.refuses(Path::new(root)).is_some(), "{root}");
        }
        #[cfg(unix)]
        for root in ["/", "/Volumes/USB", "/mnt/data"] {
            assert!(guard.refuses(Path::new(root)).is_some(), "{root}");
        }
    }

    #[test]
    fn system_guard_protects_this_machine() {
        let guard = Guard::system();
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap();
        let home = canonical(Path::new(&home)).unwrap();
        assert!(guard.refuses(&home).is_some());
        assert!(
            guard
                .refuses(&canonical(&std::env::temp_dir()).unwrap())
                .is_some()
        );
        #[cfg(windows)]
        {
            assert!(guard.refuses(Path::new(r"C:\Windows\Temp")).is_some());
            assert!(
                guard
                    .refuses(Path::new(r"C:\Program Files\App\cache"))
                    .is_some()
            );
            assert!(guard.refuses(Path::new(r"C:\Users")).is_some());
        }
        #[cfg(unix)]
        {
            assert!(guard.refuses(Path::new("/usr/lib/node_modules")).is_some());
            assert!(guard.refuses(Path::new("/var")).is_some());
        }
    }
}
