// SPDX-License-Identifier: AGPL-3.0-only
//! Read-only Git state of worktree candidates. Scoped by
//! docs/proposals/git-inspection.md.
//!
//! Removing a linked worktree folder never deletes commits: they live in the main
//! repository and the branch stays. What is lost is uncommitted work (changed,
//! staged or untracked files) and commits made on a detached HEAD that no branch,
//! tag or remote holds. That is what [`inspect`] measures, using the user's own
//! `git`, and what [`GitReport::new`] turns into a verdict, a [`NextStep`] and a
//! suggested command.
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
    /// Of `commits_only_here`, commits whose change another branch already has as
    /// the same patch (after a cherry-pick or rebase). Their content is not lost.
    pub commits_only_here_applied: u64,
    /// Of `unpushed_commits`, commits whose change a remote branch already has as
    /// the same patch, so there is nothing left to push for them.
    pub unpushed_applied: u64,
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

/// What to do with a worktree, from Git's view alone. Git cannot see whether a
/// program, an assistant thread or a terminal still uses the folder, so even
/// [`NextStep::Remove`] asks a person to check that first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NextStep {
    /// Locked, worked on within the idle threshold, or Git could not answer.
    Keep,
    /// Idle and clean, but its branch has commits no remote has (as the same
    /// change). The branch stays in the repository either way; pushing keeps a
    /// copy off this machine.
    Push,
    /// Idle, with work that exists only here: changed or untracked files, or
    /// commits on a detached HEAD. Save it (patch, branch or bundle) first.
    ArchiveThenRemove,
    /// Idle, and Git reports nothing that removal would lose.
    Remove,
}

impl NextStep {
    pub fn label(self) -> &'static str {
        match self {
            NextStep::Keep => "Keep",
            NextStep::Push => "Push, then remove",
            NextStep::ArchiveThenRemove => "Archive, then remove",
            NextStep::Remove => "Remove",
        }
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
    /// Keep, push, archive or remove, judged from Git state and idle time.
    pub next_step: NextStep,
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
                    next_step: NextStep::Keep,
                    suggested_action: format!(
                        "Run git status in {here} and check for unsaved work before removing it"
                    ),
                };
            }
        };
        let last_work = state.head_commit_unix_ms.max(newest_file_ms);
        let idle = last_work.map(|m| days(now_unix_ms, m));
        let dirty = state.changed_files + state.untracked_items;
        // Detached commits whose change no other branch has as the same patch.
        let lost_commits = state
            .commits_only_here
            .saturating_sub(state.commits_only_here_applied);
        let only_here = state.branch.is_none() && lost_commits > 0;
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
            Verdict::CommitsOnlyHere => {
                let mut text = format!(
                    "{} on a detached HEAD {} nowhere else",
                    plural(lost_commits, "commit"),
                    if lost_commits == 1 { "exists" } else { "exist" }
                );
                if state.commits_only_here_applied > 0 {
                    text.push_str(&format!(
                        " ({} more already applied on another branch)",
                        state.commits_only_here_applied
                    ));
                }
                parts.push(text);
            }
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
        let to_push = state
            .unpushed_commits
            .saturating_sub(state.unpushed_applied);
        match (&state.branch, state.unpushed_commits, state.has_remote) {
            (Some(branch), 0, true) => parts.push(format!("branch {branch} is pushed")),
            (Some(branch), n, true) if to_push == 0 => parts.push(format!(
                "branch {branch} has {} not on any remote, but a remote branch already has the same changes",
                plural(n, "commit")
            )),
            (Some(branch), _, true) => {
                let mut text = format!(
                    "branch {branch} has {} not on any remote (kept in the repository)",
                    plural(to_push, "commit")
                );
                if state.unpushed_applied > 0 {
                    text.push_str(&format!(
                        "; {} more already applied on a remote branch",
                        state.unpushed_applied
                    ));
                }
                parts.push(text);
            }
            (Some(branch), _, false) => parts.push(format!(
                "branch {branch} (no remote; kept in the repository)"
            )),
            (None, ..) if state.commits_only_here > 0 && lost_commits == 0 => parts.push(format!(
                "detached HEAD; its {} only here {} already applied on another branch",
                plural(state.commits_only_here, "commit"),
                if state.commits_only_here == 1 {
                    "is"
                } else {
                    "are"
                }
            )),
            (None, ..) => parts.push("detached HEAD".into()),
        }
        let idle_enough = idle.is_some_and(|d| d >= stale_days);
        let next_step = match verdict {
            _ if state.locked => NextStep::Keep,
            Verdict::Unknown | Verdict::Locked | Verdict::Active => NextStep::Keep,
            Verdict::UncommittedChanges | Verdict::CommitsOnlyHere if idle_enough => {
                NextStep::ArchiveThenRemove
            }
            Verdict::UncommittedChanges | Verdict::CommitsOnlyHere => NextStep::Keep,
            Verdict::CleanIdle if state.branch.is_some() && state.has_remote && to_push > 0 => {
                NextStep::Push
            }
            Verdict::CleanIdle => NextStep::Remove,
        };
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
            Verdict::CleanIdle if next_step == NextStep::Push => match &state.branch {
                Some(branch) => format!(
                    "Push the branch first (git -C {here} push -u <remote> {branch}), then git worktree remove {here}"
                ),
                None => format!("git worktree remove {here}"),
            },
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
            next_step,
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
            if state.commits_only_here > 0 {
                let bases = equivalence_bases(path, None, true);
                state.commits_only_here_applied =
                    applied_elsewhere(path, &["--branches", "--tags", "--remotes"], &bases);
            }
        } else if state.has_remote && state.unpushed_commits > 0 {
            let bases = equivalence_bases(path, state.upstream.as_deref(), false);
            state.unpushed_applied = applied_elsewhere(path, &["--remotes"], &bases);
        }
    }
    let git_dir = run(path, &["rev-parse", "--absolute-git-dir"])?;
    state.locked = Path::new(git_dir.trim()).join("locked").exists();
    Ok(state)
}

