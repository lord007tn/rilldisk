// SPDX-License-Identifier: AGPL-3.0-only
//! Offline tests: synthetic GitHub responses and archives built in-process.

use rilldisk_update::{
    Channel, Package, Plan, Release, apply_archive, archive_name, parse_releases, parse_sha256sums,
    select_update, sha256_hex, verify_sha256,
};
use semver::Version;
use std::{fs, io::Write, path::Path};

const WINDOWS: &str = "x86_64-pc-windows-msvc";
const LINUX: &str = "x86_64-unknown-linux-gnu";

fn releases() -> Vec<Release> {
    parse_releases(include_bytes!("fixtures/releases.json")).unwrap()
}

fn pick(current: &str, channel: Channel, target: &str) -> Option<String> {
    let releases = releases();
    select_update(
        &releases,
        &Version::parse(current).unwrap(),
        channel,
        target,
    )
    .map(|r| r.tag.clone())
}

#[test]
fn parsing_skips_drafts_and_non_semver_tags() {
    let tags: Vec<String> = releases().into_iter().map(|r| r.tag).collect();
    assert_eq!(
        tags,
        [
            "v0.4.0-alpha.1",
            "v0.3.2",
            "v0.3.1",
            "v0.3.0",
            "v0.3.0-rc.1",
            "v0.2.0-alpha.1"
        ]
    );
    let alpha = &releases()[0];
    assert!(alpha.prerelease);
    assert_eq!(
        alpha.notes_url,
        "https://github.com/lord007tn/rilldisk/releases/tag/v0.4.0-alpha.1"
    );
}

#[test]
fn stable_channel_never_offers_prereleases() {
    assert_eq!(
        pick("0.2.0", Channel::Stable, LINUX).as_deref(),
        Some("v0.3.0")
    );
    assert_eq!(pick("0.3.0", Channel::Stable, LINUX), None);
}

#[test]
fn releases_without_checksums_or_this_target_are_skipped() {
    // v0.3.2 has no SHA256SUMS; v0.3.1 only ships Windows.
    assert_eq!(
        pick("0.2.0", Channel::Stable, WINDOWS).as_deref(),
        Some("v0.3.1")
    );
    assert_eq!(
        pick("0.2.0", Channel::Stable, LINUX).as_deref(),
        Some("v0.3.0")
    );
    assert_eq!(
        pick("0.2.0", Channel::Stable, "riscv64gc-unknown-linux-gnu"),
        None
    );
}

#[test]
fn prerelease_channel_takes_the_highest_version() {
    assert_eq!(
        pick("0.3.0-alpha.1", Channel::Prerelease, LINUX).as_deref(),
        Some("v0.4.0-alpha.1")
    );
    assert_eq!(
        pick("0.3.0-rc.1", Channel::Prerelease, WINDOWS).as_deref(),
        Some("v0.4.0-alpha.1")
    );
}

#[test]
fn semver_ordering_and_no_downgrades() {
    // 0.3.0-rc.1 < 0.3.0 and alpha.10 > alpha.9 per semver.
    assert_eq!(
        pick("0.3.0-rc.1", Channel::Stable, LINUX).as_deref(),
        Some("v0.3.0")
    );
    assert!(Version::parse("0.4.0-alpha.10").unwrap() > Version::parse("0.4.0-alpha.9").unwrap());
    assert_eq!(pick("0.4.0-alpha.1", Channel::Prerelease, LINUX), None);
    assert_eq!(pick("0.4.0", Channel::Prerelease, LINUX), None);
    assert_eq!(pick("1.0.0", Channel::Stable, WINDOWS), None);
}

#[test]
fn checksum_file_parsing() {
    let sums = parse_sha256sums(include_str!("fixtures/SHA256SUMS"));
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(sums.len(), 2);
    assert_eq!(
        sums["rilldisk-v0.3.0-x86_64-pc-windows-msvc.zip"].as_str(),
        abc
    );
    assert_eq!(
        sums["rilldisk-v0.3.0-aarch64-apple-darwin.tar.gz"].as_str(),
        abc,
        "binary-mode marker and uppercase hex are accepted"
    );
    assert_eq!(sha256_hex(b"abc"), abc);
    verify_sha256(b"abc", abc).unwrap();
    verify_sha256(b"abc", &abc.to_uppercase()).unwrap();
    let error = verify_sha256(b"abd", abc).unwrap_err().to_string();
    assert!(error.contains("checksum mismatch"), "{error}");
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn install_dir(with_desktop: bool) -> (tempfile::TempDir, Plan) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(exe("rilldisk")), b"old cli").unwrap();
    if with_desktop {
        fs::write(dir.path().join(exe("rilldisk-desktop")), b"old desktop").unwrap();
    }
    fs::write(dir.path().join("README.md"), b"keep me").unwrap();
    let plan = Plan::for_exe(&dir.path().join(exe("rilldisk"))).unwrap();
    (dir, plan)
}

