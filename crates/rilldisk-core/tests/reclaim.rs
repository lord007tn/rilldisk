// SPDX-License-Identifier: AGPL-3.0-only
use rilldisk_core::{
    Category, Safety, ScanOptions, file_types, find_reclaimable, largest_files, scan,
};
use std::{fs, path::Path, sync::Arc};

fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7_u8; bytes]).unwrap();
}

#[test]
fn finds_regenerable_folders_worktrees_and_caches_without_false_positives() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    // Rust project with build output.
    write(&r.join("app/Cargo.toml"), 10);
    write(&r.join("app/target/debug/app.exe"), 40_000);
    // Node project with nested node_modules (reported once).
    write(&r.join("web/package.json"), 10);
    write(&r.join("web/node_modules/a/index.js"), 20_000);
    write(&r.join("web/node_modules/a/package.json"), 10);
    write(&r.join("web/node_modules/a/node_modules/b/index.js"), 5_000);
    write(&r.join("web/.next/cache/x"), 3_000);
    // Linked worktree containing its own build output.
    write(&r.join("wt/feature/.git"), 40);
    write(&r.join("wt/feature/Cargo.toml"), 10);
    write(&r.join("wt/feature/src/main.rs"), 100);
    write(&r.join("wt/feature/target/release/big.bin"), 60_000);
    // Submodule checkout: also a .git file, but declared by .gitmodules.
    fs::create_dir_all(r.join("parent/.git")).unwrap();
    write(&r.join("parent/.gitmodules"), 10);
    write(&r.join("parent/vendor/lib/.git"), 40);
    // Git keeps the submodule's repository under .git/modules/<path>.
    write(&r.join("parent/.git/modules/vendor/lib/HEAD"), 41);
    // Python environment and cache.
    write(&r.join("py/.venv/pyvenv.cfg"), 10);
    write(&r.join("py/.venv/Lib/site.py"), 7_000);
    write(&r.join("py/pkg/__pycache__/m.pyc"), 900);
    // Look-alikes that must not be flagged.
    write(&r.join("docs/target/report.pdf"), 1_000);
    write(&r.join("docs/node_modules/readme.txt"), 1_000);
    write(&r.join("data/venv/notes.txt"), 1_000);
    // Agent worktrees folder: one tracked checkout, one orphaned copy without .git.
    write(&r.join("repo/.claude/worktrees/agent-1/.git"), 40);
    write(&r.join("repo/.claude/worktrees/agent-1/README.md"), 10);
    write(&r.join("repo/.claude/worktrees/wf_1/package.json"), 10);
    write(
        &r.join("repo/.claude/worktrees/wf_1/node_modules/x/index.js"),
        9_000,
    );
    // Well-known cache location.
    write(
        &r.join("Users/me/AppData/Local/npm-cache/_cacache/blob"),
        8_000,
    );
    // The same layout somewhere else may be a backup: Review, not Cache.
    write(&r.join("backup/AppData/Local/Temp/old.tmp"), 8_000);
    // IDE build folders need their CMake marker.
    write(&r.join("cpp/CMakeLists.txt"), 10);
    write(&r.join("cpp/cmake-build-debug/app.obj"), 4_000);
    write(&r.join("notes/cmake-build-ideas/plan.txt"), 4_000);

    let snapshot = scan(r, ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete, "{:?}", snapshot.summary.issues);
    let report = find_reclaimable(&snapshot, u64::MAX / 2);
    let find = |suffix: &str| {
        report
            .candidates
            .iter()
            .find(|c| c.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("missing {suffix}: {:#?}", report.candidates))
    };
    let target = find("app/target");
    assert_eq!(target.category, Category::BuildOutput);
    assert_eq!(target.safety, Safety::Regenerable);
    assert_eq!(target.logical_bytes, 40_000);
    assert_eq!(target.file_count, 1);
    assert!(
        target
            .suggested_action
            .starts_with("cargo clean --manifest-path \""),
        "{}",
        target.suggested_action
    );
    assert!(target.suggested_action.ends_with("Cargo.toml\""));

    let modules = find("web/node_modules");
    assert_eq!(modules.category, Category::Dependencies);
    assert_eq!(modules.logical_bytes, 25_010);
    assert!(
        !report
            .candidates
            .iter()
            .any(|c| c.path.ends_with("a/node_modules")),
        "nested dependency folders add no decision"
    );
    find("web/.next");

    let worktree = find("wt/feature");
    assert_eq!(worktree.category, Category::GitWorktree);
    assert_eq!(worktree.safety, Safety::Review);
    assert!(
        worktree
            .suggested_action
            .starts_with("git worktree remove \"")
    );
    assert!(worktree.suggested_action.ends_with('"'), "pasteable as-is");
    let inner = find("wt/feature/target");
    assert_eq!(inner.nested_in, Some(worktree.entry_id));

    assert!(
        !report
            .candidates
            .iter()
            .any(|c| c.path.ends_with("vendor/lib"))
    );
    let tracked = find("worktrees/agent-1");
    assert_eq!(tracked.label, "Git worktree");
    assert!(tracked.reason.contains("Claude Code"), "{}", tracked.reason);
    assert_eq!(tracked.agent, Some("Claude Code"));
    let orphan = find("worktrees/wf_1");
    assert_eq!(orphan.label, "Orphaned agent worktree");
    assert_eq!(orphan.agent, Some("Claude Code"));
    assert_eq!(orphan.safety, Safety::Review);
    assert!(orphan.suggested_action.contains("git worktree prune"));
    assert_eq!(find("wf_1/node_modules").nested_in, Some(orphan.entry_id));
    assert_eq!(find("py/.venv").category, Category::Dependencies);
    find("pkg/__pycache__");
    let npm = find("Local/npm-cache");
    assert_eq!(npm.suggested_action, "npm cache clean --force");
    assert_eq!(npm.safety, Safety::Cache);
    let copy = find("backup/AppData/Local/Temp");
    assert_eq!(copy.safety, Safety::Review);
    assert!(copy.reason.contains("copy or backup"));
    find("cpp/cmake-build-debug");
    assert!(
        !report
            .candidates
            .iter()
            .any(|c| c.path.ends_with("cmake-build-ideas"))
    );
    for false_positive in ["docs/target", "docs/node_modules", "data/venv"] {
        assert!(
            !report
                .candidates
                .iter()
                .any(|c| c.path.ends_with(false_positive)),
            "{false_positive} must not be flagged"
        );
    }
    // Totals count only outermost candidates.
    let outer: u64 = report
        .candidates
        .iter()
        .filter(|c| c.nested_in.is_none())
        .map(|c| c.logical_bytes)
        .sum();
    assert_eq!(report.total_logical_bytes, outer);
    assert!(report.total_logical_bytes < snapshot.summary.logical_bytes);
    assert!(worktree.idle_days.is_some());
}

