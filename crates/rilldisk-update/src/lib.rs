// SPDX-License-Identifier: AGPL-3.0-only
//! Opt-in update checks and verified self-replacement for RillDisk binaries.
//!
//! This is the only RillDisk crate that opens network connections. The scope
//! is fixed by `docs/proposals/updates.md`:
//!
//! * the only requests are an anonymous `GET` of the public GitHub releases
//!   list and downloads of that release's `SHA256SUMS` and archive;
//! * nothing about the machine, its files or its scans is sent;
//! * an archive is used only after its SHA-256 matches `SHA256SUMS`;
//! * binaries are replaced only when the caller asks (after user consent);
//! * `RILLDISK_NO_UPDATE_CHECK=1` turns every network path here into an error;
//! * package-manager installs are never modified.
//!
//! Pure logic (version selection, checksum parsing, archive extraction, file
//! swapping) is public and tested without network access.

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fmt, str::FromStr};

mod archive;
mod install;
mod net;
mod state;

pub use archive::extract_binaries;
pub use install::{
    INSTALL_MARKER, InstallSource, Installed, Package, Plan, apply_archive,
    cleanup_previous_update, detect_install, install,
};
pub use net::{check, fetch_feed, fetch_releases};
pub use state::{CHECK_INTERVAL_SECS, State, state_path};

/// GitHub `owner/name` that publishes RillDisk releases.
pub const REPOSITORY: &str = "lord007tn/rilldisk";
/// Set to `1` (or any value except `0`/`false`) to disable every update request.
pub const DISABLE_ENV: &str = "RILLDISK_NO_UPDATE_CHECK";
/// `stable` or `prerelease`; overrides the default channel.
pub const CHANNEL_ENV: &str = "RILLDISK_UPDATE_CHANNEL";
/// Release asset listing `sha256  file` for every archive in the release.
pub const CHECKSUMS_ASSET: &str = "SHA256SUMS";
/// Release asset describing the release for updaters (see [`parse_feed`]).
pub const FEED_ASSET: &str = "latest.yml";
/// Executables shipped in every archive, without the platform suffix.
pub const BINARIES: [&str; 2] = ["rilldisk", "rilldisk-desktop"];

/// Version of the running binaries. All workspace crates share one version.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("workspace version is valid semver")
}

/// Which releases an installation follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Releases without a pre-release suffix.
    Stable,
    /// Every published release, including `-alpha.N`, `-beta.N` and `-rc.N`.
    Prerelease,
}

impl Channel {
    /// Pre-release builds follow pre-releases; stable builds stay stable.
    pub fn for_version(version: &Version) -> Self {
        if version.pre.is_empty() {
            Self::Stable
        } else {
            Self::Prerelease
        }
    }

    /// Explicit choice, then `RILLDISK_UPDATE_CHANNEL`, then [`Channel::for_version`].
    pub fn resolve(explicit: Option<Self>, current: &Version) -> Result<Self> {
        if let Some(channel) = explicit {
            return Ok(channel);
        }
        match std::env::var(CHANNEL_ENV) {
            Ok(value) if !value.trim().is_empty() => value
                .parse()
                .with_context(|| format!("invalid {CHANNEL_ENV}")),
            _ => Ok(Self::for_version(current)),
        }
    }
}

impl FromStr for Channel {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Ok(Self::Stable),
            "prerelease" | "pre-release" | "pre" => Ok(Self::Prerelease),
            other => bail!("unknown update channel {other:?}; use stable or prerelease"),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        })
    }
}

/// True when `RILLDISK_NO_UPDATE_CHECK` disables all update network access.
pub fn disabled_by_env() -> bool {
    env_flag(std::env::var(DISABLE_ENV).ok().as_deref())
}

fn env_flag(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        !v.is_empty()
            && v != "0"
            && !v.eq_ignore_ascii_case("false")
            && !v.eq_ignore_ascii_case("no")
    })
}

/// A downloadable file attached to a release.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    pub size: u64,
}

/// A published GitHub release whose tag is `v<semver>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    pub prerelease: bool,
    /// Human-readable release page with the changelog.
    pub notes_url: String,
    pub assets: Vec<Asset>,
    /// Archive name -> SHA-256 from the release's update feed. Empty when the
    /// release was read from the API; `SHA256SUMS` is used then.
    pub checksums: std::collections::BTreeMap<String, String>,
    /// Installers and system packages listed in the feed (MSI, .deb, .rpm,
    /// AppImage, .dmg). Empty when the release was read from the API.
    pub packages: Vec<PackageFile>,
}

