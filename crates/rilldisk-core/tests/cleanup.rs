// SPDX-License-Identifier: AGPL-3.0-only
//! Cleanup safety rules, on synthetic temp folders only.
use rilldisk_core::{
    Candidate, Category, CleanupMode, ReclaimReport, Safety, ScanOptions, Snapshot,
    cleanup::{self, CleanupControl, Guard, Outcome},
    find_reclaimable, scan,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7_u8; bytes]).unwrap();
}

/// Two Rust projects, a Node project with a nested candidate, a Python cache and
/// a linked worktree. Returns the canonical root.
fn fixture(root: &Path) -> PathBuf {
    write(&root.join("app/Cargo.toml"), 10);
    write(&root.join("app/src/main.rs"), 100);
    write(&root.join("app/target/debug/app.bin"), 40_000);
    write(&root.join("lib/Cargo.toml"), 10);
    write(&root.join("lib/target/release/lib.rlib"), 30_000);
    write(&root.join("web/package.json"), 10);
    write(&root.join("web/node_modules/a/index.js"), 20_000);
    write(&root.join("web/node_modules/a/package.json"), 10);
    write(&root.join("py/pkg/__pycache__/m.pyc"), 900);
    write(&root.join("wt/feature/.git"), 40);
    write(&root.join("wt/feature/notes.md"), 500);
    rilldisk_core::simplify(root.canonicalize().unwrap())
}

fn analyze(root: &Path) -> (Snapshot, ReclaimReport) {
    let snapshot = scan(root, ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete, "{:?}", snapshot.summary.issues);
    let report = find_reclaimable(&snapshot, 0);
    (snapshot, report)
}

fn id_of(report: &ReclaimReport, path: &Path) -> usize {
    report
        .candidates
        .iter()
        .find(|c| c.path == path)
        .unwrap_or_else(|| panic!("no candidate {}", path.display()))
        .entry_id
}

fn run(
    snapshot: &Snapshot,
    report: &ReclaimReport,
    ids: &[usize],
    guard: &Guard,
) -> cleanup::CleanupReport {
    cleanup::execute(
        snapshot,
        report,
        ids,
        CleanupMode::Permanent,
        guard,
        &CleanupControl::default(),
    )
}

fn refusal(result: &cleanup::CleanupReport) -> &str {
    match &result.items[0].outcome {
        Outcome::Refused { reason } => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn permanent_cleanup_removes_only_selected_candidates_and_reports_freed_space() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    // Read-only files (common in .git and node_modules) are removed too.
    let readonly = root.join("app/target/debug/app.bin");
    let mut permissions = fs::metadata(&readonly).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&readonly, permissions).unwrap();
    let (snapshot, report) = analyze(&root);
    let target = id_of(&report, &root.join("app/target"));
    let modules = id_of(&report, &root.join("web/node_modules"));
    let expected: u64 = report
        .candidates
        .iter()
        .filter(|c| c.entry_id == target || c.entry_id == modules)
        .map(|c| c.exclusive_bytes)
        .sum();
    let control = CleanupControl::default();
    let result = cleanup::execute(
        &snapshot,
        &report,
        &[target, modules],
        CleanupMode::Permanent,
        &Guard::built_in(),
        &control,
    );
    assert_eq!(result.removed, 2, "{result:#?}");
    assert_eq!(result.failed, 0);
    assert_eq!(result.freed_bytes, expected);
    assert!(expected >= 60_000);
    assert_eq!(control.done.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(
        control.freed.load(std::sync::atomic::Ordering::Relaxed),
        expected
    );
    assert!(!root.join("app/target").exists());
    assert!(!root.join("web/node_modules").exists());
    // Everything else is untouched.
    for kept in [
        "app/Cargo.toml",
        "app/src/main.rs",
        "lib/target/release/lib.rlib",
        "web/package.json",
        "py/pkg/__pycache__/m.pyc",
        "wt/feature/notes.md",
    ] {
        assert!(root.join(kept).exists(), "{kept}");
    }
    assert!(
        result
            .follow_ups
            .iter()
            .all(|f| !f.contains("git worktree"))
    );
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(json["items"][0]["status"], "removed");
    assert_eq!(json["mode"], "permanent");
}

#[test]
fn refuses_ids_that_are_not_candidates_of_this_report() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let src = (0..snapshot.entries.len())
        .find(|&id| snapshot.path(id) == root.join("app/src"))
        .unwrap();
    let result = run(&snapshot, &report, &[src, 0, 999_999], &Guard::built_in());
    assert_eq!(result.removed, 0);
    assert_eq!(result.refused.len(), 3);
    assert!(
        result
            .refused
            .iter()
            .all(|r| r.reason.contains("not a reclaim candidate"))
    );
    assert!(root.join("app/src/main.rs").exists());

    // A candidate from a different scan (stale report) is refused too.
    let other = tempfile::tempdir().unwrap();
    let other_root = fixture(other.path());
    let (_, other_report) = analyze(&other_root);
    let foreign = id_of(&other_report, &other_root.join("app/target"));
    let mut mixed = report.clone();
    mixed.candidates = other_report.candidates.clone();
    let result = run(&snapshot, &mixed, &[foreign], &Guard::built_in());
    assert_eq!(result.removed, 0);
    assert!(
        result.refused[0].reason.contains("does not match"),
        "{result:#?}"
    );
    assert!(other_root.join("app/target").exists());
}

