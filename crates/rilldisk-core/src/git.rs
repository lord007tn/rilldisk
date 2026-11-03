// SPDX-License-Identifier: AGPL-3.0-only
//! Read-only Git state of worktree candidates. Scoped by
//! docs/proposals/git-inspection.md.
//!
//! Removing a linked worktree folder never deletes commits: they live in the main
//! repository and the branch stays. What is lost is uncommitted work (changed,
//! staged or untracked files) and commits made on a detached HEAD that no branch,
//! tag or remote holds. That is what [`inspect`] measures, using the user's own
//! `git`, and what [`GitReport::new`] turns into a verdict and a suggested command.
//!
//! Every command is read-only: optional locks are off (the index is never
//! refreshed or written), fsmonitor hooks and the untracked cache are disabled,
//! nothing contacts a remote, prompts are off, and each command has a timeout.
use serde::Serialize;
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Longest a single Git command may take before it is stopped.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Default number of days without commits or file changes after which a clean
/// worktree counts as idle.
pub const DEFAULT_STALE_DAYS: u64 = 14;

/// What Git says about one worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GitState {
    /// Checked-out branch; `None` for a detached HEAD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Committer time of HEAD; `None` before the first commit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_commit_unix_ms: Option<u64>,
    /// Tracked files that are staged, modified, deleted, renamed or conflicted.
    pub changed_files: u64,
    /// Untracked files and folders (each untracked folder counts once), not ignored ones.
    pub untracked_items: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Commits on HEAD that no remote-tracking branch holds, as of the last fetch.
    /// With no remote at all, every commit counts.
    pub unpushed_commits: u64,
    pub has_remote: bool,
    /// Detached HEAD only: commits that no branch, tag or remote holds. They become
    /// unreachable once the worktree is removed.
    pub commits_only_here: u64,
    /// `git worktree lock` was used: Git refuses to remove or prune it.
    pub locked: bool,
}

/// Decision for one worktree, most important first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Changed or untracked files would be lost.
    UncommittedChanges,
    /// Commits on a detached HEAD that exist nowhere else.
    CommitsOnlyHere,
    /// Locked with `git worktree lock`.
    Locked,
    /// Clean, but commits or files changed within the idle threshold.
    Active,
    /// No changes Git reports, nothing only here, and no commits or file
    /// changes for at least the threshold.
    CleanIdle,
    /// Git could not answer (not installed, timed out, repository missing…).
    Unknown,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::UncommittedChanges => "Uncommitted changes",
            Verdict::CommitsOnlyHere => "Commits only here",
            Verdict::Locked => "Locked",
            Verdict::Active => "Active",
            Verdict::CleanIdle => "Clean and idle",
            Verdict::Unknown => "Git status unknown",
        }
    }

    /// Git reports nothing that removal would lose. Git does not see ignored
    /// files (`.env`, local settings) and cannot tell whether a program is still
    /// working there, so this is never a guarantee.
    pub fn no_reported_loss(self) -> bool {
        matches!(self, Verdict::CleanIdle | Verdict::Active)
    }
}

/// Git's view of a worktree candidate: the state, the verdict and what to do.
#[derive(Debug, Clone, Serialize)]
pub struct GitReport {
    pub verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<GitState>,
    /// Why the state is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Latest of the HEAD commit and the newest file change, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_work_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_days: Option<u64>,
    pub stale_days: u64,
    /// One sentence for people.
    pub summary: String,
    /// The command or step to take next.
    pub suggested_action: String,
}

