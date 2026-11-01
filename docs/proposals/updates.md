# Proposal: automated releases and opt-in updates

Status: implemented, November 1, 2026. The desktop UI wiring is pending.

AGENTS.md requires a scoped proposal before RillDisk gains network behaviour.
This document is that scope. Anything not described here is out of scope:
RillDisk still has no telemetry, no network listener, no elevation, and it never
sends anything about your machine, files or scans.

## Research

**T3 Code** (`pingdotgg/t3code`, Electron). `release.yml` runs on `v*.*.*`
tags, an hourly nightly schedule and manual dispatch. A preflight job resolves
the channel (stable, nightly, preview) and sets `prerelease`/`make_latest` from
the version suffix. Quality and test jobs gate per-platform desktop builds.
Windows binaries are signed through Azure Trusted Signing (jsign) and macOS
builds with a Developer ID plus notarization. The publish job writes
`SHA256SUMS` from the signed bytes. It merges per-arch electron-updater
manifests (`latest*.yml`, `nightly*.yml` plus blockmaps) and refuses to attach
updater metadata to preview releases, so no installation can be offered a
preview. It then publishes with `softprops/action-gh-release`; pre-releases stay
drafts until every asset is attached. Release notes combine GitHub's generated
notes with a model-written summary. The CLI ships as an npm launcher that
downloads the matching release archive. In-app updates use electron-updater,
which reads the per-channel YAML feed and verifies the signature and SHA-512 it
lists.

**ArtCraft** (`storytold/artcraft`, Tauri). Per-OS workflows run on pushes to
`release` branches and call `tauri-apps/tauri-action`. That action creates a
draft GitHub release with an MSI, an NSIS `setup.exe`, a universal DMG and an
`.app.tar.gz`. The release body is static text, and nothing generates a
changelog. Recent releases (for example `artcraft-v0.41.0`) carry no
`latest.json` or `.sig` updater artifacts, so users update by downloading
manually.

**What we take from them:** tag-driven releases; the pre-release flag derived
from the version suffix; a `SHA256SUMS` written from the final bytes; publishing
only once every asset is attached; never feeding an updater from a channel it
does not follow; a dry run that exercises the whole pipeline without
publishing.

### Rust options considered

| Option | Why not (or why) |
| --- | --- |
| cargo-dist (`dist`) + axoupdater | Generates and owns `release.yml` (CI fails when it drifts), which fights our license packaging, START-HERE files and changelogithub. axoupdater only updates installs made by dist's installers (it needs their install receipt), so none of today's zip users could update. It also updates by downloading and **running the installer script** (sh/PowerShell). That is shell execution, which AGENTS.md rules out. |
| `self_update` crate | Replaces only the running executable, but we ship two. It has no `SHA256SUMS` support, and its default HTTP stack is reqwest. We would still write channel selection, package-manager detection and the opt-out ourselves, so it saves little for a larger dependency. |
| Tauri updater / electron-updater | Need a webview runtime. Not applicable. |
| **Small `rilldisk-update` crate (chosen)** | About 600 lines on `ureq`, `semver`, `sha2`, `zip` and `tar`. The GitHub Releases API is the feed, so there is no second manifest to keep in sync. `SHA256SUMS` is the integrity source. It replaces both binaries and never runs another program to install. Windows and macOS use the OS TLS stack and trust store (Schannel, Security.framework), so no C toolchain is needed there. Linux uses rustls with Mozilla roots. |

Installers (MSI, pkg, shell/PowerShell one-liners) and package-manager
manifests (winget, Scoop, Homebrew) are follow-ups. The updater already detects
those installs and leaves them alone.

## Release pipeline

`.github/workflows/release.yml`:

1. **Plan.** On a `v*` tag push, the tag must equal `v` + `[workspace.package]
   version`, so the binary reports the same version the updater compares. The
   tag must also point at a commit on `main`. A `-suffix` (`-alpha.N`,
   `-rc.N`) marks the release as a pre-release.
2. **Build.** One job per target: `x86_64-pc-windows-msvc` (windows-2025),
   `x86_64-unknown-linux-gnu` (ubuntu-22.04 for a glibc 2.35 baseline),
   `aarch64-apple-darwin` and `x86_64-apple-darwin` (macos-15, x86_64
   cross-compiled). Native targets run a smoke test: `--version`, the update
   opt-out (exit 11) and the stdio MCP acceptance test.
   `scripts/package_release.py package` builds the archive. Each archive holds
   both binaries, LICENSE, README.md, START-HERE.md, BUILD-MANIFEST.json, the
   Rust toolchain notices and the THIRD-PARTY-LICENSES folder and inventory for
   that target's dependency graph.
