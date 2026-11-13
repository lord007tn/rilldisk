// SPDX-License-Identifier: AGPL-3.0-only
//! Git inspection of worktree candidates against real repositories. Skipped when
//! `git` is not installed. The test builds its repositories with `git`; RillDisk
//! itself only reads them.
use rilldisk_core::{
    ScanControl, ScanOptions, find_reclaimable,
    git::{self, Verdict},
    scan,
};
use std::{path::Path, process::Command, sync::Arc};

const DAY: u64 = 86_400_000;

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "RillDisk test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "RillDisk test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// `repo` with one commit, and linked worktrees `clean` (branch), `dirty`
/// (untracked file), `detached` (a commit only there) and `locked`.
fn fixture(root: &Path) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    for (name, extra) in [
        ("clean", vec!["-b", "feat-clean"]),
        ("dirty", vec!["-b", "feat-dirty"]),
        ("detached", vec!["--detach"]),
        ("locked", vec!["-b", "feat-locked"]),
    ] {
        let path = root.join("wt").join(name);
        let mut args = vec!["worktree", "add", "-q"];
        args.extend(extra);
        let path_text = path.to_str().unwrap().to_string();
        args.push(&path_text);
        git(&repo, &args);
    }
    std::fs::write(root.join("wt/dirty/notes.txt"), "unsaved\n").unwrap();
    std::fs::write(root.join("wt/detached/README.md"), "changed\n").unwrap();
    git(
        &root.join("wt/detached"),
        &["commit", "-q", "-am", "only here"],
    );
    let locked = root.join("wt/locked");
    git(&repo, &["worktree", "lock", locked.to_str().unwrap()]);
}

#[test]
fn worktree_verdicts_come_from_git_and_change_nothing() {
    if !have_git() {
        eprintln!("git is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = rilldisk_core::simplify(dir.path().canonicalize().unwrap());
    fixture(&root);

    // Index files before: inspection must not rewrite them.
    let indexes: Vec<_> = walk(&root.join("repo/.git"))
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "index"))
        .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
        .collect();
    assert!(indexes.len() >= 5, "main and worktree indexes exist");

    let state = git::inspect(&root.join("wt/clean")).unwrap();
    assert_eq!(state.branch.as_deref(), Some("feat-clean"));
    assert_eq!(state.changed_files + state.untracked_items, 0);
    assert!(!state.has_remote);
    assert_eq!(
        state.unpushed_commits, 1,
        "no remote: every commit is unpushed"
    );
    assert!(state.head_commit_unix_ms.is_some());
    assert!(!state.locked);

    let dirty = git::inspect(&root.join("wt/dirty")).unwrap();
    assert_eq!(dirty.untracked_items, 1);
    let detached = git::inspect(&root.join("wt/detached")).unwrap();
    assert_eq!(detached.branch, None);
    assert_eq!(detached.commits_only_here, 1);
    assert!(git::inspect(&root.join("wt/locked")).unwrap().locked);
    assert!(git::inspect(&root.join("repo-missing")).is_err());

    // The same through a scan and the reclaim report, judged a month later.
    let snapshot = scan(
        &root,
        ScanOptions::default(),
        Arc::new(ScanControl::default()),
    )
    .unwrap();
    let mut report = find_reclaimable(&snapshot, now_ms());
    report.inspect_git(now_ms() + 30 * DAY, git::DEFAULT_STALE_DAYS);
    let verdict = |name: &str| {
        report
            .candidates
            .iter()
            .find(|c| c.path.ends_with(name))
            .and_then(|c| c.git.as_ref())
            .map(|g| g.verdict)
    };
    assert_eq!(verdict("clean"), Some(Verdict::CleanIdle));
    assert_eq!(verdict("dirty"), Some(Verdict::UncommittedChanges));
    assert_eq!(verdict("detached"), Some(Verdict::CommitsOnlyHere));
    assert_eq!(verdict("locked"), Some(Verdict::Locked));
    let dirty = report
        .candidates
        .iter()
        .find(|c| c.path.ends_with("dirty"))
        .unwrap();
    assert!(dirty.suggested_action.starts_with("Commit or stash"));
    // Judged today, the clean worktree is still active.
    let mut today = find_reclaimable(&snapshot, now_ms());
    today.inspect_git(now_ms(), git::DEFAULT_STALE_DAYS);
    let clean = today
        .candidates
        .iter()
        .find(|c| c.path.ends_with("clean"))
        .unwrap();
    assert_eq!(clean.git.as_ref().unwrap().verdict, Verdict::Active);

    for (path, before) in indexes {
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "{} changed",
            path.display()
        );
    }
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Commit one new file in `dir` and return the commit.
fn commit_file(dir: &Path, name: &str) -> String {
    std::fs::write(dir.join(name), format!("{name}\n")).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-q", "-m", name]);
    git_out(dir, &["rev-parse", "HEAD"])
}