fn fake_candidate(snapshot: &Snapshot, id: usize, category: Category) -> Candidate {
    Candidate {
        entry_id: id,
        path: snapshot.path(id),
        category,
        safety: Safety::Cache,
        label: "fake",
        agent: None,
        reason: String::new(),
        suggested_action: String::new(),
        logical_bytes: 0,
        unique_logical_bytes: 0,
        allocated_bytes: None,
        file_count: 0,
        exclusive_bytes: 0,
        shared_bytes: 0,
        newest_modified_unix_ms: None,
        idle_days: None,
        nested_in: None,
        git: None,
        identity: None,
    }
}

#[test]
fn refuses_the_scan_root_trash_folders_and_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, mut report) = analyze(&root);
    let file = (0..snapshot.entries.len())
        .find(|&id| snapshot.path(id) == root.join("app/Cargo.toml"))
        .unwrap();
    let py = (0..snapshot.entries.len())
        .find(|&id| snapshot.path(id) == root.join("py"))
        .unwrap();
    report.candidates = vec![
        fake_candidate(&snapshot, 0, Category::BuildOutput),
        fake_candidate(&snapshot, file, Category::BuildOutput),
        fake_candidate(&snapshot, py, Category::RecycleBin),
    ];
    let result = run(&snapshot, &report, &[0, file, py], &Guard::built_in());
    assert_eq!(result.removed, 0);
    let reasons: Vec<&str> = result.refused.iter().map(|r| r.reason.as_str()).collect();
    assert!(reasons[0].contains("scanned folder"), "{reasons:?}");
    assert!(reasons[1].contains("not a folder"), "{reasons:?}");
    assert!(reasons[2].contains("Trash folder"), "{reasons:?}");
    assert!(root.join("app/Cargo.toml").exists());
    assert!(root.join("py/pkg/__pycache__/m.pyc").exists());
}

#[test]
fn refuses_the_home_folder_its_ancestors_and_system_folders() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let target = id_of(&report, &root.join("app/target"));
    let modules = id_of(&report, &root.join("web/node_modules"));
    let cache = id_of(&report, &root.join("py/pkg/__pycache__"));
    let mut guard = Guard::built_in();
    // Pretend the home folder is a candidate, sits inside one, or holds system files.
    guard.protect(&root.join("app/target"), "your home folder");
    guard.protect(&root.join("web/node_modules/a"), "your home folder");
    guard.protect_tree(&root.join("py/pkg"), "a system folder");
    let result = run(&snapshot, &report, &[target, modules, cache], &guard);
    assert_eq!(result.removed, 0, "{result:#?}");
    let reason = |id: usize| {
        result
            .refused
            .iter()
            .find(|r| r.entry_id == id)
            .unwrap()
            .reason
            .clone()
    };
    assert_eq!(reason(target), "is your home folder");
    assert_eq!(reason(modules), "contains your home folder");
    assert_eq!(reason(cache), "is inside a system folder");
    assert!(root.join("app/target/debug/app.bin").exists());
    assert!(root.join("web/node_modules/a/index.js").exists());
    // "Select all safe" leaves protected candidates out.
    let safe = cleanup::select_safe(&snapshot, &report, &guard);
    assert!(!safe.contains(&target) && !safe.contains(&modules) && !safe.contains(&cache));
}