/// An installer or system package attached to a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageFile {
    /// `msi`, `deb`, `rpm`, `appimage` or `dmg`.
    pub kind: String,
    /// Rust target triple the package was built from.
    pub target: String,
    /// Asset name; the download is `Release::asset(name)`.
    pub name: String,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The installer of `kind` (`msi`, `deb`, ...) built for `target`, if the
    /// release's feed lists one.
    pub fn package_asset(&self, kind: &str, target: &str) -> Option<&Asset> {
        self.packages
            .iter()
            .find(|p| p.kind == kind && p.target == target)
            .and_then(|p| self.asset(&p.name))
    }

    /// True when this release ships an archive for `target` plus checksums.
    pub fn installable_for(&self, target: &str) -> bool {
        let name = archive_name(&self.tag, target);
        self.asset(&name).is_some()
            && (self.checksums.contains_key(&name) || self.asset(CHECKSUMS_ASSET).is_some())
    }
}

/// The `latest.yml` update feed published with every release:
///
/// ```yaml
/// version: 0.1.0
/// tag: v0.1.0
/// prerelease: false
/// releaseDate: '2026-11-01T18:00:00Z'
/// notes: https://github.com/lord007tn/rilldisk/releases/tag/v0.1.0
/// files:
///   - target: x86_64-pc-windows-msvc
///     kind: archive
///     name: rilldisk-v0.1.0-x86_64-pc-windows-msvc.zip
///     url: https://github.com/lord007tn/rilldisk/releases/download/v0.1.0/rilldisk-v0.1.0-x86_64-pc-windows-msvc.zip
///     sha256: <64 hex digits>
///     size: 8898788
///   - target: x86_64-pc-windows-msvc
///     kind: msi
///     name: RillDisk-0.1.0-x64.msi
///     ...
/// ```
///
/// `kind` defaults to `archive` (feeds before 0.1.1 had no `kind`). Archives
/// are what [`install`] uses; other kinds are offered as downloads to copies
/// that a package installed.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Feed {
    version: String,
    tag: String,
    #[serde(default)]
    prerelease: bool,
    notes: String,
    files: Vec<FeedFile>,
}

#[derive(Debug, Deserialize)]
struct FeedFile {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    name: String,
    url: String,
    sha256: String,
    size: u64,
}

/// Parse a `latest.yml` feed into a release. Files must be downloads of this
/// repository's release with the same tag, and every checksum must be 64 hex
/// digits; anything else is rejected rather than trusted.
pub fn parse_feed(yaml: &[u8]) -> Result<Release> {
    let feed: Feed =
        serde_yaml_ng::from_slice(yaml).context("unreadable latest.yml update feed")?;
    let version = Version::parse(&feed.version).context("latest.yml has an invalid version")?;
    if feed.tag != format!("v{version}") {
        anyhow::bail!(
            "latest.yml tag {} does not match version {version}",
            feed.tag
        );
    }
    let base = format!(
        "https://github.com/{REPOSITORY}/releases/download/{}/",
        feed.tag
    );
    let mut assets = Vec::new();
    let mut checksums = std::collections::BTreeMap::new();
    let mut packages = Vec::new();
    for file in feed.files {
        let sha = file.sha256.to_ascii_lowercase();
        if file.url != format!("{base}{}", file.name)
            || sha.len() != 64
            || !sha.bytes().all(|b| b.is_ascii_hexdigit())
        {
            anyhow::bail!("latest.yml lists an unexpected file: {}", file.name);
        }
        checksums.insert(file.name.clone(), sha);
        match (file.kind.as_deref(), file.target) {
            (None | Some("archive"), _) => {}
            (Some(kind), Some(target)) => packages.push(PackageFile {
                kind: kind.to_owned(),
                target,
                name: file.name.clone(),
            }),
            (Some(_), None) => anyhow::bail!("latest.yml lists {} without a target", file.name),
        }
        assets.push(Asset {
            name: file.name,
            url: file.url,
            size: file.size,
        });
    }
    Ok(Release {
        prerelease: feed.prerelease || !version.pre.is_empty(),
        tag: feed.tag,
        version,
        notes_url: feed.notes,
        assets,
        checksums,
        packages,
    })
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// Parse the body of `GET /repos/{owner}/{repo}/releases`. Drafts and tags
/// that are not `v<semver>` are ignored.
pub fn parse_releases(json: &[u8]) -> Result<Vec<Release>> {
    parse_release_page(json).map(|(releases, _)| releases)
}

/// Like [`parse_releases`], also returning how many entries the page listed
/// (drafts and unparsable tags included), so callers can tell a short page.
pub fn parse_release_page(json: &[u8]) -> Result<(Vec<Release>, usize)> {
    let raw: Vec<ApiRelease> =
        serde_json::from_slice(json).context("unexpected GitHub releases response")?;
    let listed = raw.len();
    let releases = raw
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version = Version::parse(r.tag_name.strip_prefix('v')?).ok()?;
            Some(Release {
                prerelease: r.prerelease || !version.pre.is_empty(),
                tag: r.tag_name,
                version,
                notes_url: r.html_url,
                assets: r.assets,
                checksums: Default::default(),
                packages: Vec::new(),
            })
        })
        .collect();
    Ok((releases, listed))
}