impl GitReport {
    /// Judge `state` (or the error that replaced it). `newest_file_ms` is the
    /// newest modification time the scan saw in the folder.
    pub fn new(
        result: Result<GitState, String>,
        path: &Path,
        newest_file_ms: Option<u64>,
        now_unix_ms: u64,
        stale_days: u64,
    ) -> Self {
        let quoted = crate::reclaim::quoted(path);
        let here = quoted.as_deref().unwrap_or("<this folder>");
        let state = match result {
            Ok(state) => state,
            Err(error) => {
                return GitReport {
                    verdict: Verdict::Unknown,
                    state: None,
                    summary: format!("Git could not check this worktree: {error}"),
                    error: Some(error),
                    last_work_unix_ms: newest_file_ms,
                    idle_days: newest_file_ms.map(|m| days(now_unix_ms, m)),
                    stale_days,
                    suggested_action: format!(
                        "Run git status in {here} and check for unsaved work before removing it"
                    ),
                };
            }
        };
        let last_work = state.head_commit_unix_ms.max(newest_file_ms);
        let idle = last_work.map(|m| days(now_unix_ms, m));
        let dirty = state.changed_files + state.untracked_items;
        let only_here = state.branch.is_none() && state.commits_only_here > 0;
        let verdict = if dirty > 0 {
            Verdict::UncommittedChanges
        } else if only_here {
            Verdict::CommitsOnlyHere
        } else if state.locked {
            Verdict::Locked
        } else if idle.is_some_and(|d| d >= stale_days) {
            Verdict::CleanIdle
        } else if idle.is_some() {
            Verdict::Active
        } else {
            Verdict::Unknown
        };
        let mut parts = Vec::new();
        match verdict {
            Verdict::UncommittedChanges => {
                let mut what = Vec::new();
                if state.changed_files > 0 {
                    what.push(plural(state.changed_files, "changed file"));
                }
                if state.untracked_items > 0 {
                    what.push(plural(state.untracked_items, "untracked item"));
                }
                parts.push(format!("{} would be lost", what.join(" and ")));
            }
            Verdict::CommitsOnlyHere => parts.push(format!(
                "{} on a detached HEAD {} nowhere else",
                plural(state.commits_only_here, "commit"),
                if state.commits_only_here == 1 {
                    "exists"
                } else {
                    "exist"
                }
            )),
            Verdict::Locked => parts.push("Locked with git worktree lock".into()),
            Verdict::Active | Verdict::CleanIdle => {
                parts.push("No changes Git reports (ignored files are not checked)".into())
            }
            Verdict::Unknown => parts.push("Clean, but its age is unknown".into()),
        }
        if let Some(days) = idle {
            parts.push(match days {
                0 => "last work today".to_string(),
                1 => "last work yesterday".to_string(),
                d => format!("last work {d} days ago"),
            });
        }
        match (&state.branch, state.unpushed_commits, state.has_remote) {
            (Some(branch), 0, true) => parts.push(format!("branch {branch} is pushed")),
            (Some(branch), n, true) => parts.push(format!(
                "branch {branch} has {} not on any remote (kept in the repository)",
                plural(n, "commit")
            )),
            (Some(branch), _, false) => parts.push(format!(
                "branch {branch} (no remote; kept in the repository)"
            )),
            (None, ..) => parts.push("detached HEAD".into()),
        }
        let suggested_action = match verdict {
            Verdict::UncommittedChanges => format!(
                "Commit or stash the changes in {here} (git -C {here} status), then git worktree remove {here}"
            ),
            Verdict::CommitsOnlyHere => format!(
                "Keep the commits on a branch first: git -C {here} switch -c <name>, then git worktree remove {here}"
            ),
            Verdict::Locked => format!(
                "Check why it is locked (git worktree list -v); if it is no longer needed: git worktree unlock {here}, then git worktree remove {here}"
            ),
            Verdict::Active | Verdict::CleanIdle | Verdict::Unknown => {
                format!("git worktree remove {here}")
            }
        };
        let mut summary = parts.join("; ");
        summary.push('.');
        GitReport {
            verdict,
            state: Some(state),
            error: None,
            last_work_unix_ms: last_work,
            idle_days: idle,
            stale_days,
            summary,
            suggested_action,
        }
    }
}

/// What the user was shown about a worktree when they chose to remove it.
/// Cleanup asks Git again right before removal ([`recheck`]); this is the
/// baseline the fresh answer is compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GitExpectation {
    /// The verdict shown; `Unknown` when Git had not answered yet.
    pub verdict: Verdict,
    pub stale_days: u64,
    /// Newest file change the scan saw, kept so activity is judged the same way.
    #[serde(skip)]
    pub newest_file_ms: Option<u64>,
}

