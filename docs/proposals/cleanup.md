# Proposal: removing selected reclaim candidates

Status: implemented, November 2, 2026, in `rilldisk-core` (`cleanup` module), the
CLI (`rilldisk reclaim --delete`) and the desktop (Clean up view). Amended
November 3, 2026 (0.1.3): folder identity check, and a live Git check for
worktrees that carried a Git verdict ([git-inspection.md](git-inspection.md)).

AGENTS.md requires a scoped proposal before RillDisk deletes or moves files.
This document is that scope. The project owner asked for the ability to "select
all and issue delete for the recommended stuff". Anything not described here
stays out of scope: RillDisk still never deletes on its own, never opens file
contents itself, sends nothing anywhere, never elevates, and the MCP server stays
read-only. The only program cleanup starts is the user's `git`, read-only, for
worktrees that were checked with Git (below).

## Scope

1. **What can be removed.** Only reclaim candidates: folders that
   `find_reclaimable` reported for the snapshot on screen. A request for any
   other entry, an entry ID from another scan, or a candidate whose path no
   longer matches the snapshot is refused before anything is touched.
2. **Who decides.** A person, every time. The desktop shows a confirmation
   dialog listing every path and the total size, with Cancel as the default
   button. The CLI lists everything and asks on a terminal; without a terminal
   it refuses unless `--yes` is given. Nothing runs on a schedule, from MCP or
   from an AI assistant.
3. **Where it goes.** By default to the Windows Recycle Bin, the macOS Trash or
   the freedesktop.org Trash (Linux), so every removal can be restored.
   Permanent deletion is an explicit opt-in: a menu item or Shift-click plus an
   "I understand" tick in the desktop; `--permanent` plus typing `delete` (or
   `--yes`) in the CLI.
4. **"Select all safe".** Ticks the outermost Regenerable and Cache candidates
   that pass the static checks below. Review-first candidates (Git worktrees,
   orphaned agent worktrees, model downloads, whole `~/.cache` folders, Trash
   folders) are never ticked for you; each one must be ticked individually, and
   the confirmation dialog warns when any are included. In the CLI they are
   only included when named with `--safety review`.

## Safety rules

All rules live in `crates/rilldisk-core/src/cleanup.rs` and are shared by the CLI
and the desktop. Planning applies the static rules; execution re-runs every rule
against the live filesystem immediately before each item, one item at a time.

| Rule | Static (plan) | Live (just before removal) |
| --- | --- | --- |
| Is a candidate of this report, and its path matches the snapshot | ✓ | |
| Is not the scanned folder itself | ✓ | |
| Is not a Recycle Bin / Trash folder (empty those from the file manager) | ✓ | |
| Was a directory in the scan | ✓ | |
| Is not a volume root, drive root or standard mount location | ✓ | ✓ |
| Is not, and does not contain, the home folder, the system temp folder, `C:\Users`, `/home`, `/Users`, `/var`, `/tmp`, `/opt` and similar | ✓ | ✓ |
| Is not, does not contain and is not inside a system folder: `%SystemRoot%`, Program Files, ProgramData, `/usr`, `/bin`, `/etc`, `/System`, `/Library`, `/Applications`, ... | ✓ | ✓ |
| Does not contain the running RillDisk executable | ✓ | ✓ |
| Still exists | | ✓ |
| Is not a symlink, junction or other reparse point (`symlink_metadata`, never followed) | | ✓ |
| Is still a directory | | ✓ |
| Resolves to the same canonical path (no link or junction was swapped in above it) | | ✓ |
| Is not a mount point of another filesystem (Unix device check) | | ✓ |
| Is the folder that was found: same volume and file ID (Windows) or device and inode (Unix), recorded when candidates were found | | ✓ |
| Git-checked linked worktree: Git, asked again now, reports no changed or untracked files, no lock, no commits only on a detached HEAD, and no activity since it was judged idle; Git must answer | | ✓ |

A selected candidate inside another selected candidate is removed with its
parent and not processed twice. Removal keeps going after an error, reports
every item (removed with freed bytes, skipped with the reason, failed with a
plain reason such as "in use by another program" or "access denied", or not
started after cancellation), and stops between items when cancelled. Freed
space is the snapshot's exclusive bytes, which already account for hard links.

