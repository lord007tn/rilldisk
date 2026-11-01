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

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/): `type(scope): summary`
in the imperative, for example `fix(scan): keep junction loops out of totals`.
Release notes are generated from these by changelogithub
(`changelogithub.config.json`):

| Type | Use for | In release notes |
| --- | --- | --- |
| `feat:` | user-visible capability | Features |
| `fix:` | user-visible bug fix | Bug Fixes |
| `perf:` | measurable speed or memory change | Performance |
| `docs:` | documentation only | Documentation |
| `ci:` / `chore:` / `test:` / `refactor:` | workflows, tooling, tests, internals | hidden |

Mark breaking changes with `!` (`feat!: ...`) or a `BREAKING CHANGE:` footer.
Optional scopes: `core`, `cli`, `mcp`, `desktop`, `reclaim`, `update`, `release`.
Squash-merge titles must follow the same format.

## Releases

Releases are cut by pushing a `v<semver>` tag; see
[docs/proposals/updates.md](docs/proposals/updates.md#cutting-a-release). The tag
must equal `[workspace.package] version` and point at a commit on `main`; a
`-alpha.N` / `-rc.N` suffix publishes a pre-release. Run the Release workflow
manually on a branch for a dry run that builds and packages every target without
publishing. Network code belongs only in `crates/rilldisk-update`, and MCP must
not gain update tools.
