# Lessons from a real cleanup, and what RillDisk should do about them

Status: plan, November 13, 2026. Items 1–4 of the priority list are implemented
on the branch that adds this file; the rest are proposals.

## Where this comes from

On November 12–13, 2026 a Windows development machine was cleaned up from 73 GB to
249 GB free. RillDisk 0.1.3 did the scanning and produced the worktree candidate
list (`rilldisk reclaim --git`). Almost every decision after that was made by
throw-away Python scripts written alongside it: who owns a folder, whether a
worktree should be kept, archived or removed, how to archive without touching the
real Git index, how to delete 100+ GB of `node_modules`-heavy folders without
following junctions, and what was left at the end and why. Those scripts are the
gap this plan closes. No script, path or archive from that run is part of the
repository; the tests use synthetic fixtures only.

The single most important finding: **most of the 70 worktrees that were kept were
kept because something owned them** (an open assistant thread, a recent Codex
thread, a running process, a lock), not because of their Git state. Git state
decided what to do with the rest.

## Priorities

Ranked by safety and value per unit of effort. Effort: S (hours), M (a day or
two), L (several days, or needs a scoped proposal under AGENTS.md first).

1. **Protected paths** (lesson 11) — done here. A never-touch list that hides
   candidates from every suggestion surface and is refused by cleanup.
2. **Idle time from file times** (lesson 2) — done here. Folder times no longer
   make a partially cleaned folder look active.
3. **Verdict nuance and a next step** (lesson 4) — done here. Commits whose patch
   already landed elsewhere are not "only here", and each worktree gets keep /
   push / archive-then-remove / remove.
4. **Links inside candidates** (lesson 5) — verified here by a regression test;
   the delete path already never followed them. Showing junction targets remains.
5. **Owner signals** (lesson 1) — the highest value item left, but it reads other
   programs' databases and the process table, so it needs its own proposal.
6. **Safe archive before remove** (lesson 3) — needs a proposal: it writes refs and
   files.
7. Everything else, in table order.

## Lessons, current behaviour and proposed changes

Line references are to this branch. "0.1.3" describes behaviour before it.