fn link_dir(link: &Path, target: &Path) {
    // `cmd` would read a forward slash as a switch.
    #[cfg(windows)]
    let link = &PathBuf::from(link.to_string_lossy().replace('/', "\\"));
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn refuses_a_candidate_replaced_by_a_link_or_junction_after_the_scan() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let victim = tempfile::tempdir().unwrap();
    write(&victim.path().join("precious.txt"), 100);
    let (snapshot, report) = analyze(&root);
    let modules = id_of(&report, &root.join("web/node_modules"));
    fs::remove_dir_all(root.join("web/node_modules")).unwrap();
    link_dir(&root.join("web/node_modules"), victim.path());
    let result = run(&snapshot, &report, &[modules], &Guard::built_in());
    assert_eq!(result.removed, 0);
    assert!(refusal(&result).contains("link"), "{result:#?}");
    assert!(victim.path().join("precious.txt").exists());
}

/// Permanent removal deletes a link inside a candidate, never what it points to,
/// and the scan never counted the link target as part of the candidate.
#[test]
fn permanent_cleanup_removes_links_inside_a_candidate_but_not_their_targets() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("store/data.bin"), 50_000);
    // pnpm and agent tools link folders into node_modules and worktrees.
    link_dir(
        &root.join("web/node_modules/linked"),
        &outside.path().join("store"),
    );
    let (snapshot, report) = analyze(&root);
    let modules = id_of(&report, &root.join("web/node_modules"));
    let candidate = report
        .candidates
        .iter()
        .find(|c| c.entry_id == modules)
        .unwrap();
    assert_eq!(
        candidate.logical_bytes, 20_010,
        "the link target is not counted"
    );
    let result = run(&snapshot, &report, &[modules], &Guard::built_in());
    assert_eq!(result.removed, 1, "{result:#?}");
    assert!(!root.join("web/node_modules").exists());
    assert_eq!(
        fs::metadata(outside.path().join("store/data.bin"))
            .unwrap()
            .len(),
        50_000
    );
}

/// The user's protected paths hide candidates from suggestions and block their
/// removal, even when a removal request names them directly.
#[test]
fn protected_paths_hide_candidates_and_block_their_removal() {
    use rilldisk_core::protect::Protected;
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let modules = id_of(&report, &root.join("web/node_modules"));
    let target = id_of(&report, &root.join("app/target"));
    let other = id_of(&report, &root.join("lib/target"));
    let mut protected = Protected::default();
    // A whole project, and a folder inside a candidate (the candidate contains it).
    protected.add(&root.join("web")).unwrap();
    protected.add(&root.join("app/target/debug")).unwrap();

    let mut shown = report.clone();
    assert_eq!(shown.exclude_protected(&protected), 2);
    assert_eq!(shown.protected_excluded, 2);
    assert!(
        shown
            .candidates
            .iter()
            .all(|c| c.entry_id != modules && c.entry_id != target)
    );
    assert!(shown.candidates.iter().any(|c| c.entry_id == other));
    assert!(shown.total_exclusive_bytes < report.total_exclusive_bytes);
    assert_eq!(shown.candidate_count, shown.candidates.len());

    let mut guard = Guard::built_in();
    guard.protect_user(&protected);
    assert!(!cleanup::select_safe(&snapshot, &report, &guard).contains(&modules));
    let result = run(&snapshot, &report, &[modules, target, other], &guard);
    assert_eq!(result.removed, 1, "{result:#?}");
    assert_eq!(result.refused.len(), 2);
    for refusal in &result.refused {
        assert!(
            refusal.reason.contains("a path you protected"),
            "{}",
            refusal.reason
        );
    }
    assert!(root.join("web/node_modules/a/index.js").exists());
    assert!(root.join("app/target/debug/app.bin").exists());
    assert!(!root.join("lib/target").exists());
}