impl From<&GitReport> for GitExpectation {
    fn from(report: &GitReport) -> Self {
        GitExpectation {
            verdict: report.verdict,
            stale_days: report.stale_days,
            newest_file_ms: None,
        }
    }
}

/// Ask Git about `path` again and return why it must not be removed, if so.
/// Nothing may be lost per the fresh answer, Git must answer at all, and a
/// worktree chosen as idle must still be idle. A clean state is not proof that
/// no program is working there; this only rules out what Git can see.
pub fn recheck(path: &Path, expected: &GitExpectation, now_unix_ms: u64) -> Option<String> {
    let fresh = GitReport::new(
        inspect(path),
        path,
        expected.newest_file_ms,
        now_unix_ms,
        expected.stale_days,
    );
    if fresh.verdict == Verdict::Unknown {
        return Some(format!(
            "Git could not check it again right before removal ({}), so it was kept",
            fresh.error.as_deref().unwrap_or("no answer")
        ));
    }
    if !fresh.verdict.no_reported_loss() {
        return Some(format!("Git now reports: {}", fresh.summary));
    }
    if expected.verdict == Verdict::CleanIdle && fresh.verdict == Verdict::Active {
        return Some(format!(
            "It was idle when checked, but Git now shows recent work ({})",
            fresh.summary
        ));
    }
    None
}

fn days(now_ms: u64, then_ms: u64) -> u64 {
    now_ms.saturating_sub(then_ms) / 86_400_000
}

fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Ask Git about the worktree at `path`. Errors are short sentences.
pub fn inspect(path: &Path) -> Result<GitState, String> {
    let status = run(
        path,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=normal",
        ],
    )?;
    let mut state = parse_status(&status);
    state.head_commit_unix_ms = run(path, &["log", "-1", "--format=%ct", "HEAD"])
        .ok()
        .and_then(|out| out.trim().parse::<u64>().ok())
        .map(|seconds| seconds * 1000);
    if state.head_commit_unix_ms.is_some() {
        state.has_remote = !run(path, &["remote"])?.trim().is_empty();
        state.unpushed_commits = count(&run(
            path,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
        )?);
        if state.branch.is_none() {
            state.commits_only_here = count(&run(
                path,
                &[
                    "rev-list",
                    "--count",
                    "HEAD",
                    "--not",
                    "--branches",
                    "--tags",
                    "--remotes",
                ],
            )?);
        }
    }
    let git_dir = run(path, &["rev-parse", "--absolute-git-dir"])?;
    state.locked = Path::new(git_dir.trim()).join("locked").exists();
    Ok(state)
}

fn count(text: &str) -> u64 {
    text.trim().parse().unwrap_or(0)
}

/// `git status --porcelain=v2 --branch -z` output, without the timestamps.
pub fn parse_status(output: &str) -> GitState {
    let mut state = GitState::default();
    let mut fields = output.split('\0');
    while let Some(field) = fields.next() {
        if let Some(header) = field.strip_prefix("# ") {
            if let Some(head) = header.strip_prefix("branch.head ") {
                state.branch = (head != "(detached)").then(|| head.to_string());
            } else if let Some(upstream) = header.strip_prefix("branch.upstream ") {
                state.upstream = Some(upstream.to_string());
            }
            continue;
        }
        match field.as_bytes().first() {
            Some(b'1') | Some(b'u') => state.changed_files += 1,
            Some(b'2') => {
                state.changed_files += 1;
                // A rename carries its original path in the next field.
                fields.next();
            }
            Some(b'?') => state.untracked_items += 1,
            _ => {}
        }
    }
    state
}

