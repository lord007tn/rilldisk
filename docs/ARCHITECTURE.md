# Architecture

Status: phase one plus the first desktop release and the Linux/macOS backend,
November 1, 2026.

```text
                    ┌─ rilldisk (CLI): table / JSON / JSONL, reclaim, drives
rilldisk-core ──────┼─ rilldisk mcp: stdio, background job, paginated queries
  scan · reclaim    └─ rilldisk-desktop: egui on wgpu, overview, treemap, rings, clean up
  stats · volumes
       │
       ├─ Windows: batched Win32 directory metadata
       └─ Linux, macOS: readdir + one no-follow stat, mount-aware
```

## Phase-one contracts

The core stores a flat arena of entries. Parent IDs always precede children, so a
reverse pass computes subtree totals, file counts and newest modification times in
linear time. Each entry stores only its final name (`Box<OsStr>`); full paths are
rebuilt from the parent chain on demand (`Snapshot::path`, `Snapshot::view`). This
roughly halves memory on multi-million-entry scans. A compressed child index
(offsets + IDs, sorted by size) serves tree browsing, treemap layout and
`parent_id` queries without scanning the arena. JSON serialization is textual. Entries have stable IDs within
one scan, not across scans. Filesystem mutations can change names and sizes while
enumeration runs. The engine does not promise a point-in-time snapshot.

Each generation of directories is processed in groups of up to 4,096 on an
explicit Rayon pool (default: logical cores, clamped to 8-32), so work stealing
balances uneven folders without a per-group barrier dominating. The Win32 backend uses a directory handle opened with sharing
for read/write/delete and `FILE_FLAG_OPEN_REPARSE_POINT`, checks its attributes,
then requests `FileIdBothDirectoryInfo` records in a reusable 64 KiB aligned buffer.
Native buffer offsets and UTF-16 lengths are checked before records are decoded.
Handles use Rust RAII. No content reads or subprocesses are used by the
production scanner, and it never elevates. When the process is already elevated,
`enable_backup_privilege` (called by the CLI scan commands and the desktop, never
by MCP) enables `SeBackupPrivilege`, so the same `FILE_FLAG_BACKUP_SEMANTICS` open
lists ACL-protected folders read-only. `elevation()` reports the token state for
any backend that needs administrator rights. Every issue carries a `kind`
(`access_denied`, `not_found`, `in_use`, `stalled`, `entry_limit`, `other`). Issues
from an OS error take their kind from the raw OS error and keep it as `os_error`.
Issues the scanner raises itself (stalled folders, the entry limit, volume
warnings) set `kind` directly and have no `os_error`. See
[the elevation proposal](proposals/elevation.md).

File identity uses the volume serial plus file ID, and zero IDs are treated as
unknown rather than collapsing every such file together. Logical bytes describe
names; unique bytes describe discovered file identities. Unknown allocation
propagates as `null` rather than becoming zero. Hard-link ownership is attributed
to first encounter. The user-visible allocation figure excludes directory and
volume overhead and unenumerated alternate streams.

The allocation unit is queried once per scan. NTFS directory records can report
an implementation-rounded byte length for resident data inside the MFT. Values
below a cluster are normalized to zero separately allocated data clusters. A
Windows regression test independently confirms absence of extents with
`FSCTL_GET_RETRIEVAL_POINTERS`; logical bytes remain unchanged.

Cancellation is cooperative between directory batches and records. A blocking
filesystem operation can delay it. Every worker atomically reserves a record from
one shared budget before retaining it, bounding the combined arena and in-flight
records. Reaching the bound (default 30 million entries) produces one issue and a
partial result. Progress counters update during enumeration. Paths, frontier copies,
the hard-link set and query sorting add memory. Five million entries can require substantial RAM. A
compact name arena and indexed queries should follow profiling, not precede it.

## Unix backend (Linux, macOS)

`unix-readdir` plugs into the same parallel walk, so scheduling, the shared entry
budget, cancellation and stall reporting are identical to Windows. Each directory
is listed with `std::fs::read_dir` (`getdents64` on Linux, `readdir` on macOS).
The listing's type (`d_type`) is enough for symlinks, sockets, FIFOs and devices,
which therefore cost no further call. Regular files and directories get one
no-follow metadata call each: `statx`/`fstatat` relative to the open directory
handle on Linux, `lstat` on macOS. Symlinks are listed as links and never followed.

* **Allocation** is `st_blocks * 512` (512-byte units on both kernels), so sparse
  files report what they occupy. Directory blocks are excluded, as on Windows.
* **Hard links** use `(st_dev, st_ino)`. Only files with `st_nlink > 1` enter the
  identity table; a single-link file cannot be a duplicate, so the table stays
  small. First-encounter attribution, `unique_logical_bytes`,
  `hard_link_duplicate` and `multiple_links` behave exactly as on Windows.
* **Times** come from `st_mtime` with nanoseconds, truncated to milliseconds.
* **Unreadable folders** (`EACCES`) and entries that vanish between listing and
  `stat` become issues, and the result is partial.