fn zip_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn tar_gz_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, name, *data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn plan_covers_running_and_sibling_binaries() {
    let (dir, plan) = install_dir(true);
    assert_eq!(
        plan.binaries,
        [
            dir.path().join(exe("rilldisk")),
            dir.path().join(exe("rilldisk-desktop"))
        ]
    );
    let (_dir, plan) = install_dir(false);
    assert_eq!(plan.binaries.len(), 1);
    assert!(Plan::for_exe(&dir.path().join("renamed-tool.exe")).is_err());
}

#[test]
fn verified_zip_replaces_both_binaries() {
    let (dir, plan) = install_dir(true);
    let cli = exe("rilldisk");
    let gui = exe("rilldisk-desktop");
    let archive = zip_archive(&[
        ("README.md", b"new readme"),
        (&cli, b"new cli"),
        (&gui, b"new desktop"),
    ]);
    let name = archive_name("v9.9.9", WINDOWS);
    apply_archive(&name, &archive, &sha256_hex(&archive), &plan).unwrap();
    assert_eq!(fs::read(dir.path().join(&cli)).unwrap(), b"new cli");
    assert_eq!(fs::read(dir.path().join(&gui)).unwrap(), b"new desktop");
    assert_eq!(fs::read(dir.path().join("README.md")).unwrap(), b"keep me");
    let mut expected = vec!["README.md".to_owned(), cli, gui];
    expected.sort();
    assert_eq!(listing(dir.path()), expected, "no .new/.old leftovers");
}

#[test]
fn verified_tar_gz_with_top_level_folder() {
    let (dir, plan) = install_dir(false);
    let cli = exe("rilldisk");
    let path = format!("rilldisk-v9.9.9-{LINUX}/{cli}");
    let archive = tar_gz_archive(&[(&path, b"new cli")]);
    let name = archive_name("v9.9.9", LINUX);
    apply_archive(&name, &archive, &sha256_hex(&archive), &plan).unwrap();
    assert_eq!(fs::read(dir.path().join(&cli)).unwrap(), b"new cli");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir.path().join(&cli))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "replacement is executable");
    }
}

#[test]
fn checksum_mismatch_changes_nothing() {
    let (dir, plan) = install_dir(true);
    let before = listing(dir.path());
    let archive = zip_archive(&[
        (&exe("rilldisk"), b"evil"),
        (&exe("rilldisk-desktop"), b"evil"),
    ]);
    let wrong = sha256_hex(b"something else");
    let name = archive_name("v9.9.9", WINDOWS);
    let error = apply_archive(&name, &archive, &wrong, &plan).unwrap_err();
    assert!(error.to_string().contains("checksum mismatch"), "{error:#}");
    assert_eq!(
        fs::read(dir.path().join(exe("rilldisk"))).unwrap(),
        b"old cli"
    );
    assert_eq!(listing(dir.path()), before);
}

#[test]
fn archive_missing_a_sibling_changes_nothing() {
    let (dir, plan) = install_dir(true);
    let before = listing(dir.path());
    let archive = zip_archive(&[(&exe("rilldisk"), b"new cli")]);
    let name = archive_name("v9.9.9", WINDOWS);
    let error = apply_archive(&name, &archive, &sha256_hex(&archive), &plan).unwrap_err();
    assert!(error.to_string().contains("does not contain"), "{error:#}");
    assert_eq!(
        fs::read(dir.path().join(exe("rilldisk"))).unwrap(),
        b"old cli"
    );
    assert_eq!(
        fs::read(dir.path().join(exe("rilldisk-desktop"))).unwrap(),
        b"old desktop"
    );
    assert_eq!(listing(dir.path()), before);
}

#[test]
fn traversal_entries_are_matched_by_file_name_only() {
    let (dir, plan) = install_dir(false);
    let evil = format!("../../{}", exe("rilldisk"));
    let archive = zip_archive(&[(&evil, b"new cli")]);
    let name = archive_name("v9.9.9", WINDOWS);
    apply_archive(&name, &archive, &sha256_hex(&archive), &plan).unwrap();
    assert_eq!(
        fs::read(dir.path().join(exe("rilldisk"))).unwrap(),
        b"new cli"
    );
    assert!(!dir.path().parent().unwrap().join(exe("rilldisk")).exists());
}

#[test]
fn a_page_reports_how_many_entries_it_listed() {
    let json = include_bytes!("fixtures/releases.json");
    let (releases, listed) = rilldisk_update::parse_release_page(json).unwrap();
    assert!(listed >= releases.len());
    assert_eq!(releases.len(), parse_releases(json).unwrap().len());
}