/// Commits whose change already landed elsewhere (cherry-picked or rebased) are
/// not lost work; the next step follows from that and from idle time.
#[test]
fn applied_commits_and_next_steps_come_from_real_repositories() {
    use rilldisk_core::git::{GitReport, NextStep};
    if !have_git() {
        eprintln!("git is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = rilldisk_core::simplify(dir.path().canonicalize().unwrap());
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    commit_file(&repo, "README.md");
    let remote = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", remote.to_str().unwrap()]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["remote", "set-head", "origin", "main"]);
    for (name, extra) in [
        ("picked", vec!["--detach"]),
        ("unique", vec!["--detach"]),
        ("rebased", vec!["-b", "feat-rebased"]),
        ("ahead", vec!["-b", "feat-ahead"]),
        ("lookalike", vec!["--detach"]),
        ("tracks-local", vec!["--track", "-b", "feat-local"]),
    ] {
        let path = root.join("wt").join(name);
        let mut args = vec!["worktree", "add", "-q"];
        args.extend(extra);
        let text = path.to_str().unwrap().to_string();
        args.push(&text);
        if name == "tracks-local" {
            // Its upstream is the local `main`, not a remote branch.
            args.push("main");
        }
        git(&repo, &args);
    }
    let wt = |name: &str| root.join("wt").join(name);
    // Detached commits: one later cherry-picked onto main, one nowhere else.
    let picked = commit_file(&wt("picked"), "picked.txt");
    commit_file(&wt("unique"), "unique.txt");
    // Branch commits: one whose change main got through a cherry-pick (as a
    // rebase-merge does), one that was never pushed anywhere.
    let rebased = commit_file(&wt("rebased"), "rebased.txt");
    commit_file(&wt("ahead"), "ahead.txt");
    // Same file, same words, different spacing: Git's own patch matching ignores
    // whitespace and would call these equal.
    let write_commit = |dir: &Path, text: &str| {
        std::fs::write(dir.join("spaced.txt"), text).unwrap();
        git(dir, &["add", "spaced.txt"]);
        git(dir, &["commit", "-q", "-m", "spaced"]);
    };
    write_commit(&wt("lookalike"), "a b\n");
    let local = commit_file(&wt("tracks-local"), "local.txt");
    // Main moves on first, so a cherry-pick is a new commit with the same patch
    // (on the same parent within the same second it would be the same commit).
    commit_file(&repo, "main.txt");
    git(&repo, &["cherry-pick", &picked]);
    git(&repo, &["cherry-pick", &rebased]);
    write_commit(&repo, "ab\n");
    git(&repo, &["push", "-q", "origin", "main"]);
    // Applied to local main only, after the push: no remote has it.
    git(&repo, &["cherry-pick", &local]);
    let refs_before = git_out(&repo, &["for-each-ref"]);

    let state = |name: &str| git::inspect(&wt(name)).unwrap();
    let picked = state("picked");
    assert_eq!(
        (picked.commits_only_here, picked.commits_only_here_applied),
        (1, 1)
    );
    let unique = state("unique");
    assert_eq!(
        (unique.commits_only_here, unique.commits_only_here_applied),
        (1, 0)
    );
    let rebased = state("rebased");
    assert_eq!((rebased.unpushed_commits, rebased.unpushed_applied), (1, 1));
    let ahead = state("ahead");
    assert_eq!((ahead.unpushed_commits, ahead.unpushed_applied), (1, 0));
    let lookalike = state("lookalike");
    assert_eq!(
        (
            lookalike.commits_only_here,
            lookalike.commits_only_here_applied
        ),
        (1, 0),
        "a whitespace-only match is not the same work"
    );
    let tracks_local = state("tracks-local");
    assert_eq!(tracks_local.upstream.as_deref(), Some("main"));
    assert_eq!(
        (tracks_local.unpushed_commits, tracks_local.unpushed_applied),
        (1, 0),
        "a local upstream is not a remote"
    );

    let later = now_ms() + 30 * DAY;
    let judge = |name: &str, state: git::GitState, now: u64| {
        let report = GitReport::new(Ok(state), &wt(name), None, now, git::DEFAULT_STALE_DAYS);
        (report.verdict, report.next_step)
    };
    assert_eq!(
        judge("picked", picked, later),
        (Verdict::CleanIdle, NextStep::Remove)
    );
    assert_eq!(
        judge("unique", unique.clone(), later),
        (Verdict::CommitsOnlyHere, NextStep::ArchiveThenRemove)
    );
    assert_eq!(
        judge("unique", unique, now_ms()),
        (Verdict::CommitsOnlyHere, NextStep::Keep),
        "worked on today"
    );
    assert_eq!(
        judge("rebased", rebased, later),
        (Verdict::CleanIdle, NextStep::Remove)
    );
    assert_eq!(
        judge("ahead", ahead, later),
        (Verdict::CleanIdle, NextStep::Push)
    );
    // Inspection wrote no refs.
    assert_eq!(git_out(&repo, &["for-each-ref"]), refs_before);
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// Removal asks Git again: a verdict from the scan is never trusted on its own.
/// Nothing is deleted here; the remover only records what reached it.
#[test]
fn cleanup_rechecks_git_and_refuses_stale_reassurance() {
    use rilldisk_core::cleanup::{self, CleanupControl, Guard, Outcome};
    if !have_git() {
        eprintln!("git is not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = rilldisk_core::simplify(dir.path().canonicalize().unwrap());
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    // A remote that has main but not the branch commits below.
    let remote = root.join("remote.git");
    git(&root, &["init", "-q", "--bare", remote.to_str().unwrap()]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let names = [
        "gets-dirty",
        "gets-locked",
        "detached",
        "gets-busy",
        "unpushed",
        "manual",
    ];
    for name in names {
        let path = root.join("wt").join(name);
        let mut args = vec!["worktree", "add", "-q"];
        if name == "detached" {
            args.push("--detach");
        } else {
            args.extend(["-b", name]);
        }
        let text = path.to_str().unwrap().to_string();
        args.push(&text);
        git(&repo, &args);
    }
    let unpushed = root.join("wt/unpushed");
    std::fs::write(unpushed.join("work.md"), "kept in the repository\n").unwrap();
    git(&unpushed, &["add", "work.md"]);
    git(&unpushed, &["commit", "-q", "-m", "not pushed"]);
    // A second repository whose worktree loses its repository after the check.
    let other = root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    std::fs::write(other.join("a.txt"), "a\n").unwrap();
    git(&other, &["add", "a.txt"]);
    git(&other, &["commit", "-q", "-m", "a"]);
    let orphaned = root.join("wt/repo-moved");
    git(
        &other,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "moved",
            orphaned.to_str().unwrap(),
        ],
    );

    let snapshot = scan(
        &root,
        ScanOptions::default(),
        Arc::new(ScanControl::default()),
    )
    .unwrap();
    let mut report = find_reclaimable(&snapshot, now_ms());
    // Threshold 0: every clean worktree counts as idle now and at removal.
    report.inspect_git(now_ms(), 0);
    let busy = report
        .candidates
        .iter()
        .position(|c| c.path.ends_with("gets-busy"))
        .unwrap();
    // This one was judged a month ahead with a 1-day threshold: idle then, but its
    // files are from today, so at removal it shows recent work.
    report.candidates[busy].git = Some(report.candidates[busy].inspect_git(now_ms() + 30 * DAY, 1));
    let manual = report
        .candidates
        .iter()
        .position(|c| c.path.ends_with("manual"))
        .unwrap();
    report.candidates[manual].git = None;
    for c in report
        .candidates
        .iter()
        .filter(|c| c.is_linked_worktree() && c.git.is_some())
    {
        let verdict = c.git.as_ref().unwrap().verdict;
        assert!(
            verdict.no_reported_loss(),
            "{} starts as {verdict:?}",
            c.path.display()
        );
    }

    // What changes between the check and the removal.
    std::fs::write(root.join("wt/gets-dirty/new.txt"), "unsaved\n").unwrap();
    git(
        &repo,
        &[
            "worktree",
            "lock",
            root.join("wt/gets-locked").to_str().unwrap(),
        ],
    );
    let detached = root.join("wt/detached");
    std::fs::write(detached.join("README.md"), "changed\n").unwrap();
    git(&detached, &["commit", "-q", "-am", "only here"]);
    std::fs::rename(&other, root.join("other-moved")).unwrap();

    let ids: Vec<usize> = report
        .candidates
        .iter()
        .filter(|c| c.is_linked_worktree())
        .map(|c| c.entry_id)
        .collect();
    let mut reached = Vec::new();
    let result = cleanup::execute_with(
        &snapshot,
        &report,
        &ids,
        cleanup::CleanupMode::Trash,
        &Guard::built_in(),
        &CleanupControl::default(),
        &mut |path, _| {
            reached.push(path.to_path_buf());
            Ok(())
        },
    );
    let outcome = |name: &str| {
        &result
            .items
            .iter()
            .find(|r| r.item.path.ends_with(name))
            .unwrap_or_else(|| panic!("{name} planned"))
            .outcome
    };
    let refused = |name: &str, expect: &str| match outcome(name) {
        Outcome::Refused { reason } => assert!(reason.contains(expect), "{name}: {reason}"),
        other => panic!("{name} should be refused, got {other:?}"),
    };
    refused("gets-dirty", "untracked item would be lost");
    refused("gets-locked", "Locked");
    refused("detached", "exists nowhere else");
    refused("gets-busy", "now shows recent work");
    refused("repo-moved", "could not check it again");
    // Clean: branch commits stay in the repository even though they were never
    // pushed, so Git reports nothing that removal would lose.
    assert!(matches!(outcome("unpushed"), Outcome::Removed { .. }));
    // Explicitly unchecked: no Git claim is made and the item is not refused for it.
    assert!(matches!(outcome("manual"), Outcome::Removed { .. }));
    assert_eq!(reached.len(), 2, "{reached:?}");
}