A scan stays on the root's filesystem by default, like `du -x`. At scan start the
backend records the root's device and, on Linux, every mount point below the root
from `/proc/self/mountinfo` (macOS: `getfsstat`). A directory on another device, or
listed as a mount point (which also catches same-device bind mounts), is recorded
as a directory but not entered: `fully_listed` stays false, its sizes are not
included, and `summary.skipped_mounts` counts it. `ScanOptions::cross_filesystems`
(`--cross-filesystems`) enters real filesystems, but virtual ones are always
skipped: `/proc`, `/sys`, `/dev` and `/run` at the top of the tree, and any mount of
type proc, sysfs, devtmpfs, tmpfs, cgroup, overlay, squashfs (snaps), autofs and
similar. On macOS `/dev`, `/Volumes` and `/System/Volumes` are always skipped when
reached from `/`. `/System/Volumes/Data` holds the same files that firmlinks such as
`/Users` and `/Applications` show, so the Data volume's device counts as part of a
root on the system volume and its own mount point is never entered twice.

Known gaps: bind mounts are entered with `--cross-filesystems` and can then repeat
content; APFS clones and btrfs/XFS reflinks share blocks that `st_blocks` reports
once per file, so allocation over-counts them; btrfs subvolumes have their own
device numbers and are skipped without the option (as with `du -x`).

`volumes()` lists disk-backed filesystems. Linux reads `/proc/self/mountinfo`,
keeps ext*, XFS, btrfs, ZFS, FAT/exFAT/NTFS and similar plus WSL `drvfs` drives,
drops pseudo, overlay, snap, network and `/boot` mounts, shows one row per
`major:minor` (the shortest mount point), and reads capacity with `statvfs`. Labels
come from `/dev/disk/by-label`; removable media from sysfs or `/media`. macOS lists
`/` and browsable local volumes under `/Volumes` with `getfsstat`. `free_bytes` is
`f_bfree`, matching the Windows total-free figure.

