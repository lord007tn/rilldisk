// SPDX-License-Identifier: AGPL-3.0-only
//! The network edge: anonymous HTTPS GETs to GitHub, nothing else.

use crate::{
    Channel, Check, DISABLE_ENV, FEED_ASSET, REPOSITORY, Release, current_version, disabled_by_env,
    parse_feed, parse_release_page, select_update, target,
};
use anyhow::{Context, Result, bail};
use std::{io::Read, time::Duration};

/// Generic agent string: no version, platform or machine identifier.
const USER_AGENT: &str = "rilldisk-update";
const RELEASES_LIMIT: u64 = 8 << 20;
const FEED_LIMIT: u64 = 64 << 10;
/// Hosts a download may start from. GitHub redirects asset downloads to its
/// CDN over HTTPS; integrity comes from the SHA-256 check either way.
const ALLOWED_PREFIXES: [&str; 2] = ["https://api.github.com/", "https://github.com/"];

fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder();
    #[cfg(any(windows, target_os = "macos"))]
    let config = config.tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build(),
    );
    config
        .https_only(true)
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(300)))
        .max_redirects(5)
        .build()
        .into()
}

fn allowed_url(url: &str) -> bool {
    ALLOWED_PREFIXES.iter().any(|p| url.starts_with(p))
}

fn guard(url: &str) -> Result<()> {
    if disabled_by_env() {
        bail!("update checks are disabled by {DISABLE_ENV}");
    }
    if !allowed_url(url) {
        bail!("refusing to download from {url}: not a GitHub release URL");
    }
    Ok(())
}

/// Published (non-draft) releases, newest first, as GitHub lists them: every
/// page until a short one, up to `MAX_PAGES` pages of `PAGE_SIZE`.
pub fn fetch_releases() -> Result<Vec<Release>> {
    const PAGE_SIZE: usize = 100;
    const MAX_PAGES: usize = 5;
    let mut releases = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = format!(
            "https://api.github.com/repos/{REPOSITORY}/releases?per_page={PAGE_SIZE}&page={page}"
        );
        let bytes = download(&url, RELEASES_LIMIT, &mut |_, _| {})?;
        let (batch, listed) = parse_release_page(&bytes)?;
        releases.extend(batch);
        if listed < PAGE_SIZE {
            break;
        }
    }
    Ok(releases)
}

/// The newest stable release, read from its `latest.yml` feed through GitHub's
/// fixed "latest release" download link: one small request, no API rate limit.
pub fn fetch_feed() -> Result<Release> {
    let url = format!("https://github.com/{REPOSITORY}/releases/latest/download/{FEED_ASSET}");
    let bytes = download(&url, FEED_LIMIT, &mut |_, _| {})?;
    parse_feed(&bytes)
}

/// Ask GitHub for the newest release on `channel` newer than this build.
/// Stable reads the update feed and falls back to the releases API (for example
/// before any release ships a feed); pre-releases always use the API, because
/// GitHub's "latest" link never points at a pre-release.
pub fn check(channel: Channel) -> Result<Check> {
    let Some(target) = target() else {
        bail!("no prebuilt RillDisk release exists for this platform; build from source");
    };
    let releases = match channel {
        Channel::Stable => match fetch_feed() {
            Ok(release) if release.installable_for(target) => vec![release],
            _ => fetch_releases()?,
        },
        Channel::Prerelease => fetch_releases()?,
    };
    let current = current_version();
    let update = select_update(&releases, &current, channel, target).cloned();
    Ok(Check {
        current,
        channel,
        update,
    })
}

/// GET `url` into memory, refusing bodies over `limit` bytes. `progress`
/// receives `(received, total)` as the body streams in.
pub(crate) fn download(
    url: &str,
    limit: u64,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Vec<u8>> {
    guard(url)?;
    let response = agent()
        .get(url)
        .header(
            "Accept",
            "application/vnd.github+json, application/octet-stream",
        )
        .call()
        .with_context(|| format!("request to {url} failed"))?;
    let body = response.into_body();
    let total = body.content_length();
    if total.is_some_and(|t| t > limit) {
        bail!("{url} is larger than the {limit}-byte limit");
    }
    let mut reader = body.into_with_config().limit(limit).reader();
    let mut out = Vec::with_capacity(total.unwrap_or(0).min(limit) as usize);
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let n = reader
            .read(&mut chunk)
            .with_context(|| format!("download of {url} was interrupted"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        progress(out.len() as u64, total);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_github_urls_are_fetched() {
        assert!(allowed_url(
            "https://github.com/lord007tn/rilldisk/releases/download/v1/x.zip"
        ));
        assert!(allowed_url(
            "https://api.github.com/repos/lord007tn/rilldisk/releases"
        ));
        assert!(!allowed_url("http://github.com/x"));
        assert!(!allowed_url("https://example.com/rilldisk.zip"));
        assert!(!allowed_url("https://github.com.evil.example/x"));
    }
}