#[test]
fn summarizes_types_largest_files_and_children() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("a/movie.MP4"), 9_000);
    write(&root.path().join("a/clip.mp4"), 1_000);
    write(&root.path().join("b/notes"), 50);
    write(&root.path().join("b/c.txt"), 500);
    let snapshot = scan(root.path(), ScanOptions::default(), Arc::default()).unwrap();
    let types = file_types(&snapshot);
    assert_eq!(types[0].extension, "mp4");
    assert_eq!(types[0].files, 2);
    assert_eq!(types[0].logical_bytes, 10_000);
    assert!(types.iter().any(|t| t.extension.is_empty() && t.files == 1));
    let largest = largest_files(&snapshot, 2, false);
    assert_eq!(largest.len(), 2);
    assert_eq!(snapshot.entries[largest[0]].logical_bytes, 9_000);
    // Children are sorted by logical size, largest first.
    let top = snapshot.children(0);
    assert_eq!(top.len(), 2);
    assert_eq!(snapshot.name(top[0] as usize), "a");
    assert_eq!(snapshot.entries[0].file_count, 4);
    assert!(snapshot.entries[0].modified_unix_ms.is_some());
    assert!(snapshot.children(usize::MAX - 1).is_empty());
}

#[test]
fn hard_linked_dependencies_do_not_count_as_freed_space() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    // A pnpm-style store outside the project, hard-linked into node_modules.
    write(&r.join("store/pkg/index.js"), 10_000);
    write(&r.join("web/package.json"), 10);
    fs::create_dir_all(r.join("web/node_modules/pkg")).unwrap();
    fs::hard_link(
        r.join("store/pkg/index.js"),
        r.join("web/node_modules/pkg/index.js"),
    )
    .unwrap();
    write(&r.join("web/node_modules/own.js"), 3_000);
    let snapshot = scan(r, ScanOptions::default(), Arc::default()).unwrap();
    let report = find_reclaimable(&snapshot, 0);
    let modules = report
        .candidates
        .iter()
        .find(|c| c.path.ends_with("web/node_modules"))
        .expect("node_modules candidate");
    assert_eq!(modules.logical_bytes, 13_000);
    assert_eq!(modules.shared_bytes, 10_000);
    assert!(modules.exclusive_bytes >= 3_000 && modules.exclusive_bytes < 10_000);
    assert_eq!(report.total_exclusive_bytes, modules.exclusive_bytes);
    let linked = snapshot.entries.iter().filter(|e| e.multiple_links).count();
    assert_eq!(linked, 2, "both names of the hard-linked file are marked");
}