Measured so far (Linux under WSL2 only; macOS is verified by CI tests, not timed):
file and folder counts of `/usr` and `/` match `find -xdev`, unique bytes match
`du --apparent-size` apart from symlink lengths, and allocated bytes match `du -B1`
apart from directory blocks. See [PERFORMANCE.md](PERFORMANCE.md#linux-wsl2-observation).

## MCP state

The official `rmcp` SDK owns transport/protocol negotiation. The executable
exposes a stdio transport, with no listener or authentication service. Startup
requires explicit allowed roots. Requests canonicalize paths and enforce
component-wise containment. One background scan runs at a time, on blocking
workers. A completed snapshot is shared immutably. A later scan replaces it and
invalidates its ID. Status polling and cancellation remain available during work.
Shutdown requests cancellation. Tool output is paginated and has explicit
completion/error metadata. Filenames are untrusted data, including prompt-like text.
At most two snapshot queries run concurrently; excess callers receive a retryable
capacity error. Permits remain held until blocking query work actually finishes.

## Reclaim analysis

`find_reclaimable` walks the finished snapshot once. Directory rules match names
plus marker siblings or children (`target` + `Cargo.toml`, `node_modules` +
`package.json`, `.venv` containing `pyvenv.cfg`, Unity `Library` beside `Assets`
and `ProjectSettings`). A `.git` *file* marks a linked worktree unless the
enclosing repository declares submodules. Children of an AI agent's `worktrees`
folder with no `.git` are orphaned worktrees. Known cache locations match trailing
path components. Candidates nested inside a regenerable or cache candidate are
dropped; candidates inside a worktree are kept with `nested_in` and excluded from
totals. Idle days come from the newest modification time in the subtree. No file
contents are read and nothing is modified.

## Cleanup

`rilldisk-core::cleanup` is the only code that changes the filesystem
([scope and threat model](proposals/cleanup.md)). `plan` turns selected entry
IDs into the outermost reclaim candidates of the current report and refuses
anything else (non-candidates, the scan root, Trash folders, volume roots, the
home, temp and system folders and their ancestors, folders holding the running
executable). `execute` rebuilds the plan, then for each item re-checks the live
path (exists, still a directory, no link or reparse point at or above it, same
canonical path, not a mount point) and moves it to the Recycle Bin / Trash
(`trash` crate; NSFileManager on macOS, so no `osascript`) or, when asked,
deletes it with `std::fs::remove_dir_all`, which never follows links. It keeps
going on errors, reports each item, and checks a `CleanupControl` for
cancellation between items. `Snapshot::without` subtracts removed subtrees from
their ancestors and renumbers entries so the desktop can re-analyze without a
rescan. The CLI (`reclaim --delete`) and the desktop (`cleanup_ui.rs`) both call
it; MCP does not.

## Desktop

`rilldisk-desktop` is a separate crate on eframe/egui with the wgpu renderer, so
CLI and MCP builds carry no GPU dependencies. Scans and analysis (reclaim, types,
largest files, agent grouping) run on a worker thread; the UI thread only polls
progress counters and swaps in a finished `Arc<Snapshot>`.

* **Design tokens** live in `style.rs`: three color identities (Tide, Moss,
  Aurora), each with a light and dark theme, read through one `t()` accessor.
  Surfaces are separated by tone, not outlines. Fonts are the platform's own
  (Segoe UI Variable, SF, or a common Linux sans) with a variable-weight semibold.
  The chosen palette and theme are remembered in a small `desktop.txt` beside the
  update preferences.
* **Treemap** (`treemap.rs`): squarified layout of up to four levels below the
  shown folder, computed once per (root, metric, canvas, theme) and tessellated
  with egui's feathering into one cached mesh of rounded tiles (capped at 40,000).
  Hit testing walks the cached tiles; single-child folder chains fold into one
  label. Tiles are colored by top-level folder (a gradient position shared with
  the rings) or by file type.
* **Rings** (`sunburst.rs`): a six-level sunburst built into one mesh of
  anti-aliased annular sectors (each a 4 × n vertex grid whose outer rows fade over
  one pixel) and hit-tested in polar coordinates.
* **Views**: Overview, Explore (map or rings plus a virtualized folder list),
  Clean up (virtualized, grouped by category; tick folders or "Select all safe",
  then copy a plan or move them to the Recycle Bin / Trash after a confirmation
  dialog, see `cleanup_ui.rs`), Largest files, File types, Issues and Search.
* **Elevation** (`elevation.rs`): the Issues view grouped by kind, the
  Administrator badge and the restart offer (Windows `ShellExecuteExW` "runas" on
  the same executable and scan root; Linux `pkexec`; macOS Full Disk Access
  settings). The window closes only after the elevated instance started.

`--capture-cleanup DIR` captures the selection bar and cleanup dialogs; it only
removes folders from a synthetic fixture (a scan with
`.rilldisk-synthetic-fixture` at its root, written by
`scripts/make_demo_fixture.py`, or one inside the temp folder).

`--capture DIR` runs a scripted tour that injects real pointer events and saves PNG
frames for documentation, with invented drives in place of the real ones.

## Connecting AI clients

`rilldisk-mcp-setup` finds AI clients (Claude Code, Claude Desktop, Codex,
Cursor, VS Code, Windsurf, Gemini CLI, Zed, OpenCode, Cline), adds or removes
the `rilldisk` entry in their user config, and checks an entry by starting it
and doing an MCP `initialize` + `tools/list` handshake over stdio. It has no
async runtime and no MCP library, so the desktop can depend on it. JSON/JSONC
configs are edited through a concrete syntax tree (`jsonc-parser`) and TOML
through `toml_edit`, so comments and formatting survive. Every function takes
an `Env` (home, app data, PATH) rather than reading the process environment, so
tests run against temporary folders. The CLI (`rilldisk mcp setup|doctor`) and
the desktop (`src/assistants.rs`) call it. Scope:
[proposals/mcp-setup.md](proposals/mcp-setup.md).

## Updates and releases

`rilldisk-update` is the only crate with network code and depends on nothing
else in the workspace. The CLI (`rilldisk update`) and the desktop
(`src/update.rs`) call it. `rilldisk-core` and MCP never do. It reads the public
GitHub releases list, picks the newest release on the channel that ships an
archive for this target, verifies that archive against the release's
`SHA256SUMS`, and swaps the executables by rename with rollback.
`.github/workflows/release.yml` and `scripts/package_release.py` produce those
archives for eight targets (Windows x64/ARM64, macOS Apple silicon/Intel, Linux
x86_64/aarch64 against glibc 2.28, and static musl CLI builds), and
`scripts/package_installers.py` turns them into an MSI, `.deb`, `.rpm`,
AppImage and `.dmg` from `packaging/`. Copies installed by one of those packages
carry an `installed-by` marker (or live in `RillDisk.app`); the updater leaves
them alone and links the matching new package from `latest.yml` instead. Scope,
privacy and threat model are in [proposals/updates.md](proposals/updates.md);
installation per system is in [INSTALL.md](INSTALL.md).

## Direct NTFS backend

WizTree documents direct MFT access as its NTFS speed advantage. This first
backend is not equivalent. A fast path must read NTFS metadata in bulk, reconstruct
parent/name relationships, handle extension records and resident/nonresident data,
account for hard links and alternate streams, handle malformed records, and keep
privilege requirements explicit. Enumerating USN names alone does not provide all
the size/accounting data required for a disk analyzer.

Before enabling auto-selection: fixture parity with the directory backend,
comparison against an independent Windows inventory, no writes to the volume,
clear fallback behavior, and repeated whole-volume measurements. Incremental USN
updates follow an accepted full-scan implementation.

## Sources

* [Microsoft: GetFileInformationByHandleEx](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getfileinformationbyhandleex)
* [Microsoft: FILE_ID_BOTH_DIR_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_both_dir_info)
* [Official MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
* [WizTree's explanation of MFT scanning](https://diskanalyzer.com/)
