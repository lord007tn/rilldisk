# Roadmap

## Phase 1 — disk data for people and tools

* Native Windows metadata scanner with concurrent batched enumeration.
* Explicit logical, unique and filesystem-reported allocation accounting.
* CLI table, JSON and JSONL output; cancellation and partial results.
* Local stdio MCP server with scoped roots and paginated snapshot queries.
* Windows correctness tests, black-box MCP acceptance, repeatable benchmarks and CI.

## Phase 2 — native GPU desktop and reclaim (first release done)

* Done: separate `rilldisk-desktop` crate on egui + wgpu; drive picker, live
  progress, cancellation, issues view.
* Done: virtualized tree, GPU squarified treemap with zoom and breadcrumbs,
  largest files, file types, search, keyboard navigation, copy path/command.
* Done: shared reclaim engine (worktrees, orphaned agent worktrees, build output,
  dependencies, caches) in core, CLI (`rilldisk reclaim`) and MCP
  (`find_reclaimable`, `file_types`).
* Done: name-only entry storage and a compact child index (about half the memory).
* Next: accessibility audit (screen-reader labels for custom-painted views), DPI
  and RTL path rendering checks, frame-time and memory profiling at 20M+ entries,
  saved scans and scan-to-scan diff, optional Explorer reveal behind a scoped
  proposal.

## Phase 3 — qualify the fast NTFS path

* Read-only bulk MFT backend, explicit privilege detection and ordinary-scan fallback.
* NTFS fixtures for hard links, resident data, compressed/sparse files, alternate
  streams, extension records, long paths and concurrently changing volumes.
* Compare counts and accounting before comparing speed with other analyzers.
* Publish cold/warm, end-to-end latency and peak working set on real Windows disks.
* USN incremental refresh after correctness and recovery behavior are accepted.

## Phase 4 — reviewed actions and portability

* Done: removal of selected reclaim candidates with explicit approval,
  Recycle-Bin-first execution, protected paths, live path revalidation and
  per-item results ([proposal](docs/proposals/cleanup.md)).
* Next: directory identity revalidation (needs directory IDs in the snapshot) and
  restore from the Trash inside RillDisk.
* Separate capability for mutation; the read-only MCP remains the default.
* macOS/Linux distribution (the native Unix scanner, CLI, MCP and desktop already
  build and pass CI), plus measured macOS performance.

No guaranteed scan-time promises, unattended deletion or auto-elevation are part
of phase one. Open a focused issue for proposals; measurements drive priorities.
