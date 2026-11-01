# Contributing

RillDisk is AGPL-3.0-only. Submit contributions under that license. Keep scanner,
transport and renderer dependencies separate. New production code must be Rust;
test/benchmark scripts may use standard development tools.

Before a pull request, run the checks in the README. Include a regression test for
accounting, partial results or trust-boundary changes. Report actual Windows
validation separately from portable-backend checks. For performance changes,
include dataset shape, filesystem, hardware, build profile, warm/cold conditions,
counts, elapsed times and memory when measured. Do not publish personal paths.

Explain any meaningful AI assistance and verify the resulting behavior. UI
changes need screenshots and interaction recordings when relevant. CLI/MCP-only
changes can use real transcripts and machine-readable acceptance evidence with
an explanation of why a graphical screenshot is not useful.

PRs ready for review must not remain drafts. CI must not automatically build
Docker images on pull requests. Do not commit credentials, local scan inventories
or reports containing private filenames.