**Git worktrees.** A worktree candidate that carries a Git verdict (the desktop
with Git checks on, which also covers worktrees not checked yet; the CLI with
`--git`) is checked with Git again right before removal, using the read-only
commands in [git-inspection.md](git-inspection.md); unknown or changed state is
refused. Without a Git verdict (checks off, or the CLI without `--git`) a worktree
takes the explicit Review-first path and nothing claims Git was asked. Git does not
see ignored files or running programs, so worktrees stay Review first either way.
Cleanup never runs a Git command that changes anything: removing a linked worktree
folder leaves a stale entry in the original repository's `.git/worktrees`, and the
result tells the user to run `git worktree prune` there once they will not restore
the folder from the Trash.

**After removal.** The desktop subtracts removed subtrees from every ancestor
(`Snapshot::without`) and recomputes the analysis, so all views stay correct
without a rescan, and offers a rescan. A failure in permanent mode (or on
Windows) may have removed part of a folder; the result says so and recommends a
rescan.

## Threat model

| Threat | Mitigation |
| --- | --- |
| A wrong or stale selection removes the wrong folder | Only candidates of the current report; path must match the snapshot; live existence and kind checks. |
| The scanned folder is renamed away and a different ordinary folder is created at the same path | Folder identity recorded when candidates were found is compared right before removal; a different or unidentifiable folder is refused. |
| A worktree judged clean gains uncommitted work, a lock or a detached-only commit before removal | Git is asked again right before removal; anything but a fresh clean answer is refused. |
| A folder is swapped for a link or junction after the scan (by a user, a tool or an attacker with write access) so removal lands elsewhere | No-follow `symlink_metadata` at the path, Windows reparse-point attribute check, canonical-path comparison that catches links above the path. `std::fs::remove_dir_all` does not follow links inside the folder. The Trash backends move the named item, never a link target. |
| A heuristic misfires on an important location | Static and live guards for volume roots, home, temp, system folders and the program's own folder; Review-first items are never auto-selected. |
| Accidental confirmation (double click, Enter) | Cancel is the default button; the confirm button is disabled for 400 ms after the dialog opens; permanent deletion needs a separate tick. |
| An AI assistant or prompt injection triggers deletion | MCP exposes no removal tool. Tool output still says a response is not authorization to delete. |
| Unattended scripts delete by accident | The CLI refuses without a terminal unless `--yes` is passed explicitly. |
| An item is too large for the Windows Recycle Bin | Windows itself warns before it would delete instead of recycling (`FOF_WANTNUKEWARNING`). |
| A race between the live check and the removal (TOCTOU) | Reduced to a single item and milliseconds. RillDisk does not claim to defend against an attacker who can already rewrite the folder concurrently; that attacker can delete the files anyway. |
| A crash in the platform Trash layer | Caught per item and reported as a failure; the run continues. |

Not defended: an elevated or other user's process mutating the tree during the
run, network shares whose server deletes instead of recycling, a folder swapped
in during the scan itself (identity is recorded when candidates are found, right
after the scan), files Git ignores, and a program still working in a worktree.

## Implementation notes

* `rilldisk-core::cleanup`: `select_safe`, `plan`, `execute` (with a
  `CleanupControl` for progress and cancellation), `Guard::system()` and the
  report types; `Snapshot::without` for in-memory subtraction.
* Trash: the `trash` crate (MIT). Windows uses `IFileOperation` with undo and
  no UI; Linux follows the freedesktop.org Trash specification in Rust; macOS is
  switched to `NSFileManager`, because the crate's default asks Finder through
  `osascript`, which would run another program.
* CLI: `rilldisk reclaim PATH --delete [--safety regenerable,cache]
  [--min-size SIZE] [--permanent] [--yes] [--format json]`. Exit codes: 0 done,
  3 some items failed or were skipped, 130 cancelled (Ctrl+C stops after the
  current item), 1 error or not confirmed.
* Desktop: `src/cleanup_ui.rs` (toolbar, selection bar buttons, confirmation,
  progress and result dialogs) with small hooks in `src/app.rs`.

## Out of scope

* Any MCP tool that removes, moves or modifies files. The MCP server stays
  read-only.
* Removing anything that is not a reclaim candidate, including single files,
  arbitrary folders from Explore, or the contents of a candidate.
* Emptying the Recycle Bin / Trash, or clearing OS-managed temp folders in place.
* Running package managers, Git commands that change a repository, or any other
  program (`git worktree remove`, `git worktree prune`, `cargo clean`,
  `npm cache clean`); RillDisk offers those commands to copy. The read-only Git
  check above is the only exception.
* Elevation, scheduled or automatic cleanup, cleanup rules or policies, and
  restoring from the Trash inside RillDisk.
* Secure erase.
