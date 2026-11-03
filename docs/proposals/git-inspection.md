# Proposal: reading Git state of worktree candidates

Status: implemented, November 3, 2026, in the core (`rilldisk_core::git`,
`Candidate::inspect_git`, `ReclaimReport::inspect_git`), the CLI
(`rilldisk reclaim --git [--stale-days N]`) and the desktop (verdict chips, "Select
clean idle worktrees", a warning in the removal dialog, a Settings section).

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
   Unknown is never treated as safe.
5. **Verdicts**, most important first: uncommitted changes; commits only here;
   locked; active (clean, but commits or file changes within the idle threshold,
   14 days by default); clean and idle; unknown. Each verdict comes with a
   suggested command (commit or stash first, create a branch first, unlock first,
   or `git worktree remove`). Verdicts never change a candidate's safety class:
   worktrees stay "Review first".
6. **Where.**
   - **CLI:** only with `rilldisk reclaim --git`. Table and JSON output gain the
     verdict (`candidates[].git`). With `--delete`, worktrees whose verdict is not
     clean (including unknown) are left out, and the CLI says how many.
   - **Desktop:** on by default, with an off switch and the idle threshold in
     Settings. Cards show the verdict, "Select clean idle worktrees" ticks only
     clean and idle ones, and the removal dialog names ticked worktrees that would
     lose work. Scripted captures show fixed preview states and never start Git.
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
file changed.