fn count(text: &str) -> u64 {
    text.trim().parse().unwrap_or(0)
}

/// Most commits whose patches are compared; beyond this nothing counts as applied.
const MAX_COMPARED_COMMITS: usize = 500;

/// Refs to compare HEAD's commits with: the upstream, each remote's default
/// branch, and with `local` the usual local default branches. At most four.
/// Without `local` only remote-tracking refs count: a branch may track a local
/// branch, and that one says nothing about what a remote holds.
fn equivalence_bases(path: &Path, upstream: Option<&str>, local: bool) -> Vec<String> {
    let mut bases: Vec<String> = upstream
        .and_then(|name| {
            let full = run(path, &["rev-parse", "--symbolic-full-name", name]).ok()?;
            let full = full.trim().to_string();
            (local || full.starts_with("refs/remotes/")).then_some(full)
        })
        .into_iter()
        .collect();
    if let Ok(refs) = run(
        path,
        &["for-each-ref", "--format=%(refname)", "refs/remotes/"],
    ) {
        bases.extend(
            refs.lines()
                .filter(|r| r.ends_with("/HEAD"))
                .map(str::to_string),
        );
    }
    if local {
        for name in ["refs/heads/main", "refs/heads/master"] {
            if run(path, &["rev-parse", "--verify", "-q", name]).is_ok() {
                bases.push(name.to_string());
            }
        }
    }
    bases.dedup();
    bases.truncate(4);
    bases
}

/// How many of HEAD's commits outside `exclude` (`--remotes` and so on) have the
/// same patch on one of `bases`, byte for byte. Git's own matching
/// (`--cherry-mark`) ignores whitespace, so `"a b"` and `"ab"` would match; it
/// only proposes pairs, and [`verbatim_patch_ids`] confirms them. Merge commits,
/// empty commits and any Git failure never count as applied: an error must not
/// make work look safe to lose.
fn applied_elsewhere(path: &Path, exclude: &[&str], bases: &[String]) -> u64 {
    if bases.is_empty() {
        return 0;
    }
    let mut args = vec!["rev-list", "--no-merges", "HEAD", "--not"];
    args.extend_from_slice(exclude);
    let Ok(candidates) = run(path, &args) else {
        return 0;
    };
    let mut pending: std::collections::HashSet<&str> = candidates.lines().collect();
    if pending.is_empty() || pending.len() > MAX_COMPARED_COMMITS {
        return 0;
    }
    let total = pending.len();
    for base in bases {
        let range = format!("{base}...HEAD");
        // `=` marks commits on either side whose patch Git considers equivalent
        // to one on the other side (whitespace ignored).
        let Ok(marked) = run(path, &["rev-list", "--cherry-mark", "--no-merges", &range]) else {
            continue;
        };
        let equivalent: Vec<&str> = marked.lines().filter_map(|l| l.strip_prefix('=')).collect();
        if !equivalent.iter().any(|c| pending.contains(c)) {
            continue;
        }
        if equivalent.len() > 2 * MAX_COMPARED_COMMITS {
            continue;
        }
        let Some(ids) = verbatim_patch_ids(path, &equivalent) else {
            continue;
        };
        // Patch IDs of the commits that stay after removal (not pending).
        let elsewhere: std::collections::HashSet<&str> = ids
            .iter()
            .filter(|(commit, _)| !pending.contains(commit.as_str()))
            .map(|(_, id)| id.as_str())
            .collect();
        pending.retain(|commit| {
            ids.get(*commit)
                .is_none_or(|id| !elsewhere.contains(id.as_str()))
        });
        if pending.is_empty() {
            break;
        }
    }
    (total - pending.len()) as u64
}