/// Run one read-only Git command in `dir` and return its standard output.
fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    // Git does not understand `\\?\` verbatim paths.
    let dir = crate::simplify(dir.to_path_buf());
    let mut command = Command::new("git");
    command
        .arg("--no-optional-locks")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .arg("-C")
        .arg(&dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        // Inspect the folder itself, whatever the parent process points Git at.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window flashes up from the desktop app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "Git is not installed or not on PATH".to_string(),
        _ => format!("could not start Git ({error})"),
    })?;
    // Read both pipes on threads so a chatty command cannot block on a full pipe.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > COMMAND_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git {} took longer than {} seconds",
                    args[0],
                    COMMAND_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Err(format!("could not wait for Git ({error})")),
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        let first = err
            .lines()
            .map(|line| line.trim_start_matches("fatal: ").trim())
            .find(|line| !line.is_empty())
            .unwrap_or("Git reported an error");
        Err(first.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400_000;

    #[test]
    fn porcelain_counts_changes_untracked_and_renames() {
        let output = "# branch.oid abc\0# branch.head feat/x\0# branch.upstream origin/feat/x\0# branch.ab +2 -0\0\
1 .M N... 100644 100644 100644 a b src/lib.rs\0\
2 R. N... 100644 100644 100644 a b R100 new.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 a b c conflict.rs\0\
? notes.txt\0? scratch/\0";
        let state = parse_status(output);
        assert_eq!(state.branch.as_deref(), Some("feat/x"));
        assert_eq!(state.upstream.as_deref(), Some("origin/feat/x"));
        assert_eq!(state.changed_files, 3);
        assert_eq!(state.untracked_items, 2);
        let detached = parse_status("# branch.oid abc\0# branch.head (detached)\0");
        assert_eq!(detached.branch, None);
        assert_eq!(detached.changed_files + detached.untracked_items, 0);
    }

    fn clean(branch: Option<&str>) -> GitState {
        GitState {
            branch: branch.map(str::to_string),
            head_commit_unix_ms: Some(0),
            has_remote: true,
            ..GitState::default()
        }
    }

    #[test]
    fn verdicts_put_unsaved_work_first() {
        let path = Path::new("/w/wt");
        let now = 40 * DAY;
        let judge = |state: GitState| GitReport::new(Ok(state), path, None, now, 14).verdict;
        assert_eq!(judge(clean(Some("main"))), Verdict::CleanIdle);
        let mut dirty = clean(Some("main"));
        dirty.untracked_items = 1;
        dirty.locked = true;
        assert_eq!(judge(dirty), Verdict::UncommittedChanges);
        let mut detached = clean(None);
        detached.commits_only_here = 2;
        assert_eq!(judge(detached.clone()), Verdict::CommitsOnlyHere);
        // On a branch the same commits stay in the repository.
        detached.branch = Some("feat".into());
        assert_eq!(judge(detached), Verdict::CleanIdle);
        let mut locked = clean(Some("main"));
        locked.locked = true;
        assert_eq!(judge(locked), Verdict::Locked);
        let mut recent = clean(Some("main"));
        recent.head_commit_unix_ms = Some(now - 3 * DAY);
        assert_eq!(judge(recent), Verdict::Active);
    }

    #[test]
    fn recent_file_changes_count_as_work() {
        let state = clean(Some("main"));
        let report = GitReport::new(Ok(state), Path::new("/w/wt"), Some(39 * DAY), 40 * DAY, 14);
        assert_eq!(report.verdict, Verdict::Active);
        assert_eq!(report.idle_days, Some(1));
        assert!(
            report.summary.contains("last work yesterday"),
            "{}",
            report.summary
        );
    }

    #[test]
    fn reports_explain_and_suggest() {
        let mut state = clean(Some("feat/x"));
        state.unpushed_commits = 3;
        state.changed_files = 1;
        let report = GitReport::new(Ok(state), Path::new("/w/wt"), None, 40 * DAY, 14);
        assert!(
            report.summary.starts_with("1 changed file would be lost"),
            "{}",
            report.summary
        );
        assert!(report.summary.contains("3 commits not on any remote"));
        assert!(report.suggested_action.starts_with("Commit or stash"));
        let unknown = GitReport::new(
            Err("Git is not installed".into()),
            Path::new("/w"),
            None,
            0,
            14,
        );
        assert_eq!(unknown.verdict, Verdict::Unknown);
        assert!(!unknown.verdict.no_reported_loss());
    }
}
