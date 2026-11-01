# Roadmap

## Phase 1 — disk data for people and tools

* Native Windows metadata scanner with concurrent batched enumeration.
* Explicit logical, unique and filesystem-reported allocation accounting.
* CLI table, JSON and JSONL output; cancellation and partial results.
* Local stdio MCP server with scoped roots and paginated snapshot queries.
* Windows correctness tests, black-box MCP acceptance, repeatable benchmarks and CI.

## Phase 2 — native GPU desktop

* Separate Rust desktop crate; initial renderer candidate is egui + wgpu.
* Drive/directory selection, live scan progress, cancellation and error review.
* Virtualized directory tree, largest-file view, search and GPU treemap navigation.
* Reuse core snapshots and queries; no renderer dependencies in CLI/MCP builds.
* Keyboard/accessibility checks, DPI scaling, Unicode/RTL path display and Windows
  frame-time/memory profiling. Record real screenshots and interaction video.

## Phase 3 — qualify the fast NTFS path

* Read-only bulk MFT backend, explicit privilege detection and ordinary-scan fallback.
* NTFS fixtures for hard links, resident data, compressed/sparse files, alternate
  streams, extension records, long paths and concurrently changing volumes.
* Compare counts and accounting before comparing speed with other analyzers.
* Publish cold/warm, end-to-end latency and peak working set on real Windows disks.
* USN incremental refresh after correctness and recovery behavior are accepted.

## Phase 4 — reviewed actions and portability

* Cleanup proposals with live identity revalidation and explicit user approval.
* Recycle-bin-first execution, protected paths, auditable results and recovery.
* Separate capability for mutation; the read-only MCP remains the default.
* Platform-native macOS/Linux scanners and distribution after Windows qualification.

No guaranteed scan-time promises, unattended deletion or auto-elevation are part
of phase one. Open a focused issue for proposals; measurements drive priorities.
