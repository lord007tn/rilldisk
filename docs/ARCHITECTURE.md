# Architecture

Status: phase one plus the first desktop release, November 1, 2026.

```text
                    ┌─ rilldisk (CLI): table / JSON / JSONL, reclaim, drives
rilldisk-core ──────┼─ rilldisk mcp: stdio, background job, paginated queries
  scan · reclaim    └─ rilldisk-desktop: egui on wgpu, treemap, tree, reclaim
  stats · volumes
       │
       ├─ Windows: batched Win32 directory metadata
       └─ portable walk: development / test fallback
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
Handles use Rust RAII. No content reads, subprocesses or elevation are used by the
production scanner.

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

## Desktop

`rilldisk-desktop` is a separate crate on eframe/egui with the wgpu renderer, so
CLI and MCP builds carry no GPU dependencies. Scans and analysis (reclaim, types,
largest files) run on a worker thread; the UI thread only polls progress counters
and swaps in a finished `Arc<Snapshot>`. The tree is virtualized (only visible
rows are laid out). The treemap is a squarified layout computed once per (zoom
root, size metric, canvas size) and cached as a single vertex mesh with four-
corner cushion shading, capped at 120,000 tiles; hit testing walks the cached
tiles. Single-child folder chains fold into one header. `--capture DIR` runs a
scripted tour that injects real pointer events and saves PNG frames for
documentation.

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
