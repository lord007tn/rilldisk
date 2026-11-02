# Proposal: automated releases and opt-in updates

## In-app installer extension, November 2, 2026

The owner requested T3 Code's visible bottom update control and an in-app
download/progress/install flow. This extends the older restrictions below only
for RillDisk's own Windows MSI installation. Other package managers retain their
existing ownership and manual update instructions. MCP remains read-only.

After an explicit **Update and restart** click, the native updater may download
the matching MSI from this repository's selected release, verify SHA-256 against
the release feed/checksum asset, and stage it in per-user RillDisk update storage.
An unelevated copy of this application's executable acts as a temporary helper.
It verifies the staged package again, invokes only the Windows system
`msiexec.exe` with fixed install/progress/no-reboot options via the OS `runas`
consent flow, waits for completion, records the real result and relaunches the
desktop with its original arguments. No shell or downloaded script runs. The
helper keeps the package read-locked through installer launch. Consent denial
leaves the existing window open; installer failure relaunches the existing app
with an error. A reboot-required result is shown explicitly. Only updater-owned
staging/receipt files may be created or cleaned up by this flow.

The app must not restart during a scan or cleanup. Download progress is based
on received bytes; verification and installer handoff are named phases rather
than invented percentages. Windows Installer shows installation progress after
the application closes. The helper stays at the application's original privilege
level; only the installer requests elevation. Existing records, credentials,
scan accounting and MCP capabilities are unchanged.

Update availability/progress/error/retry controls live in a persistent bottom
bar, including the welcome screen. Automatic checks remain opt-out and throttled
to once a day, also while the app stays open; manual checks remain available
without entering Settings. No update installs automatically.