#[test]
fn orphan_rule_needs_a_real_agent_root_and_a_complete_listing() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    // Tracked worktrees: a partial scan must not call them orphaned.
    for i in 1..=3 {
        write(&r.join(format!(".codex/worktrees/w{i}/.git")), 40);
        write(&r.join(format!(".codex/worktrees/w{i}/work.txt")), 10);
    }
    let partial = scan(
        r,
        ScanOptions {
            max_entries: 6,
            ..Default::default()
        },
        Arc::default(),
    )
    .unwrap();
    assert!(!partial.summary.complete);
    let report = find_reclaimable(&partial, 0);
    assert!(
        !report
            .candidates
            .iter()
            .any(|c| c.label.starts_with("Orphaned")),
        "{:#?}",
        report.candidates
    );

    // Source folders named `worktrees` inside a checkout are not agent roots.
    let other = tempfile::tempdir().unwrap();
    let o = other.path();
    write(&o.join(".claude/worktrees/feat/.git"), 40);
    write(
        &o.join(".claude/worktrees/feat/src/worktrees/module/code.rs"),
        500,
    );
    write(&o.join("repo/.git/worktrees/old/HEAD"), 41);
    let snapshot = scan(o, ScanOptions::default(), Arc::default()).unwrap();
    assert!(snapshot.summary.complete);
    assert!(snapshot.entries.iter().all(|e| e.fully_listed));
    let report = find_reclaimable(&snapshot, 0);
    assert!(
        !report
            .candidates
            .iter()
            .any(|c| c.label.starts_with("Orphaned")),
        "{:#?}",
        report.candidates
    );
    assert!(
        report
            .candidates
            .iter()
            .any(|c| c.label == "Git worktree" && c.path.ends_with("worktrees/feat"))
    );
}

#[test]
fn agent_worktrees_are_found_inside_repositories_with_submodules() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    fs::create_dir_all(r.join("repo/.git")).unwrap();
    write(&r.join("repo/.gitmodules"), 10);
    write(&r.join("repo/.claude/worktrees/agent-1/.git"), 40);
    let snapshot = scan(r, ScanOptions::default(), Arc::default()).unwrap();
    let report = find_reclaimable(&snapshot, 0);
    assert!(
        report
            .candidates
            .iter()
            .any(|c| c.label == "Git worktree" && c.path.ends_with("worktrees/agent-1"))
    );
}

#[test]
fn review_candidates_inside_regenerable_folders_are_kept() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    write(&r.join("web/package.json"), 10);
    write(&r.join("web/node_modules/dep/index.js"), 1_000);
    // A worktree someone created inside node_modules still needs a decision.
    write(&r.join("web/node_modules/scratch/.git"), 40);
    write(&r.join("web/node_modules/scratch/notes.md"), 500);
    let snapshot = scan(r, ScanOptions::default(), Arc::default()).unwrap();
    let report = find_reclaimable(&snapshot, 0);
    let modules = report
        .candidates
        .iter()
        .find(|c| c.path.ends_with("web/node_modules"))
        .unwrap();
    let scratch = report
        .candidates
        .iter()
        .find(|c| c.path.ends_with("node_modules/scratch"))
        .expect("review candidate kept");
    assert_eq!(scratch.nested_in, Some(modules.entry_id));
    // Totals still count the outer folder only.
    assert_eq!(report.total_logical_bytes, modules.logical_bytes);
}

#[test]
fn retain_recomputes_totals() {
    let root = tempfile::tempdir().unwrap();
    let r = root.path();
    write(&r.join("a/Cargo.toml"), 10);
    write(&r.join("a/target/big.bin"), 50_000);
    write(&r.join("b/package.json"), 10);
    write(&r.join("b/node_modules/x.js"), 1_000);
    let snapshot = scan(r, ScanOptions::default(), Arc::default()).unwrap();
    let mut report = find_reclaimable(&snapshot, 0);
    assert_eq!(report.candidate_count, 2);
    report.retain(|c| c.logical_bytes >= 10_000);
    assert_eq!(report.candidate_count, 1);
    assert_eq!(report.total_logical_bytes, 50_000);
}
