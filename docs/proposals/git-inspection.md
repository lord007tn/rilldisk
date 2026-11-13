# Proposal: reading Git state of worktree candidates

Status: implemented, November 3, 2026, in the core (`rilldisk_core::git`,
`Candidate::inspect_git`, `ReclaimReport::inspect_git`), the CLI
(`rilldisk reclaim --git [--stale-days N]`) and the desktop (verdict chips, "Select
clean idle worktrees", a warning in the removal dialog, a Settings section).
Amended November 13, 2026: commits already applied elsewhere, and a next step
([cleanup-lessons.md](../cleanup-lessons.md)).

AGENTS.md asks for a scoped proposal before RillDisk starts another program or
reads more than metadata. This document is that scope. Anything not described here
is out of scope.

## Problem

Linked Git worktrees, many of them left behind by AI coding agents, are among the
largest cleanup candidates. Until now RillDisk could only say "Review first":
from metadata alone, a worktree holding a day of uncommitted work looks the same as
one whose branch was merged a month ago. People had to open each one in a
terminal to decide.

## What is at stake

Removing a linked worktree folder never deletes commits: they are stored in the
main repository and the branch stays. What is lost is:

1. **Uncommitted work**: staged, modified, deleted or untracked (not ignored) files.
2. **Commits on a detached HEAD** that no branch, tag or remote holds. Once the
   worktree is gone, nothing references them.

A lock (`git worktree lock`) also matters: someone said "keep this".

## Scope

1. **Which folders.** Only candidates RillDisk already lists as a linked Git
   worktree (a folder whose `.git` is a file). Orphaned agent worktrees have no
   `.git` and are not inspected. Ordinary repositories are never inspected.
2. **What runs.** The user's own `git`, found on `PATH`, never a shell. Per
   worktree, at most these commands, each run with
   `git --no-optional-locks -c core.fsmonitor=false -c core.untrackedCache=false -C <worktree>`:
   - `status --porcelain=v2 --branch -z --untracked-files=normal`
   - `log -1 --format=%ct HEAD`
   - `remote`
   - `rev-list --count HEAD --not --remotes`
   - `rev-list --count HEAD --not --branches --tags --remotes` (detached HEAD only)
   - when there are commits only here (detached) or unpushed branch commits with a
     remote: `for-each-ref --format=%(refname) refs/remotes/`,
     `rev-parse --verify -q refs/heads/main` (and `master`, detached only),
     `rev-list --no-merges HEAD --not <the refs above>` and, for at most four
     bases (the upstream, each `refs/remotes/<remote>/HEAD`, local `main` or
     `master`), `rev-list --cherry-pick --right-only --no-merges <base>...HEAD`.
     Patch IDs are computed by Git in memory; nothing is written.
   - `rev-parse --absolute-git-dir`, then a metadata check for `<git-dir>/locked`
3. **Read-only.** `--no-optional-locks` and `GIT_OPTIONAL_LOCKS=0` stop `status`
   from refreshing or writing the index. fsmonitor hooks and the untracked cache
   are turned off. Nothing is fetched, so remote-tracking branches are as of the
   user's last fetch, and the text says "not on any remote", never "unpushed to
   GitHub". `GIT_TERMINAL_PROMPT=0`, standard input closed, `GIT_DIR`,
   `GIT_WORK_TREE` and `GIT_INDEX_FILE` removed, `LC_ALL=C`. No console window on
   Windows. The integration test checks that every index file is byte-for-byte
   unchanged afterwards.
4. **Limits.** Each command stops after 15 seconds. The desktop runs at most four
   worktrees at once, in the background after a scan; the CLI also uses four.
   Missing Git, a timeout or an error gives the verdict "unknown" with the reason.
   A check that produces no report at all (a failed worker) is also "unknown".
   Unknown is never treated as safe.
5. **Verdicts**, most important first: uncommitted changes; commits only here;
   locked; active (clean, but commits or file changes within the idle threshold,
   14 days by default); clean and idle; unknown. Each verdict comes with a
   suggested command (commit or stash first, create a branch first, unlock first,
   or `git worktree remove`). A commit that no ref holds but whose patch another
   branch already has (after a cherry-pick or rebase) is counted as applied
   (`commits_only_here_applied`, `unpushed_applied`) and is not lost work; merge
   commits and any Git error never count as applied, and at most 500 commits are
   compared. Each report also has a `next_step`: keep (locked, worked on within
   the idle threshold, or unknown), archive then remove (idle with unsaved work or
   detached-only commits), push (idle and clean, branch has commits no remote has
   as the same change), or remove. Git cannot see programs or assistant threads
   still using a folder, so the CLI says so next to every step but keep.
   Verdicts never change a candidate's safety class:
   worktrees stay "Review first". The wording says what Git reports, never that a
   worktree is safe: `git status` does not list ignored files (`.env`, local
   settings), and nothing here detects a program still working in the folder.
7. **At removal.** A worktree that carries a Git verdict into cleanup (the desktop
   with checks on, including worktrees whose check had not finished; the CLI with
   `--git`) is asked about again with the same commands right before it is moved.
   It is refused unless Git answers and reports nothing that would be lost, and a
   worktree picked as clean and idle is refused if it shows activity since
   (`git::recheck`, called from `cleanup::execute_with`). Without a Git verdict a
   worktree takes the explicit Review-first path, labelled as not checked.
6. **Where.**
   - **CLI:** only with `rilldisk reclaim --git`. Table and JSON output gain the
     verdict (`candidates[].git`). With `--delete`, worktrees whose verdict is not
     clean (including unknown) are left out, and the CLI says how many.
   - **Desktop:** on by default, with an off switch and the idle threshold in
     Settings. Cards show the verdict, "Select clean idle worktrees" ticks only
     clean and idle ones, and the removal dialog names ticked worktrees Git
     reports changes for, ones not checked yet, and the ignored-file and
     running-agent caveat. Scripted captures run Git on the synthetic fixture's
     own repositories.
   - **MCP:** unchanged. The server promises no program execution, so it does not
     run Git; `find_reclaimable` output keeps its shape.

## Out of scope

- Any Git command that writes: `worktree remove`, `prune`, `stash`, `commit`,
  `fetch`. RillDisk shows these as suggestions to copy, never runs them.
- Reading file contents, diffs or commit messages.
- Inspecting ordinary clones or submodules.

## Tests

`git::tests` covers porcelain parsing (renames carry two paths) and the verdict
order. `tests/git.rs` builds real repositories with clean, dirty, detached and
locked worktrees, checks every verdict through a scan, and checks that no index
file changed. It also changes worktrees after they were judged (an untracked file,
a lock, a detached-only commit, new activity, a moved main repository) and checks
that removal refuses each one, while a clean worktree with unpushed branch commits
and an explicitly unchecked one still reach the remover (nothing is deleted: the
test records instead). A third test pushes to a bare remote, cherry-picks a
detached commit and a branch commit onto `main`, and checks that both count as
applied, that a commit found nowhere else does not, the next step of each, and
that no ref was written. `reclaim::tests` and the CLI tests check that a missing
report becomes unknown and keeps a worktree out of `--git --delete`.