#[test]
fn install_refuses_package_managed_binaries_before_downloading() {
    let release = releases().into_iter().next().unwrap();
    let plan = Plan {
        dir: std::path::PathBuf::from("/opt/homebrew/bin"),
        binaries: vec![std::path::PathBuf::from("/opt/homebrew/bin/rilldisk")],
    };
    let error = rilldisk_update::install(&release, &plan, &mut |_, _| {}).unwrap_err();
    assert!(error.to_string().contains("Homebrew"), "{error}");
}

#[test]
fn the_yaml_feed_describes_an_installable_release_with_checksums() {
    let release = rilldisk_update::parse_feed(include_bytes!("fixtures/latest.yml")).unwrap();
    assert_eq!(release.tag, "v0.1.0");
    assert_eq!(release.version, Version::new(0, 1, 0));
    assert!(!release.prerelease);
    assert!(release.installable_for(WINDOWS));
    assert!(release.installable_for(LINUX));
    assert!(!release.installable_for("aarch64-apple-darwin"));
    let linux = archive_name("v0.1.0", LINUX);
    // Checksums are normalized to lowercase hex.
    assert_eq!(
        release.checksums[&linux],
        "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f809"
    );
    let newer = select_update(
        std::slice::from_ref(&release),
        &Version::parse("0.0.9").unwrap(),
        Channel::Stable,
        WINDOWS,
    );
    assert_eq!(newer.map(|r| r.tag.as_str()), Some("v0.1.0"));
}

#[test]
fn the_feed_lists_installers_for_copies_a_package_installed() {
    let release =
        rilldisk_update::parse_feed(include_bytes!("fixtures/latest-packages.yml")).unwrap();
    // Archives stay self-updatable; installers are offered as downloads.
    assert!(release.installable_for(WINDOWS));
    let msi = release.package_asset("msi", WINDOWS).unwrap();
    assert_eq!(msi.name, "RillDisk-0.1.1-x64.msi");
    assert_eq!(
        release.checksums["RillDisk-0.1.1-x64.msi"],
        "2f343b0931126a20f133d67c2b018a3b5c3a3ca7ff2b4f2e0f8a3e1b8e2c1d3a"
    );
    let deb = release
        .package_asset(Package::Deb.kind(), "aarch64-unknown-linux-gnu")
        .unwrap();
    assert!(deb.url.ends_with("/v0.1.1/rilldisk_0.1.1_arm64.deb"));
    // No package of another kind or for another target is made up.
    assert!(release.package_asset("deb", LINUX).is_none());
    assert!(release.package_asset("rpm", WINDOWS).is_none());

    // A package entry must say which target it was built for.
    let feed = String::from_utf8(include_bytes!("fixtures/latest-packages.yml").to_vec())
        .unwrap()
        .replace("\r\n", "\n");
    let untargeted = feed.replacen(
        "  - target: aarch64-unknown-linux-gnu\n    kind: deb",
        "  - kind: deb",
        1,
    );
    assert_ne!(untargeted, feed);
    assert!(rilldisk_update::parse_feed(untargeted.as_bytes()).is_err());
}

#[test]
fn a_feed_pointing_elsewhere_or_with_bad_checksums_is_rejected() {
    let feed = String::from_utf8(include_bytes!("fixtures/latest.yml").to_vec()).unwrap();
    for (from, to) in [
        (
            "https://github.com/lord007tn/rilldisk/releases/download/v0.1.0/rilldisk-v0.1.0-x86_64-pc",
            "https://evil.example/rilldisk-v0.1.0-x86_64-pc",
        ),
        ("sha256: 0f343b", "sha256: zz343b"),
        ("tag: v0.1.0", "tag: v9.9.9"),
    ] {
        let bad = feed.replacen(from, to, 1);
        assert_ne!(bad, feed);
        assert!(rilldisk_update::parse_feed(bad.as_bytes()).is_err(), "{to}");
    }
}

#[test]
fn an_empty_plan_is_refused_instead_of_reporting_success() {
    let plan = Plan {
        dir: std::path::PathBuf::from("unused"),
        binaries: Vec::new(),
    };
    let bytes = b"anything";
    let error = apply_archive("x.zip", bytes, &sha256_hex(bytes), &plan).unwrap_err();
    assert!(
        error.to_string().contains("no RillDisk binaries"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_sibling_binary_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join("rilldisk");
    fs::write(&cli, b"cli").unwrap();
    let real = dir.path().join("elsewhere");
    fs::write(&real, b"desktop").unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("rilldisk-desktop")).unwrap();
    let error = Plan::for_exe(&cli).unwrap_err();
    assert!(error.to_string().contains("symbolic link"), "{error}");
}