/// Whitespace-preserving patch ID of each commit (`git patch-id --verbatim`,
/// Git 2.39 or later), keyed by commit. Binary changes are compared in full.
/// Commits without a patch (empty) are absent. `None` when Git cannot answer
/// or a patch contains a NUL byte.
fn verbatim_patch_ids(
    path: &Path,
    commits: &[&str],
) -> Option<std::collections::HashMap<String, String>> {
    let mut args = vec![
        "log",
        "--no-walk=unsorted",
        "-p",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--binary",
        "--full-index",
        "--format=commit %H",
    ];
    args.extend_from_slice(commits);
    // Raw bytes: decoding a patch that is not UTF-8 could make two differ-
    // ent patches look the same.
    let patches = run_bytes(path, &args, None).ok()?;
    // Patch IDs hash each line only up to its first NUL byte, so a patch that
    // contains one (a file diffed as text) cannot be confirmed byte for byte.
    if patches.contains(&0) {
        return None;
    }
    let ids = run_bytes(path, &["patch-id", "--verbatim"], Some(patches)).ok()?;
    let ids = String::from_utf8_lossy(&ids);
    Some(
        ids.lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(id, commit)| (commit.to_string(), id.to_string()))
            .collect(),
    )
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
    // Lossy: a file name that is not UTF-8 must not empty the whole output.
    run_bytes(dir, args, None).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// [`run`] returning raw bytes, with optional `input` on standard input.
fn run_bytes(dir: &Path, args: &[&str], input: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
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
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
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
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // A thread, so Git can fill its output pipe while it still reads input.
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(&input);
        });
    }
    // Read both pipes on threads so a chatty command cannot block on a full pipe.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
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
    fn next_steps_follow_state_and_idle_time() {
        let path = Path::new("/w/wt");
        let now = 40 * DAY;
        let step = |state: GitState, newest: u64| {
            GitReport::new(Ok(state), path, Some(newest), now, 14).next_step
        };
        let old = now - 30 * DAY;
        let recent = now - DAY;
        assert_eq!(step(clean(Some("main")), old), NextStep::Remove);
        assert_eq!(step(clean(Some("main")), recent), NextStep::Keep);
        let mut dirty = clean(Some("main"));
        dirty.changed_files = 2;
        assert_eq!(step(dirty.clone(), old), NextStep::ArchiveThenRemove);
        assert_eq!(step(dirty.clone(), recent), NextStep::Keep, "in use");
        dirty.locked = true;
        assert_eq!(step(dirty, old), NextStep::Keep, "locked wins");
        let mut ahead = clean(Some("feat"));
        ahead.unpushed_commits = 3;
        assert_eq!(step(ahead.clone(), old), NextStep::Push);
        ahead.unpushed_applied = 3;
        let report = GitReport::new(Ok(ahead), path, Some(old), now, 14);
        assert_eq!(report.next_step, NextStep::Remove);
        assert!(
            report.summary.contains("already has the same changes"),
            "{}",
            report.summary
        );
        let mut local = clean(Some("feat"));
        local.has_remote = false;
        local.unpushed_commits = 3;
        assert_eq!(step(local, old), NextStep::Remove, "nowhere to push");
    }

    #[test]
    fn detached_commits_already_applied_elsewhere_are_not_lost() {
        let path = Path::new("/w/wt");
        let now = 40 * DAY;
        let mut detached = clean(None);
        detached.commits_only_here = 2;
        detached.commits_only_here_applied = 1;
        let report = GitReport::new(Ok(detached.clone()), path, None, now, 14);
        assert_eq!(report.verdict, Verdict::CommitsOnlyHere);
        assert!(
            report
                .summary
                .starts_with("1 commit on a detached HEAD exists nowhere else (1 more"),
            "{}",
            report.summary
        );
        assert_eq!(report.next_step, NextStep::ArchiveThenRemove);
        detached.commits_only_here_applied = 2;
        let report = GitReport::new(Ok(detached), path, None, now, 14);
        assert_eq!(report.verdict, Verdict::CleanIdle);
        assert_eq!(report.next_step, NextStep::Remove);
        assert!(
            report.summary.contains("already applied on another branch"),
            "{}",
            report.summary
        );
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