/// The newest release on `channel` that is newer than `current` and ships an
/// archive for `target`. Never selects a downgrade.
pub fn select_update<'a>(
    releases: &'a [Release],
    current: &Version,
    channel: Channel,
    target: &str,
) -> Option<&'a Release> {
    releases
        .iter()
        .filter(|r| channel == Channel::Prerelease || !r.prerelease)
        .filter(|r| r.version > *current)
        .filter(|r| r.installable_for(target))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// Rust target triple of the running build when releases ship an archive for it.
pub const fn target() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64", target_env = "msvc")) {
        Some("x86_64-pc-windows-msvc")
    } else if cfg!(all(windows, target_arch = "aarch64", target_env = "msvc")) {
        Some("aarch64-pc-windows-msvc")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "aarch64",
        target_env = "gnu"
    )) {
        Some("aarch64-unknown-linux-gnu")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "musl"
    )) {
        Some("x86_64-unknown-linux-musl")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "aarch64",
        target_env = "musl"
    )) {
        Some("aarch64-unknown-linux-musl")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else {
        None
    }
}

/// Archive file name produced by `scripts/package_release.py`.
pub fn archive_name(tag: &str, target: &str) -> String {
    let ext = if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("rilldisk-{tag}-{target}.{ext}")
}

/// Parse `sha256sum` output (`<hex>  <name>` or `<hex> *<name>`) into
/// `name -> lowercase hex`. Malformed lines are skipped.
pub fn parse_sha256sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*').trim();
            (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty())
                .then(|| (name.to_owned(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Fail unless `bytes` hash to `expected` (hex, case-insensitive).
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        bail!("checksum mismatch: expected {expected}, downloaded file is {actual}")
    }
}

/// Outcome of an update check.
#[derive(Clone, Debug)]
pub struct Check {
    pub current: Version,
    pub channel: Channel,
    /// Newest installable release on the channel, if newer than `current`.
    pub update: Option<Release>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_flag_values() {
        assert!(!env_flag(None));
        assert!(!env_flag(Some("")));
        assert!(!env_flag(Some("0")));
        assert!(!env_flag(Some("false")));
        assert!(!env_flag(Some("No")));
        assert!(env_flag(Some("1")));
        assert!(env_flag(Some("yes")));
        assert!(env_flag(Some("TRUE")));
    }

    #[test]
    fn channel_parsing_and_defaults() {
        assert_eq!("stable".parse::<Channel>().unwrap(), Channel::Stable);
        assert_eq!(
            " Prerelease ".parse::<Channel>().unwrap(),
            Channel::Prerelease
        );
        assert!("nightly".parse::<Channel>().is_err());
        let alpha = Version::parse("0.3.0-alpha.1").unwrap();
        let stable = Version::parse("0.3.0").unwrap();
        assert_eq!(Channel::for_version(&alpha), Channel::Prerelease);
        assert_eq!(Channel::for_version(&stable), Channel::Stable);
        assert_eq!(
            Channel::resolve(Some(Channel::Stable), &alpha).unwrap(),
            Channel::Stable
        );
    }

    #[test]
    fn archive_names_match_packaging_script() {
        assert_eq!(
            archive_name("v0.3.0", "x86_64-pc-windows-msvc"),
            "rilldisk-v0.3.0-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            archive_name("v0.3.0-alpha.2", "aarch64-apple-darwin"),
            "rilldisk-v0.3.0-alpha.2-aarch64-apple-darwin.tar.gz"
        );
    }
}