3. **Notes.** `npx changelogithub@15.0.5 --output` builds the notes from
   Conventional Commits since the previous tag. `changelogithub.config.json`
   shows Features, Bug Fixes, Performance, Documentation and Breaking Changes.
   `ci:`, `chore:`, `test:`, `refactor:` and untyped commits are left out.
4. **Assemble.** `package_release.py finalize` writes `SHA256SUMS` and a
   release-wide `BUILD-MANIFEST.json`, then checks the sums with
   `sha256sum --check`.
5. **Publish** (tag pushes only, `release` environment). Creates a Sigstore
   build-provenance attestation for every archive (`gh attestation verify
   <archive> --repo lord007tn/rilldisk`). Then `gh release create --verify-tag`
   uploads every asset to a draft before publishing, so an updater never sees
   a release without `SHA256SUMS`.

Manual `workflow_dispatch` runs are dry runs. They do everything except
publish, and the archives, sums and notes stay as workflow artifacts for seven
days. Actions are pinned to commit SHAs. The workflow defaults to
`contents: read`, and only the publish job gets `contents: write`,
`id-token: write` and `attestations: write`. Release builds restore no caches,
so pull-request runs cannot poison them. No Docker.

Archive names are the contract between packaging and the updater:
`rilldisk-<tag>-<target>.zip` on Windows (flat) and
`rilldisk-<tag>-<target>.tar.gz` elsewhere (one top-level folder).

### Cutting a release

1. Set `version` in `[workspace.package]` and the path-dependency `version =`
   requirements to the new version (for example `0.3.0` or `0.3.0-alpha.1`),
   run `cargo check` to refresh `Cargo.lock`, and merge to `main` as
   `chore(release): v0.3.0`.
2. `git tag v0.3.0 <main sha> && git push origin v0.3.0`.
3. Optionally rehearse first with **Actions → Release → Run workflow** on any
   branch.

## Update behaviour

### What leaves the machine

The only requests are anonymous HTTPS `GET`s:

| Request | When |
| --- | --- |
| `https://api.github.com/repos/lord007tn/rilldisk/releases?per_page=30` | every check |
| `https://github.com/lord007tn/rilldisk/releases/download/<tag>/SHA256SUMS` | install only |
| `https://github.com/lord007tn/rilldisk/releases/download/<tag>/rilldisk-<tag>-<target>.<ext>` | install only |

Headers are `User-Agent: rilldisk-update` and `Accept`. They carry no version,
OS, machine ID, cookie or token. GitHub sees the IP address and time like any
HTTPS request, and on install it can infer the platform from the archive name.
Requests must start at `https://api.github.com/` or `https://github.com/`.
GitHub's HTTPS redirects to its download CDN are followed, at most five times.
Responses are size-capped: 8 MiB for the API, 1 MiB for checksums, 1 GiB for
archives.

### When it happens

* **CLI:** only `rilldisk update --check` and `rilldisk update`. `scan`,
  `reclaim`, `drives` and `mcp` never touch the network.
* **MCP:** no update tool, and the server never checks.
* **Desktop:** at most one automatic check per 24 hours, on startup. It is on
  by default and can be turned off with an in-app preference stored in
  `update.json`. Manual "Check for updates" works anytime. A result is only
  shown, never installed, without a click.
* **`RILLDISK_NO_UPDATE_CHECK=1`** (any value except empty, `0`, `false` or
  `no`) disables every request: the CLI exits 11, the desktop skips automatic
  checks and manual checks report that they are disabled.

The preference file is `%LOCALAPPDATA%\RillDisk\update.json` on Windows,
`~/Library/Application Support/RillDisk/update.json` on macOS and
`$XDG_STATE_HOME/rilldisk/update.json` (default
`~/.local/state/rilldisk/update.json`) on Linux. It holds `automatic_checks`,
`channel` and `last_check_unix`, and nothing else.

### Channels

`stable` follows releases without a suffix. `prerelease` follows every
published release (alpha, beta, rc). The channel comes from the first of these
that is set:

1. `--channel`, or the desktop preference;
2. `RILLDISK_UPDATE_CHANNEL`;
3. the running version: pre-release builds follow `prerelease`, stable builds
   follow `stable`.