#[test]
fn refuses_a_candidate_whose_parent_became_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let elsewhere = tempfile::tempdir().unwrap();
    write(&elsewhere.path().join("target/keep.bin"), 100);
    write(&elsewhere.path().join("Cargo.toml"), 10);
    let (snapshot, report) = analyze(&root);
    let target = id_of(&report, &root.join("lib/target"));
    fs::remove_dir_all(root.join("lib")).unwrap();
    link_dir(&root.join("lib"), elsewhere.path());
    let result = run(&snapshot, &report, &[target], &Guard::built_in());
    assert_eq!(result.removed, 0);
    assert!(
        refusal(&result).contains("different location"),
        "{result:#?}"
    );
    assert!(elsewhere.path().join("target/keep.bin").exists());
}

#[test]
fn refuses_a_different_folder_created_at_a_candidate_path_after_the_scan() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let target = id_of(&report, &root.join("app/target"));
    // The scanned folder moves away and an ordinary folder takes its name: same
    // path, still a real directory, no link anywhere.
    fs::rename(root.join("app/target"), root.join("app/target-moved")).unwrap();
    write(&root.join("app/target/precious.txt"), 10);
    let mut removed = Vec::new();
    let result = cleanup::execute_with(
        &snapshot,
        &report,
        &[target],
        CleanupMode::Permanent,
        &Guard::built_in(),
        &CleanupControl::default(),
        &mut |path, _| {
            removed.push(path.to_path_buf());
            Ok(())
        },
    );
    assert!(
        removed.is_empty(),
        "the replacement folder reached the remover"
    );
    assert!(
        refusal(&result).contains("not the folder that was scanned"),
        "{}",
        refusal(&result)
    );
    assert!(root.join("app/target/precious.txt").exists());
}

#[test]
fn refuses_candidates_that_changed_kind_or_vanished() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let target = id_of(&report, &root.join("app/target"));
    let lib = id_of(&report, &root.join("lib/target"));
    fs::remove_dir_all(root.join("app/target")).unwrap();
    fs::write(root.join("app/target"), b"now a file").unwrap();
    fs::remove_dir_all(root.join("lib/target")).unwrap();
    let result = run(&snapshot, &report, &[target, lib], &Guard::built_in());
    assert_eq!(result.removed, 0);
    let reason = |path: &str| {
        let item = result
            .items
            .iter()
            .find(|r| r.item.path == root.join(path))
            .unwrap();
        match &item.outcome {
            Outcome::Refused { reason } => reason.clone(),
            other => panic!("{other:?}"),
        }
    };
    assert!(reason("app/target").contains("no longer a folder"));
    assert!(reason("lib/target").contains("no longer exists"));
    assert_eq!(fs::read(root.join("app/target")).unwrap(), b"now a file");
}

#[test]
fn keeps_going_after_failures_and_reports_each_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let ids = [
        id_of(&report, &root.join("app/target")),
        id_of(&report, &root.join("lib/target")),
        id_of(&report, &root.join("web/node_modules")),
    ];
    let failing = root.join("lib/target");
    let mut remover = |path: &Path, _: CleanupMode| {
        if path == failing {
            Err("access denied".to_string())
        } else {
            fs::remove_dir_all(path).map_err(|e| e.to_string())
        }
    };
    let result = cleanup::execute_with(
        &snapshot,
        &report,
        &ids,
        CleanupMode::Permanent,
        &Guard::built_in(),
        &CleanupControl::default(),
        &mut remover,
    );
    assert_eq!((result.removed, result.failed), (2, 1), "{result:#?}");
    assert!(result.needs_rescan());
    assert!(root.join("lib/target/release/lib.rlib").exists());
    assert!(!root.join("app/target").exists());
    assert!(!root.join("web/node_modules").exists());
    assert_eq!(result.removed_entry_ids().len(), 2);
}

