# Contributing

RillDisk is AGPL-3.0-only. Submit contributions under that license. Keep scanner,
transport and renderer dependencies separate. New production code must be Rust;
test/benchmark scripts may use standard development tools.

Before a pull request, run the checks in the README. Include a regression test for
accounting, partial results or trust-boundary changes. Report actual Windows
validation separately from portable-backend checks. For performance changes,
include dataset shape, filesystem, hardware, build profile, warm/cold conditions,
counts, elapsed times and memory when measured. Do not publish personal paths.

For desktop screenshots and recordings, use the synthetic demo disk:
`python scripts/make_demo_fixture.py "C:\RillDisk Demo"`, then
`rilldisk-desktop "C:\RillDisk Demo" --capture DIR` saves deterministic PNGs and
GIF frames. Never capture a real home folder for public media.

Reclaim rules must stay metadata-only and conservative: prefer a marker file next
to the folder over a name alone, add a false-positive case to
`crates/rilldisk-core/tests/reclaim.rs`, and give every rule a concrete suggested
action.

Explain any meaningful AI assistance and verify the resulting behavior. UI
changes need screenshots and interaction recordings when relevant. CLI/MCP-only
changes can use real transcripts and machine-readable acceptance evidence with
an explanation of why a graphical screenshot is not useful.

PRs ready for review must not remain drafts. CI must not automatically build
Docker images on pull requests. Do not commit credentials, local scan inventories
or reports containing private filenames.
