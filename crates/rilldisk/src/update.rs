// SPDX-License-Identifier: AGPL-3.0-only
//! `rilldisk update`: explicit, user-confirmed updates from GitHub Releases.
//! The CLI never checks on its own; see docs/proposals/updates.md.

use anyhow::{Context, Result, bail};
use rilldisk_update::{
    Channel, Check, DISABLE_ENV, InstallSource, Plan, archive_name, cleanup_previous_update,
    detect_install, disabled_by_env, target,
};
use serde_json::json;
use std::{
    io::{self, BufRead, IsTerminal, Write},
    process::ExitCode,
};

/// Up to date, or the update was installed.
const UP_TO_DATE: u8 = 0;
/// `--check` found a newer release.
const UPDATE_AVAILABLE: u8 = 10;
/// Updates are off here: opt-out env var, package-manager install or no
/// prebuilt archive for this platform.
const UNAVAILABLE: u8 = 11;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ChannelArg {
    Stable,
    Prerelease,
}

pub struct Args {
    pub check: bool,
    pub yes: bool,
    pub channel: Option<ChannelArg>,
    pub json: bool,
}

pub async fn run(args: Args) -> Result<ExitCode> {
    tokio::task::spawn_blocking(move || run_blocking(args)).await?
}

fn unavailable(args: &Args, reason: &str, command: Option<&str>) -> Result<ExitCode> {
    if args.json {
        println!(
            "{}",
            json!({
                "current": rilldisk_update::current_version().to_string(),
                "update_available": null,
                "unavailable": reason,
                "update_command": command,
            })
        );
    } else {
        eprintln!("{reason}");
        if let Some(command) = command {
            eprintln!("Update with: {command}");
        }
    }
    Ok(ExitCode::from(UNAVAILABLE))
}

fn run_blocking(args: Args) -> Result<ExitCode> {
    if args.json && !args.check {
        anyhow::bail!("--format json is only available with --check");
    }
    if disabled_by_env() {
        return unavailable(
            &args,
            &format!("Update checks are disabled by {DISABLE_ENV}."),
            None,
        );
    }
    let Some(target) = target() else {
        return unavailable(
            &args,
            "No prebuilt RillDisk release exists for this platform; build from source.",
            None,
        );
    };
    let plan = Plan::for_current_exe();
    let source = std::env::current_exe()
        .map(|exe| detect_install(&exe.canonicalize().unwrap_or(exe)))
        .unwrap_or(InstallSource::Standalone);
    // `--check` changes nothing: leftovers of an interrupted swap may be the
    // only copies of the old or new binaries.
    if let (false, Ok(plan), InstallSource::Standalone) = (args.check, &plan, &source) {
        cleanup_previous_update(&plan.dir);
    }
    let current = rilldisk_update::current_version();
    let channel = Channel::resolve(
        args.channel.map(|c| match c {
            ChannelArg::Stable => Channel::Stable,
            ChannelArg::Prerelease => Channel::Prerelease,
        }),
        &current,
    )?;
    let check = rilldisk_update::check(channel)?;
    let managed = match source {
        InstallSource::Managed { manager, command } => Some((manager, command)),
        InstallSource::Standalone => None,
    };

    if args.check {
        let managed_update = managed.is_some() && check.update.is_some();
        report(&check, managed, args.json)?;
        // Same codes as `rilldisk update`: a managed copy cannot update itself.
        return Ok(ExitCode::from(if managed_update {
            UNAVAILABLE
        } else if check.update.is_some() {
            UPDATE_AVAILABLE
        } else {
            UP_TO_DATE
        }));
    }

    let Some(release) = check.update else {
        println!(
            "RillDisk {} is up to date on the {} channel.",
            check.current, check.channel
        );
        return Ok(ExitCode::from(UP_TO_DATE));
    };
    if let Some((manager, command)) = managed {
        return unavailable(
            &args,
            &format!(
                "RillDisk {} is available, but this copy is managed by {manager}; it will not replace itself.",
                release.version
            ),
            Some(command),
        );
    }
    let plan = plan?;
    let archive = archive_name(&release.tag, target);
    eprintln!(
        "Update RillDisk {} -> {} ({} channel)",
        check.current, release.version, check.channel
    );
    eprintln!("Release notes: {}", release.notes_url);
    eprintln!("Download: {archive} (verified against SHA256SUMS before use)");
    eprintln!("Will replace:");
    for path in &plan.binaries {
        eprintln!("  {}", path.display());
    }
    if !args.yes && !confirm()? {
        eprintln!("Update cancelled; nothing was changed.");
        return Ok(ExitCode::from(1));
    }
    let interactive = io::stderr().is_terminal();
    let mut last = 0;
    let installed = rilldisk_update::install(&release, &plan, &mut |done, total| {
        if interactive && (done - last >= 1 << 20 || Some(done) == total) {
            last = done;
            let total = total.map_or_else(|| "?".to_owned(), super::human);
            eprint!("\rDownloading {} / {total}   ", super::human(done));
        }
    })
    .context("update failed; the existing binaries were left in place")?;
    if interactive {
        eprintln!();
    }
    println!("Updated RillDisk to {}.", installed.version);
    if plan.binaries.len() > 1 {
        println!("Restart rilldisk-desktop if it is running to use the new version.");
    }
    Ok(ExitCode::from(UP_TO_DATE))
}

fn report(check: &Check, managed: Option<(&str, &str)>, json: bool) -> Result<()> {
    let latest = check.update.as_ref();
    if json {
        println!(
            "{}",
            json!({
                "current": check.current.to_string(),
                "channel": check.channel,
                "update_available": latest.is_some(),
                "latest": latest.map(|r| r.version.to_string()),
                "tag": latest.map(|r| r.tag.clone()),
                "notes_url": latest.map(|r| r.notes_url.clone()),
                "managed_by": managed.map(|(m, _)| m),
                "update_command": latest.map(|_| managed.map_or("rilldisk update", |(_, c)| c)),
            })
        );
        return Ok(());
    }
    println!("Current: {} ({} channel)", check.current, check.channel);
    match latest {
        None => println!("Latest:  {} (up to date)", check.current),
        Some(release) => {
            println!("Latest:  {}", release.version);
            println!("Notes:   {}", release.notes_url);
            let command = managed.map_or("rilldisk update", |(_, c)| c);
            println!("Update with: {command}");
        }
    }
    Ok(())
}

fn confirm() -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("refusing to update without confirmation; re-run with --yes");
    }
    eprint!("Continue? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES"))
}