Drafts, tags that are not `v<semver>`, releases without `SHA256SUMS` and
releases without an archive for this target are ignored. The updater never
downgrades.

### Verification

* HTTPS only, with certificate validation by the OS trust store on Windows and
  macOS and Mozilla's roots on Linux.
* The archive must match the SHA-256 listed for its exact file name in that
  release's `SHA256SUMS`. If it does not, nothing on disk changes.
* Binaries are taken from the archive into memory by file name only, so a
  path-traversal entry cannot write anywhere.

**Limits:** `SHA256SUMS` comes from the same release as the archive. It
protects against corrupted, truncated or CDN-altered downloads, but not against
someone who controls the GitHub repository and replaces both files. Build
provenance attestations let anyone check an archive offline. The next step is
to sign `SHA256SUMS` with minisign in CI, using an offline-generated key stored
as a release-environment secret, and have the updater verify it with an
embedded public key (`minisign-verify`, no dependencies). Authenticode signing
and Apple notarization follow once certificates exist.

### Installing

* **Consent.** The CLI shows the versions, the release notes URL, the archive
  name and every file it will replace, then asks `Continue? [y/N]`.
  Non-interactive runs refuse unless `--yes` is given. The desktop installs
  only after the user clicks.
* **Files replaced.** The running executable, plus `rilldisk` or
  `rilldisk-desktop` when present in the same folder. If the archive lacks a
  needed binary, nothing changes. A renamed executable is not updated.
* **Swap.** Each new file is written as `<name>.new` next to its target. The
  old one is renamed to `<name>.old` (Windows allows renaming a running
  executable) and the new one is renamed into place. Any failure rolls back
  completed renames. Leftover `.old` and `.new` files are removed on the next
  update check.
* **Restart.** The desktop relaunches its own new executable with the same
  arguments and closes its window. That is the only process RillDisk starts.
  It never invokes a shell or an installer.
* **Package-managed installs.** Paths under Scoop, winget, Homebrew, Nix, Snap,
  `/usr/bin` and friends, `~/.cargo/bin` or a source `target/` folder are never
  modified. The user gets the right command instead (`scoop update rilldisk`,
  `brew upgrade rilldisk`, ...). Packagers can set `RILLDISK_PACKAGE_MANAGER`
  (and optionally `RILLDISK_UPDATE_COMMAND`) at build time to switch
  self-update off.
* **No elevation.** If the folder is not writable (for example
  `Program Files`), the update fails with a message and changes nothing.
* **Notices.** Only the executables are replaced. LICENSE and the third-party
  notices beside them stay as originally downloaded, and each version's
  notices ship in its release archive. Refreshing them in place is a
  follow-up.

### CLI

```text
rilldisk update --check [--channel stable|prerelease] [--format table|json]
rilldisk update [--yes] [--channel stable|prerelease]
```

| Exit | Meaning |
| --- | --- |
| 0 | Up to date, or the update was installed |
| 10 | `--check` found a newer release |
| 11 | Updates unavailable here: opt-out env var, package-managed install, or no prebuilt archive for this platform |
| 1 | Error (network, verification, permissions, declined prompt) |

### Desktop API

`crates/rilldisk-desktop/src/update.rs` provides these calls, all running on
background threads:

* `check(ctx, Trigger::Automatic | Trigger::Manual) -> Pending<Result<Option<Release>, String>>`
* `install_and_restart(ctx, release) -> Install`, which exposes `progress()`
  and `poll()`.
* `automatic_checks()`, `set_automatic_checks(bool)` and
  `set_channel(Option<Channel>)`.

`Release` has `version`, `notes_url` and `update_command`. `update_command` is
`Some` for package-managed installs; show it instead of an install button.

## Not included

Silent or background installs, delta updates, rollback to older versions, an
update tool in MCP, elevation, analytics of any kind, and installer packages.
Each would need its own proposal.

## Tests

`crates/rilldisk-update/tests/update.rs` and the unit tests use a synthetic
GitHub response (`tests/fixtures/releases.json`) and archives built in-process.
No test touches the network. They cover:

* channel selection, semver ordering and no downgrades;
* skipping drafts, non-semver tags, releases without checksums and releases
  without this target;
* `SHA256SUMS` parsing;
* checksum mismatch, a missing sibling binary and path-traversal entries;
* zip and tar.gz swaps, including the executable bit;
* `.old` cleanup;
* package-manager path detection;
* the daily throttle and the env opt-out parsing.