/// A real operating-system failure: a file held open without delete sharing
/// (Windows) or a parent folder without write permission (Unix).
#[test]
fn reports_real_removal_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let target = id_of(&report, &root.join("app/target"));
    #[cfg(windows)]
    let _held = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(root.join("app/target/debug/app.bin"))
            .unwrap()
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Root ignores permissions; nothing meaningful to test then.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        fs::set_permissions(
            root.join("app/target/debug"),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
    }
    let result = run(&snapshot, &report, &[target], &Guard::built_in());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            root.join("app/target/debug"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    assert_eq!(result.failed, 1, "{result:#?}");
    match &result.items[0].outcome {
        Outcome::Failed {
            error,
            maybe_partial,
        } => {
            #[cfg(windows)]
            assert!(error.contains("in use"), "{error}");
            #[cfg(unix)]
            assert!(error.contains("access denied"), "{error}");
            assert!(maybe_partial);
        }
        other => panic!("{other:?}"),
    }
    assert!(root.join("app/target/debug/app.bin").exists());
}

#[test]
fn cancellation_stops_before_the_next_item() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let ids = [
        id_of(&report, &root.join("app/target")),
        id_of(&report, &root.join("lib/target")),
        id_of(&report, &root.join("web/node_modules")),
    ];
    let control = CleanupControl::default();
    let mut remover = |path: &Path, _: CleanupMode| {
        control.cancel();
        fs::remove_dir_all(path).map_err(|e| e.to_string())
    };
    let result = cleanup::execute_with(
        &snapshot,
        &report,
        &ids,
        CleanupMode::Permanent,
        &Guard::built_in(),
        &control,
        &mut remover,
    );
    assert!(result.cancelled);
    assert_eq!(result.removed, 1);
    assert_eq!(
        result
            .items
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::NotRun))
            .count(),
        2
    );
}

#[test]
fn select_safe_skips_review_items_and_nested_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    write(&root.join("wt/feature/Cargo.toml"), 10);
    write(&root.join("wt/feature/target/x.bin"), 5_000);
    let (snapshot, report) = analyze(&root);
    let safe = cleanup::select_safe(&snapshot, &report, &Guard::built_in());
    let paths: Vec<PathBuf> = safe.iter().map(|&id| snapshot.path(id)).collect();
    assert!(paths.contains(&root.join("app/target")));
    assert!(paths.contains(&root.join("web/node_modules")));
    assert!(paths.contains(&root.join("py/pkg/__pycache__")));
    // The worktree is Review; its build output is still offered on its own.
    assert!(!paths.contains(&root.join("wt/feature")));
    assert!(paths.contains(&root.join("wt/feature/target")));
    for &id in &safe {
        let c = report.candidates.iter().find(|c| c.entry_id == id).unwrap();
        assert_ne!(c.safety, Safety::Review);
    }
    // Selecting a worktree and its nested build output removes the outer one once.
    let worktree = id_of(&report, &root.join("wt/feature"));
    let nested = id_of(&report, &root.join("wt/feature/target"));
    let plan = cleanup::plan(
        &snapshot,
        &report,
        &[nested, worktree],
        CleanupMode::Permanent,
        &Guard::built_in(),
    );
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.covered, 1);
    assert_eq!(plan.items[0].entry_id, worktree);
    let result = run(&snapshot, &report, &[nested, worktree], &Guard::built_in());
    assert_eq!(result.removed, 1);
    assert!(
        result
            .follow_ups
            .iter()
            .any(|f| f.contains("git worktree prune")),
        "{:?}",
        result.follow_ups
    );
    assert!(!root.join("wt/feature").exists());
}

