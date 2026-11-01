# RillDisk contributor rules

* Windows is the first supported platform. macOS/Linux parity follows later.
* Production code is Rust. Native GPU UI belongs in a separate desktop crate;
  no JavaScript, Tauri, Electron or webviews.
* Core, CLI and MCP must share scanner and accounting logic.
* Phase one is read-only. Do not add file deletion/moves, content reads, shell
  execution, telemetry, elevation or a network listener without a scoped proposal.
* Preserve logical/unique/allocation distinctions. Partial scans, skipped paths,
  cancellation and unknown values must remain visible.
* Use AGPL-3.0-only for project code. Never commit private scan inventories.
* Run formatting, Clippy, Rust tests and real stdio MCP acceptance before handoff.
  Use synthetic fixtures for public evidence; make performance claims measurable.
* PRs ready for review must not remain drafts. Include visual evidence for GUI
  changes and recordings for interactions; CLI/MCP changes may explain why a GUI
  screenshot is not meaningful and include real transcripts instead.
* Do not automatically build Docker images in PR or push workflows.