Reference: [T3 Code updating](https://github.com/pingdotgg/t3code/blob/main/docs/user/updating.md)
describes a bottom-sidebar update button that expands with progress and restart
controls. RillDisk uses its native Rust UI and Windows Installer rather than an
Electron runtime. Microsoft documents the fixed
[installer options](https://learn.microsoft.com/en-us/windows/win32/msi/standard-installer-command-line-options)
and [ShellExecuteExW process handoff](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecuteexw).
This extension supersedes the earlier "never runs an installer" and "no elevation"
statements only for this explicit MSI update action.

Status: implemented, November 1, 2026, in the CLI (`rilldisk update`) and the desktop
(update card with Install and restart; daily check toggled from the sidebar).

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
2. **Build.** One job per target, each on a runner of that architecture so
   every build is tested where it runs:

   | target | runner | ships |
   | --- | --- | --- |
   | `x86_64-pc-windows-msvc` | windows-2025 | .zip, .msi |
   | `aarch64-pc-windows-msvc` | windows-11-arm | .zip, .msi |
   | `x86_64-unknown-linux-gnu` | ubuntu-24.04 | .tar.gz, .deb, .rpm, .AppImage |
   | `aarch64-unknown-linux-gnu` | ubuntu-24.04-arm | .tar.gz, .deb, .rpm, .AppImage |
   | `x86_64-unknown-linux-musl` | ubuntu-24.04 | .tar.gz (CLI only, static) |
   | `aarch64-unknown-linux-musl` | ubuntu-24.04-arm | .tar.gz (CLI only, static) |
   | `aarch64-apple-darwin` | macos-15 | .tar.gz, .dmg |
   | `x86_64-apple-darwin` | macos-15-intel | .tar.gz, .dmg |

   Linux builds link with zig (`cargo zigbuild`, pinned) against glibc 2.28,
   so one binary runs on Debian 10+, Ubuntu 20.04+ and RHEL 8+; a step fails
   the build if any symbol needs a newer glibc. The desktop loads its window
   and GPU libraries at run time, so the programs link nothing but glibc.
   Every target runs a smoke test: `--version`, the update opt-out (exit 11)
   and the stdio MCP acceptance test. `scripts/package_release.py package`
   builds the archive. Each archive holds the programs, LICENSE, README.md,
   START-HERE.md, BUILD-MANIFEST.json, the Rust toolchain notices and the
   THIRD-PARTY-LICENSES folder and inventory for that target's dependency
   graph. `scripts/package_installers.py` then builds the installers from the
   archive's own files and the job installs each one: the MSI is installed,
   run, checked for its Start menu entry and PATH entry, then uninstalled and
   checked for leftovers; the .deb is installed with apt, its desktop entry and
   AppStream data validated, then removed; the .rpm's metadata is listed; the
   AppImage is started; the .dmg is mounted, its signature verified and the
   app run from it. Each installed copy must report itself as package-managed.
3. **Notes.** `npx changelogithub@15.0.5 --output` builds the notes from
   Conventional Commits since the previous tag. `changelogithub.config.json`
   shows Features, Bug Fixes, Performance, Documentation and Breaking Changes.
   `ci:`, `chore:`, `test:`, `refactor:` and untyped commits are left out.
4. **Assemble.** `package_release.py finalize` writes `SHA256SUMS`, a
   release-wide `BUILD-MANIFEST.json` and the `latest.yml` update feed, then
   checks the sums with `sha256sum --check`.
5. **Publish** (tag pushes only, `release` environment). Creates a Sigstore
   build-provenance attestation for every archive and installer
   (`gh attestation verify <file> --repo lord007tn/rilldisk`). Then `gh release create --verify-tag`
   uploads every asset to a draft before publishing, so an updater never sees
   a release without `SHA256SUMS` or `latest.yml`.

### The update feed

Every release carries `latest.yml`, in the spirit of electron-updater's feed:

```yaml
# RillDisk update feed: read by `rilldisk update` and the desktop app.
version: 0.1.1
tag: v0.1.1
prerelease: false
releaseDate: '2026-11-03T18:00:00Z'
notes: https://github.com/lord007tn/rilldisk/releases/tag/v0.1.1
files:
  - target: x86_64-pc-windows-msvc
    kind: archive
    name: rilldisk-v0.1.1-x86_64-pc-windows-msvc.zip
    url: https://github.com/lord007tn/rilldisk/releases/download/v0.1.1/rilldisk-v0.1.1-x86_64-pc-windows-msvc.zip
    sha256: <64 hex digits>
    size: 8898788
  - target: x86_64-pc-windows-msvc
    kind: msi
    name: RillDisk-0.1.1-x64.msi
    url: https://github.com/lord007tn/rilldisk/releases/download/v0.1.1/RillDisk-0.1.1-x64.msi
    sha256: <64 hex digits>
    size: 8401720
  # ... one archive per target, plus each installer (msi, deb, rpm, appimage, dmg)
```

`kind` is `archive` (what `rilldisk update` installs) or the installer type a
package-installed copy is pointed at. Feeds written before 0.1.1 have no
`kind`; their entries are archives. Older clients ignore `kind` and still find
their archive, so adding installers to the feed changes nothing for them.

The stable channel reads it through GitHub's fixed link to the newest stable
release, `https://github.com/lord007tn/rilldisk/releases/latest/download/latest.yml`:
one small request with no API rate limit, and the checksum arrives with the
release description. If that fails (for example a release without a feed), the
updater falls back to the releases API and `SHA256SUMS`. Pre-releases always use
the API, because GitHub's "latest" link never points at a pre-release. A feed is
rejected unless its tag matches its version, every URL is this repository's
download link for that tag and file, and every checksum is 64 hex digits.

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
   requirements to the new version (for example `0.1.1`, or `0.2.0-beta.1` for
   a pre-release), run `cargo check` to refresh `Cargo.lock`, and merge to
   `main` as `chore(release): v0.1.1`. Until 1.0, fixes and small features ship
   as `0.1.x`; the minor version moves only for a substantial change.
2. `git tag v0.1.1 <main sha> && git push origin v0.1.1`.
3. Optionally rehearse first with **Actions → Release → Run workflow** on any
   branch.

## Update behaviour

### What leaves the machine

The only requests are anonymous HTTPS `GET`s:

| Request | When |
| --- | --- |
| `https://github.com/lord007tn/rilldisk/releases/latest/download/latest.yml` | every stable check |
| `https://api.github.com/repos/lord007tn/rilldisk/releases?per_page=100&page=N` | pre-release checks, and stable checks when the feed is unavailable (at most 5 pages) |
| `https://github.com/lord007tn/rilldisk/releases/download/<tag>/SHA256SUMS` | install only |
| `https://github.com/lord007tn/rilldisk/releases/download/<tag>/rilldisk-<tag>-<target>.<ext>` | install only |

Headers are `User-Agent: rilldisk-update` and `Accept`. They carry no version,
OS, machine ID, cookie or token. GitHub sees the IP address and time like any
HTTPS request, and on install it can infer the platform from the archive name.
Requests must start at `https://api.github.com/` or `https://github.com/`.
GitHub's HTTPS redirects to its download CDN are followed, at most five times.
Responses are size-capped: 64 KiB for the feed, 8 MiB per API page, 1 MiB for
checksums, 1 GiB for archives.

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
  release's `latest.yml` (or, when read from the API, its `SHA256SUMS`). If it
  does not, nothing on disk changes.
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
* **RillDisk's own packages.** The MSI, .deb, .rpm and AppImage install a
  one-word `installed-by` marker (next to the programs for the MSI, in
  `usr/share/rilldisk/` otherwise); `RillDisk.app` is recognized by its
  bundle path. Such copies are never modified either. The CLI prints and the
  desktop offers a **Download** link to the same kind of package for the same
  target from the new release (listed in `latest.yml` with `kind:` and
  `target:`), plus the command that installs it. Opening that link is the
  user's choice; RillDisk never downloads or runs an installer itself.
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

Silent or background installs, running an installer from the updater, delta
updates (the AppImage's `.zsync` file serves external tools such as
AppImageUpdate, not RillDisk), rollback to older versions, an update tool in
MCP, elevation, and analytics of any kind. Each would need its own proposal.

## Installer packages

Scope of the packages added in 0.1.1, all built and tested in the release
workflow:

* **MSI** (WiX Toolset 5, pinned). Per-machine install to
  `Program Files\RillDisk`: Start menu entry, the CLI on the system PATH, an
  Apps & features entry with icon and links, `installed-by`. Installing asks
  for administrator approval through Windows Installer, as every per-machine
  installer does; RillDisk itself still never elevates. A newer MSI upgrades
  in place (same UpgradeCode), an older one refuses to downgrade, and
  uninstalling removes every file, the shortcut and the PATH entry. The
  portable .zip stays available for installs without admin rights.
* **.deb / .rpm.** `/usr/bin/rilldisk` and `/usr/bin/rilldisk-desktop`, the
  desktop entry, hicolor icons, AppStream metadata, licenses and notices. The
  only hard dependency is glibc; the window and GPU libraries are recommended
  (weak) dependencies because the CLI needs none of them. No maintainer
  scripts run at install time.
* **AppImage** (appimagetool and the type 2 runtime, pinned by SHA-256). The
  desktop app and the CLI in one file, with zsync update information for
  external tools.
* **.dmg.** `RillDisk.app` (ad-hoc signed, not notarized) with the CLI inside
  at `Contents/MacOS/rilldisk`, an Applications link and a note on adding the
  CLI to PATH. The Info.plist carries usage descriptions for the folders
  macOS protects, stating that RillDisk reads names and sizes only.

Windows executables also embed an icon, version information and a manifest
(`asInvoker`, per-monitor DPI awareness, long paths).

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
* package-manager path detection and `installed-by` markers;
* package entries in `latest.yml` (`kind`, `target`) and old feeds without them;
* the daily throttle and the env opt-out parsing.