| # | Lesson | Current RillDisk behaviour | Proposed change | Effort | Value |
| --- | --- | --- | --- | --- | --- |
| 11 | A user-configurable never-touch list, applied to suggestions and delete alike | 0.1.3: only built-in guards (home, system, temp, volume roots, own exe) in `Guard::system` ([cleanup.rs:213](../crates/rilldisk-core/src/cleanup.rs#L213)); nothing user-defined. **Now:** `protected-paths.txt` in the RillDisk state folder plus `rilldisk reclaim --protect PATH` ([protect.rs:20](../crates/rilldisk-core/src/protect.rs#L20)); `ReclaimReport::exclude_protected` ([reclaim.rs:841](../crates/rilldisk-core/src/reclaim.rs#L841)) hides candidates in the CLI, MCP and desktop; `Guard::system` loads the same file and `Guard::protect_user` ([cleanup.rs:324](../crates/rilldisk-core/src/cleanup.rs#L324)) refuses the path, its contents and its parents; an unreadable file refuses every removal | Done. Next: show an unreadable list or ignored lines in the desktop Clean up view (today only the CLI warns; the desktop just refuses removals), a Settings editor (both need screenshots), and a "why hidden" listing (`--show-protected`) | S | Very high |
| 2 | Recency must come from the newest file, not folder times | 0.1.3: `idle_days` used the folder's rolled-up time, which includes every folder's own write time ([lib.rs:1276](../crates/rilldisk-core/src/lib.rs#L1276)), so removing entries made a parent look active. **Now:** the candidate walk takes the newest *file* time ([reclaim.rs:1366](../crates/rilldisk-core/src/reclaim.rs#L1366), [reclaim.rs:986](../crates/rilldisk-core/src/reclaim.rs#L986)); the folder's own time only when it holds no files. Git verdicts and the removal re-check use the same value | Done. Next: a "touched in the last N days → keep" filter for every category, not just Git worktrees (`--min-idle-days`) | S | High |
| 4 | Distinguish uncommitted vs untracked, unpushed vs only-here, patch-equivalent commits, detached HEAD; output push / archive+remove / remove / keep | 0.1.3: `changed_files` and `untracked_items` already separate ([git.rs:42](../crates/rilldisk-core/src/git.rs#L42)); `commits_only_here` for detached HEADs ([git.rs:51](../crates/rilldisk-core/src/git.rs#L51)); a cherry-picked or rebased commit still counted as lost; no recommendation. **Now:** `commits_only_here_applied` and `unpushed_applied` from `git rev-list --cherry-mark` against a remote-tracking upstream, each remote's default branch and local `main`/`master`, each pair then confirmed byte for byte with `git patch-id --verbatim`, because Git's own matching ignores whitespace ([git.rs:493](../crates/rilldisk-core/src/git.rs#L493)); merges, empty commits and Git errors never count as applied. `next_step` ([git.rs:106](../crates/rilldisk-core/src/git.rs#L106)): locked, recent or unknown → keep; idle with unsaved work → archive then remove; idle, clean, branch ahead of its remote → push; otherwise remove. CLI prints it | Done. Next: show `next_step` in the desktop chips; squash-merged branches (patch of many commits) are still not detected — compare the branch's combined diff with the upstream as a later step | M | High |
| 5 | Junctions: never double-count, show targets, never follow while deleting; long paths; read-only files | Scanner never enters links or reparse points ([scanner test](../crates/rilldisk-core/tests/scanner.rs#L106)), so a junction such as `~\.codex\worktrees` is counted once, where it really lives. Cleanup refuses a candidate that is or sits below a link ([cleanup.rs:713](../crates/rilldisk-core/src/cleanup.rs#L713)); permanent removal is `std::fs::remove_dir_all` ([cleanup.rs:746](../crates/rilldisk-core/src/cleanup.rs#L746)), which removes a link without entering it and handles read-only files and long paths. **Now** covered by `permanent_cleanup_removes_links_inside_a_candidate_but_not_their_targets` | Done (test). Next: record link targets as metadata in the snapshot and show "→ target" for link entries in Explore, so a junction is visibly not duplicate usage | M | Medium |
| 1 | Report an owner and a keep reason per candidate: open T3 threads, Codex threads, running processes, locked worktrees; pluggable read-only owner providers | Only the Git lock is known ([git.rs:59](../crates/rilldisk-core/src/git.rs#L59)). `agent` names the tool that *created* a worktree from its path ([reclaim.rs:1483](../crates/rilldisk-core/src/reclaim.rs#L1483)), not whether it is still in use | New `owners` module with an `OwnerProvider` trait returning `{kind, label, detail, since}`; built-ins: Git lock, running processes (Windows: command line via `NtQueryInformationProcess`, no memory reads; Unix: `/proc/*/cwd`), and optional, opt-in readers for T3 (`statev2.sqlite`: thread `worktreePath` and provider-session `cwd`, open = not settled and not archived) and Codex (`state_5.sqlite` `threads`: `cwd`, `archived`, `updated_at`, `is_pinned`), opened read-only and immutable, any error or unknown schema → "owner unknown", never "unowned". Candidates gain `owners[]` and `keep_reason`; `next_step` becomes Keep when owned; cleanup re-checks owners right before removal. Needs a scoped proposal (content reads of another program's files) | L | Very high |
| 3 | Archive before delete without touching the real index; recovery refs; verified patches; size guard; bundle for duplicate clones | Not done. Cleanup only suggests "commit or stash" ([git.rs suggested_action](../crates/rilldisk-core/src/git.rs#L295)); the Git proposal forbids any Git write | An explicit, separate "Archive" action: copy the index to a temp `GIT_INDEX_FILE`, `git add -A`, `git diff --cached --binary HEAD`; a staged-only patch from the real index (read-only); `refs/cleanup-recovery/<stamp>/<sha>` for HEAD and only-here commits; verify with `git apply --cached --check` on a `read-tree HEAD` temp index; refuse patches over a size limit (300 MB worked); manifest + README in the archive folder; `git bundle create --all` + `bundle verify` for whole clones. Amend git-inspection.md (writes refs and objects) | L | Very high |
| 7 | Deletion throughput and resumability: parallel, time budget, append-only JSONL log, re-check blockers per item, `git worktree prune` after | Sequential, one item at a time ([cleanup.rs:531](../crates/rilldisk-core/src/cleanup.rs#L531)) with a full live re-check per item (identity, links, guard, Git). Cancellation stops between items. Resuming is natural: a rescan no longer lists removed folders. `git worktree prune` is only suggested ([cleanup.rs:143](../crates/rilldisk-core/src/cleanup.rs#L143)) | `--jobs N` for permanent mode (Recycle Bin moves stay sequential), `--time-budget 90m` (stop starting new items), `--log results.jsonl` appended per item, and a prune suggestion that names each repository. Running `git worktree prune` itself stays out of scope unless the proposal is amended | M | Medium |
| 6 | Honest freed space with pnpm hard links, across candidates | Any file with another link in the scan counts as shared and never as freed ([reclaim.rs:1378](../crates/rilldisk-core/src/reclaim.rs#L1378), flag at [lib.rs:122](../crates/rilldisk-core/src/lib.rs#L122)). So totals never double-count across candidates, but they under-count when every link of a file is inside the selection, and over-count when the other link is outside the scanned folder (a pnpm store elsewhere on the drive) | Keep a link-group id per multi-link file in the snapshot; compute freed space for a *selection* (plan) as files whose every link in the scan is selected; on NTFS read the link count (the MFT has it) so links outside the scan are known | M | Medium |
| 8 | Classify non-git folders under worktree roots: orphan checkouts, empty residue, regenerable vs other bytes | Orphaned agent worktree rule (no `.git` directly or one level down) ([reclaim.rs:917](../crates/rilldisk-core/src/reclaim.rs#L917)). A `.git` pointer to a missing gitdir is a "Git worktree" whose Git check fails as Unknown | Label Unknown-with-"not a git repository" as *orphan checkout*; mark empty orphans as *empty residue* (suggested action: delete); show regenerable bytes (nested Regenerable candidates) vs other bytes for orphan folders | S | Medium |
| 9 | Duplicate clones (same root commit or remote under another folder name) | Ordinary clones are not candidates; the Git proposal excludes them | Opt-in: for top-level repositories under a scanned work folder, group by root commit (`git rev-list --max-parents=0 HEAD`), flag older idle members as "duplicate clone", suggest bundle + remove. Needs a proposal amendment | M | Medium |
| 10 | Agent-tool caches as reclaim targets, each with a safety check; prefer a rules/catalog file | Rules are compiled in ([reclaim.rs:205](../crates/rilldisk-core/src/reclaim.rs#L205)); none for Codex or Claude state | Add metadata-only rules: `.codex/tmp`, `.codex/.tmp`, `.codex/trash` entries idle ≥ 7 days; Codex `tmp` and `validation`. Checks that need content (Codex backup manifests' HEAD still in its repo; Claude `projects/<dir>` whose recorded `cwd` is gone) need a proposal. Later, move `LOCATIONS` to a data file shipped with the binary | M | Medium |
| 12 | Large old media and evidence (116 GB of renders and screenshots, 54 GB of benchmark results) | Not detected; Largest files and File types show it only indirectly | A `large_media` category: folders whose files are mostly video/audio/image/archives by extension, ≥ 1 GB, idle ≥ N days. Review first, suggestion only: cleanup refuses it like Recycle Bin folders | M | High |
| 13 | MCP: paging hint at the 500 cap; are verdicts and owners exposed? | Pagination existed (`next_offset`, [mcp.rs:244](../crates/rilldisk/src/mcp.rs#L244)) but the error just said "limit must be between 1 and 500". The MCP server never runs Git (git-inspection.md), so no verdicts. **Now:** the error explains paging ([mcp.rs:94](../crates/rilldisk/src/mcp.rs#L94)); `find_reclaimable` applies protected paths and reports `protected_excluded` ([mcp.rs:277](../crates/rilldisk/src/mcp.rs#L277)); `idle_days` is file-based | Done in part. Next: owners from item 1 in MCP output (read-only, no program execution needed for database readers); Git verdicts stay CLI/desktop-only | S | Medium |
| 14 | A final "what remains and why" listing, grouped by root | `reclaim` lists candidates largest first ([main.rs:818](../crates/rilldisk/src/main.rs#L818)); `next_step` now explains Git's view | `rilldisk reclaim --git --group-by root`: per root, each worktree with size, branch, state and keep reason (owner, recent, locked, unsaved work); JSON the same | M | High |

## Implemented on this branch

* **Protected paths.** `rilldisk_core::protect`: one absolute path per line in
  `protected-paths.txt` (`%LOCALAPPDATA%\RillDisk` on Windows,
  `~/Library/Application Support/RillDisk` on macOS, `$XDG_STATE_HOME/rilldisk`
  elsewhere), `#` comments, `~` for home, relative paths reported and ignored. A
  protected path hides every candidate that is it, is inside it or contains it,
  in `rilldisk reclaim` (table, JSON, `--delete`), MCP `find_reclaimable` and the
  desktop; cleanup refuses the same with "is a path you protected". A file that
  exists but cannot be read makes cleanup refuse every item instead of silently
  dropping the protection. `rilldisk reclaim --protect PATH` adds paths for one run.
* **File-based idle time.** `Candidate::newest_modified_unix_ms` and `idle_days`
  come from the newest file in the folder.
* **Git nuance.** `GitState::commits_only_here_applied` and
  `GitState::unpushed_applied`, `GitReport::next_step`, CLI `next:` line. Detached
  commits that are all applied elsewhere no longer give "Commits only here", so
  such a worktree can be removed after the usual re-check.
* **Links.** A regression test that permanent removal of a candidate deletes a
  junction (or symlink) inside it and leaves the target intact, and that the scan
  never counted the target.
* **MCP.** Paging hint in the limit error; protected paths applied.

## Reference: the archive recipe that worked

For item 3, this is the sequence that never touched a real index and could always
be verified:

1. `git rev-parse HEAD`, `git rev-parse --git-common-dir`, `git symbolic-ref -q HEAD`.
2. `git update-ref refs/cleanup-recovery/<stamp>/<sha> <sha>` (HEAD and any commit
   only in this worktree).
3. Copy `<gitdir>/index` to a temp file; with `GIT_INDEX_FILE` set to it run
   `git add -A -- .` and `git diff --cached --binary HEAD` → all-changes patch.
   Refuse when the patch is larger than a limit (untracked build output).
4. Without the temp index: `git diff --cached --binary HEAD` → staged-only patch.
5. Verify: a second temp index, `git read-tree HEAD`, then
   `git apply --cached --check <patch>`.
6. Write a manifest (repo, head, branch, patch sizes, verified) and a README with
   restore steps next to the patches.

`git add -N` followed by `git reset` on the real index lost a staged index once
during the run and had to be rebuilt from loose objects; that is why step 3 uses a
copy.