#[test]
fn snapshot_without_counts_a_nested_removal_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, _) = analyze(&root);
    let id = |path: &str| {
        (0..snapshot.entries.len())
            .find(|&id| snapshot.path(id) == root.join(path))
            .unwrap()
    };
    let outer = snapshot.without(&[id("app")]);
    // Also naming an entry two levels inside it changes nothing.
    let both = snapshot.without(&[id("app"), id("app/target/debug")]);
    assert_eq!(both.summary.logical_bytes, outer.summary.logical_bytes);
    assert_eq!(both.summary.files, outer.summary.files);
    assert_eq!(
        both.entries[0].logical_bytes,
        outer.entries[0].logical_bytes
    );
}

#[test]
fn snapshot_without_removed_subtrees_matches_a_rescan() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let ids = [
        id_of(&report, &root.join("app/target")),
        id_of(&report, &root.join("web/node_modules")),
    ];
    let result = run(&snapshot, &report, &ids, &Guard::built_in());
    assert_eq!(result.removed, 2);
    let adjusted = snapshot.without(&result.removed_entry_ids());
    let (fresh, _) = analyze(&root);
    let (a, f) = (&adjusted.summary, &fresh.summary);
    assert_eq!(a.logical_bytes, f.logical_bytes);
    assert_eq!(a.unique_logical_bytes, f.unique_logical_bytes);
    assert_eq!(a.allocated_bytes, f.allocated_bytes);
    assert_eq!((a.files, a.directories), (f.files, f.directories));
    assert_eq!(adjusted.entries.len(), fresh.entries.len());
    // Every remaining path, size and parent link agrees with the rescan.
    let by_path = |s: &Snapshot| {
        let mut rows: Vec<(PathBuf, u64, u64, u64)> = (0..s.entries.len())
            .map(|id| {
                let e = &s.entries[id];
                (
                    s.path(id),
                    e.logical_bytes,
                    e.file_count,
                    e.unique_logical_bytes,
                )
            })
            .collect();
        rows.sort();
        rows
    };
    assert_eq!(by_path(&adjusted), by_path(&fresh));
    for (id, entry) in adjusted.entries.iter().enumerate() {
        assert_eq!(entry.id, id);
        if let Some(parent) = entry.parent_id {
            assert!(parent < id);
            assert!(adjusted.children(parent).contains(&(id as u32)));
        }
    }
    // The reclaim view recomputed from the adjusted snapshot drops removed folders.
    let after = find_reclaimable(&adjusted, 0);
    assert!(
        after
            .candidates
            .iter()
            .all(|c| c.path != root.join("app/target"))
    );
    assert!(
        after
            .candidates
            .iter()
            .any(|c| c.path == root.join("lib/target"))
    );
}

/// Moves a synthetic folder to the real Recycle Bin / Trash, then purges that one
/// item where the platform allows it. Skipped (with a note) when this machine has
/// no usable Trash for the temp folder, as on some CI containers.
#[test]
fn trash_mode_moves_the_folder_to_the_trash() {
    let dir = tempfile::tempdir().unwrap();
    let root = fixture(dir.path());
    let (snapshot, report) = analyze(&root);
    let target = id_of(&report, &root.join("lib/target"));
    // Identifies this test's item in the Trash (purged below where supported).
    #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
    let unique = root.file_name().unwrap().to_owned();
    let result = cleanup::execute(
        &snapshot,
        &report,
        &[target],
        CleanupMode::Trash,
        &Guard::built_in(),
        &CleanupControl::default(),
    );
    match &result.items[0].outcome {
        Outcome::Removed { freed_bytes } => {
            assert!(*freed_bytes >= 30_000);
            assert!(!root.join("lib/target").exists());
            assert!(result.follow_ups.iter().any(|f| f.contains("empty the")));
            #[cfg(any(windows, all(unix, not(target_os = "macos"))))]
            {
                let ours: Vec<_> = trash::os_limited::list()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|item| {
                        item.original_parent
                            .to_string_lossy()
                            .contains(&*unique.to_string_lossy())
                    })
                    .collect();
                let _ = trash::os_limited::purge_all(ours);
            }
        }
        Outcome::Failed { error, .. } => {
            eprintln!("skipped: no usable Trash here ({error})");
            assert!(root.join("lib/target/release/lib.rlib").exists());
        }
        other => panic!("{other:?}"),
    }
}
