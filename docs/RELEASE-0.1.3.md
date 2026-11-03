# RillDisk 0.1.3

## Changes

- **Git worktree verdicts.** RillDisk can ask the user's own `git`, read-only,
  about each linked worktree: uncommitted or untracked changes, commits only on a
  detached HEAD, a lock, and the last commit. Verdicts: uncommitted changes,
  commits only here, locked, active, clean and idle (14 days by default), unknown.
  Worktrees stay Review first: Git does not see ignored files or running agents.
  Desktop: on by default (Settings can turn it off and set the idle threshold),
  verdict chips, a worktree inventory with scope, **Select clean idle worktrees**.
  CLI: `rilldisk reclaim --git [--stale-days N]`, JSON `candidates[].git`. MCP:
  unchanged, never runs Git. Scope: [git-inspection.md](proposals/git-inspection.md).
- **Safer removal.** Right before removal, a worktree that carried a Git verdict is
  checked with Git again and refused on any change, lock, detached-only commit,
  new activity after an idle verdict, or no answer. A missing report counts as
  unknown. Every folder must also be the one the scan found (volume and file ID,
  or device and inode); a different folder created at the same path is refused.
  `reclaim --git --delete` keeps a linked worktree only with a clean report.
  Scope: [cleanup.md](proposals/cleanup.md).
- **What removal frees is always on disk.** The desktop's Clean up figures,
  cards, selection bar, removal dialog and result now use allocated space without
  hard-linked files in both the Size and On disk views (Size used to show file
  length there, which overstated sparse and compressed files). They read "at
  least" only when every allocation is known and "about" otherwise; the CLI's
  reclaim and delete summaries do the same.
- **Incomplete scans are explicit.** `summary.entry_limit_reached` (set even when
  the issue list is full). The desktop shows a banner and offers a rescan with a
  60M or 120M entry limit when memory allows; Settings has the limit.
- **NTFS table scans show their progress.** A whole-drive scan as administrator
  reads the file table (MFT) in one pass before any folder exists. The live view
  now shows how far through the table it is (records read of the total, with a
  determinate bar), then that folders are being built, instead of an empty map.
  No speed change is claimed.
- **Desktop polish.** Show in Explorer / Finder / file manager on every
  right-click menu and in the Explore path bar ([reveal.md](proposals/reveal.md));
  chart menus stay open; the folder list lost its guide lines and accent bar; the
  version and update check moved to the bottom of the sidebar, with an update card
  only when there is news (the bottom bar is gone); readable size summary under
  the title; error toasts no longer show a success mark.

## Media provenance

All screenshots, `media/tour.gif` and `media/tour.mp4` (38.6 s, 1600×994, H.264,
captions burned in, [caption timing](media/tour-captions.json)) come from the app's
own scripted tours on the synthetic disk built by `scripts/make_demo_fixture.py`
(seeded; invented paths and drives; sparse files, so the demo's on-disk sizes are
small by design). The fixture creates real Git repositories and worktrees in fixed
states, so the verdicts shown are Git's own answers. Frames are recorded at one per
three app frames and played at 20 fps: timing is not real time and nothing in the
media is a speed claim. The removal dialog in the video is opened and cancelled.

```powershell
python scripts/make_demo_fixture.py "C:\RillDisk Demo"
rilldisk-desktop.exe "C:\RillDisk Demo" --theme light --capture shots
rilldisk-desktop.exe "C:\RillDisk Demo" --capture video --tour video
python scripts/make_demo_video.py video --ffmpeg <ffmpeg> --mp4 tour.mp4 --gif tour.gif
```

## Acceptance checklist

Executed on Windows 11 x64 for the candidate commit (receipts kept with the
release candidate, not in the repository):

- [x] Version 0.1.3 in the workspace and every internal dependency; `Cargo.lock`
      changes only the five workspace packages.
- [x] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] `cargo test --workspace`, including real-Git tests: verdicts, unchanged index
      files, stale-state refusals at removal (dirty, locked, detached-only commit,
      new activity, moved repository; unpushed-branch and explicitly unchecked
      worktrees still allowed), missing-report fail-closed, folder replacement
      refused, chart context menu stays open, Show in Explorer argument building.
- [x] `python scripts/test_mcp.py rilldisk.exe` against the release build (real stdio).
- [x] `rilldisk reclaim --git` on synthetic repositories: every verdict as designed.
- [x] Demo disk accounting: every candidate's allocation known; total frees equals
      total allocated (sparse files), shown as "freed on disk, at least".
- [x] Desktop scripted tours (full, welcome, updates, video) on the synthetic disk.
- [ ] `scripts/test_cli_signals.py`: needs Unix signal semantics; not run on Windows.
- [ ] Forward upgrade 0.1.2 → 0.1.3 (MSI and portable): needs the published 0.1.3
      artifacts and feed.
- [ ] Linux and macOS desktop runtime and release packages: built and tested by CI
      on the release tag; not run locally.
- [ ] Code signing: not available (builds are unsigned).

## Known limits

- Git checks see what `git status` sees: not ignored files, not running
  processes, and remote state only as of the user's last fetch.
- Folder identity is recorded when candidates are found, right after the scan; a
  folder swapped in during the scan itself is not detected.
- A scan stopped at its entry limit is incomplete; counts from it, including
  worktree counts, are lower bounds.
- The scripted update tour uses fictional release metadata and does not download
  or install anything.
