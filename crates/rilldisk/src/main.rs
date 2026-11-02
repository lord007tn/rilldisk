// SPDX-License-Identifier: AGPL-3.0-only
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use rilldisk_core::{
    ReclaimReport, Safety, ScanControl, ScanOptions, Snapshot, Summary, find_reclaimable, scan,
    volumes,
};
use std::{
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    path::PathBuf,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

mod cleanup;
mod mcp;
mod setup;
mod update;

#[derive(Parser)]
#[command(
    version,
    about = "Disk intelligence for Windows, Linux and macOS. Local, metadata-only, native Rust.",
    after_help = "AGPL-3.0-only. Source: https://github.com/lord007tn/rilldisk"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct ScanArgs {
    path: PathBuf,
    /// Write to a NEW file. Existing files are never overwritten.
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
    threads: Option<u8>,
    #[arg(long, default_value_t = rilldisk_core::DEFAULT_MAX_ENTRIES)]
    max_entries: usize,
    /// Linux/macOS: also enter other mounted filesystems below the path. Virtual
    /// filesystems such as /proc, /sys, /dev and /run are skipped either way.
    #[arg(long)]
    cross_filesystems: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a directory or drive without reading file contents.
    Scan {
        #[command(flatten)]
        args: ScanArgs,
        #[arg(long, value_enum, default_value = "table")]
        format: Format,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=500))]
        top: u16,
    },
    /// Find space you can likely reclaim: worktrees, build output, dependency
    /// folders and caches. Only lists suggestions unless you pass --delete.
    #[command(
        after_help = "With --delete, the matching candidates (default: regenerable and cache) are listed and, after you confirm, moved to the Recycle Bin / Trash. Exit codes: 0 done, 2 partial scan (listing only), 3 some items failed or were refused, 130 cancelled, 1 error or not confirmed."
    )]
    Reclaim {
        #[command(flatten)]
        args: ScanArgs,
        #[arg(long, value_enum, default_value = "table")]
        format: ReportFormat,
        /// Hide candidates smaller than this (e.g. 500MB, 2GiB, 0).
        #[arg(long, default_value = "10MiB", value_parser = parse_size)]
        min_size: u64,
        /// Only these safety classes. With --delete the default is
        /// regenerable,cache; 'review' items are only removed when named here.
        #[arg(long, value_enum, value_delimiter = ',')]
        safety: Option<Vec<SafetyArg>>,
        /// Remove the listed candidates after confirmation. Moves them to the
        /// Recycle Bin / Trash unless --permanent is given.
        #[arg(long)]
        delete: bool,
        /// Delete immediately instead of moving to the Recycle Bin / Trash.
        #[arg(long, requires = "delete")]
        permanent: bool,
        /// Do not ask for confirmation. Required when stdin is not a terminal.
        #[arg(long, short = 'y', requires = "delete")]
        yes: bool,
    },
    /// List local drives with capacity and free space.
    Drives {
        #[arg(long, value_enum, default_value = "table")]
        format: ReportFormat,
    },
    /// Run the local MCP server over stdio (stdout is reserved for the protocol),
    /// or add it to your AI clients (`setup`) and check them (`doctor`).
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,
        /// Explicitly allow scans under this directory. Repeat for more roots.
        #[arg(long, required = true, num_args = 1)]
        root: Vec<PathBuf>,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
        threads: Option<u8>,
        #[arg(long, default_value_t = rilldisk_core::DEFAULT_MAX_ENTRIES)]
        max_entries: usize,
        /// Linux/macOS: let scans enter other mounted filesystems below a root.
        #[arg(long)]
        cross_filesystems: bool,
    },
    /// Check GitHub Releases for a newer RillDisk and install it after you
    /// confirm. Never runs on its own; RILLDISK_NO_UPDATE_CHECK=1 disables it.
    #[command(
        after_help = "Exit codes: 0 up to date or updated, 10 update available (--check), 11 updates unavailable here (disabled, package-managed or unsupported platform), 1 error."
    )]
    Update {
        /// Only report the current and latest versions; change nothing.
        #[arg(long)]
        check: bool,
        /// Install without asking for confirmation.
        #[arg(long, short = 'y', conflicts_with = "check")]
        yes: bool,
        /// Release channel. Defaults to prerelease for pre-release builds,
        /// stable otherwise, or RILLDISK_UPDATE_CHANNEL.
        #[arg(long, value_enum)]
        channel: Option<update::ChannelArg>,
        #[arg(long, value_enum, default_value = "table")]
        format: ReportFormat,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Add RillDisk to installed AI clients (Claude Code, Codex, Cursor, VS Code...).
    /// Shows the exact change and asks before writing; keeps a backup.
    #[command(
        after_help = "Clients: claude-code, claude-desktop, codex, cursor, vscode, windsurf, gemini, zed, opencode, cline.
Only the `rilldisk` entry is written or removed. See docs/MCP.md."
    )]
    Setup {
        /// Client to change. Repeat for more.
        #[arg(long = "client", value_name = "NAME", conflicts_with = "all")]
        clients: Vec<String>,
        /// Every installed client (with --remove: every client that has the entry).
        #[arg(long)]
        all: bool,
        /// Show the changes without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Remove the RillDisk entry instead of adding it.
        #[arg(long)]
        remove: bool,
        /// Apply without asking.
        #[arg(long, short = 'y', conflicts_with = "dry_run")]
        yes: bool,
        /// Directory the server may scan (default: your home folder). Repeat for more.
        #[arg(long = "root", value_name = "DIR")]
        roots: Vec<PathBuf>,
    },
    /// Show which AI clients are installed and configured, and test each
    /// configured RillDisk entry with a real MCP handshake. Changes nothing.
    #[command(after_help = "Exit codes: 0 all configured entries answered, 1 a check failed.")]
    Doctor {
        #[arg(long, value_enum, default_value = "table")]
        format: ReportFormat,
        /// Seconds to wait for each server.
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=120))]
        timeout: u64,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Table,
    Json,
    Jsonl,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum SafetyArg {
    Regenerable,
    Cache,
    Review,
}

impl SafetyArg {
    fn matches(self, safety: Safety) -> bool {
        matches!(
            (self, safety),
            (SafetyArg::Regenerable, Safety::Regenerable)
                | (SafetyArg::Cache, Safety::Cache)
                | (SafetyArg::Review, Safety::Review)
        )
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
    Table,
    Json,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Update {
            check,
            yes,
            channel,
            format,
        } => {
            update::run(update::Args {
                check,
                yes,
                channel,
                json: matches!(format, ReportFormat::Json),
            })
            .await
        }
        Command::Mcp {
            action: Some(action),
            ..
        } => {
            tokio::task::spawn_blocking(move || match action {
                McpAction::Setup {
                    clients,
                    all,
                    dry_run,
                    remove,
                    yes,
                    roots,
                } => setup::run_setup(setup::SetupArgs {
                    clients,
                    all,
                    dry_run,
                    remove,
                    yes,
                    roots,
                }),
                McpAction::Doctor { format, timeout } => setup::run_doctor(
                    matches!(format, ReportFormat::Json),
                    std::time::Duration::from_secs(timeout),
                ),
            })
            .await?
        }
        Command::Mcp {
            action: None,
            root,
            threads,
            max_entries,
            cross_filesystems,
        } => {
            mcp::serve(root, options(threads, max_entries, cross_filesystems)).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Drives { format } => {
            let drives = volumes();
            let mut out = io::stdout().lock();
            match format {
                ReportFormat::Json => writeln!(out, "{}", serde_json::to_string(&drives)?)?,
                ReportFormat::Table => {
                    // Unix mount points are longer than drive letters.
                    let width = drives
                        .iter()
                        .map(|d| d.root.display().to_string().chars().count())
                        .max()
                        .unwrap_or(0)
                        .max(6);
                    writeln!(
                        out,
                        "{:<width$} {:>11} {:>11} {:>6}  {:<6} LABEL",
                        "DRIVE", "USED", "TOTAL", "USED%", "FS"
                    )?;
                    for d in &drives {
                        let used = d.total_bytes.saturating_sub(d.free_bytes);
                        writeln!(
                            out,
                            "{:<width$} {:>11} {:>11} {:>5.1}%  {:<6} {}",
                            d.root.display().to_string(),
                            human(used),
                            human(d.total_bytes),
                            used as f64 * 100.0 / d.total_bytes.max(1) as f64,
                            d.filesystem,
                            d.label
                        )?;
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Scan { args, format, top } => {
            let table = matches!(format, Format::Table);
            run(args, table, |writer, snapshot| {
                write_scan(writer, snapshot, format, top as usize)
            })
            .await
        }
        Command::Reclaim {
            args,
            format,
            min_size,
            safety,
            delete,
            permanent,
            yes,
        } if delete => {
            let code = std::cell::Cell::new(None);
            let scan_code = run(args, |writer, snapshot| {
                let safety = safety.unwrap_or(vec![SafetyArg::Regenerable, SafetyArg::Cache]);
                let mut report = find_reclaimable(snapshot, now_ms());
                report.retain(|c| {
                    c.logical_bytes >= min_size && safety.iter().any(|s| s.matches(c.safety))
                });
                code.set(Some(cleanup::run(
                    writer,
                    snapshot,
                    &report,
                    cleanup::Options {
                        permanent,
                        yes,
                        json: matches!(format, ReportFormat::Json),
                    },
                )?));
                Ok(())
            })
            .await?;
            Ok(code.get().map_or(scan_code, ExitCode::from))
        }
        Command::Reclaim {
            args,
            format,
            min_size,
            safety,
            ..
        } => {
            let table = matches!(format, ReportFormat::Table);
            run(args, table, |writer, snapshot| {
                let mut report = find_reclaimable(snapshot, now_ms());
                report.retain(|c| {
                    c.logical_bytes >= min_size
                        && safety
                            .as_ref()
                            .is_none_or(|s| s.iter().any(|s| s.matches(c.safety)))
                });
                match format {
                    ReportFormat::Json => {
                        serde_json::to_writer(&mut *writer, &report)?;
                        writeln!(writer)?;
                        Ok(())
                    }
                    ReportFormat::Table => write_reclaim(writer, snapshot, &report, min_size),
                }
            })
            .await
        }
    }
}

/// Scan with progress, Ctrl+C cancellation and safe output, then hand the
/// snapshot to `write`. `table` output ends with its own elevation hint; other
/// formats get it on stderr.
async fn run(
    args: ScanArgs,
    table: bool,
    write: impl FnOnce(&mut dyn Write, &Snapshot) -> Result<()>,
) -> Result<ExitCode> {
    let control = Arc::new(ScanControl::default());
    let exporting = Arc::new(AtomicBool::new(false));
    let signal_exporting = exporting.clone();
    let signal_control = control.clone();
    let signal = tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            // A running cleanup stops between items rather than mid-folder.
            if cleanup::cancel_if_running() {
                continue;
            }
            if signal_exporting.load(Ordering::Relaxed) {
                std::process::exit(130);
            }
            signal_control.cancel();
        }
    });
    let progress_control = control.clone();
    let progress = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            eprintln!(
                "Scanning: {} files, {} directories, {}",
                progress_control.files.load(Ordering::Relaxed),
                progress_control.directories.load(Ordering::Relaxed),
                human(progress_control.bytes.load(Ordering::Relaxed)),
            );
        }
    });
    let ScanArgs {
        path,
        output,
        threads,
        max_entries,
        cross_filesystems,
    } = args;
    let options = options(threads, max_entries, cross_filesystems);
    // Only takes effect in an elevated terminal; never asks for elevation itself.
    rilldisk_core::enable_backup_privilege();
    let snapshot = tokio::task::spawn_blocking(move || scan(&path, options, control)).await;
    progress.abort();
    let snapshot = snapshot??;
    exporting.store(true, Ordering::Relaxed);
    let exit_code = ExitCode::from(if snapshot.summary.complete {
        0
    } else if snapshot.summary.cancelled {
        130
    } else {
        2
    });
    let mut writer: Box<dyn Write> = match output {
        Some(path) => Box::new(BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| format!("cannot create new output {}", path.display()))?,
        )),
        None => Box::new(BufWriter::new(io::stdout().lock())),
    };
    let result = write(&mut writer, &snapshot).and_then(|()| Ok(writer.flush()?));
    if let Err(error) = result {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
            || error
                .downcast_ref::<serde_json::Error>()
                .is_some_and(|e| e.io_error_kind() == Some(io::ErrorKind::BrokenPipe))
        {
            return Ok(exit_code);
        }
        return Err(error);
    }
    if !snapshot.summary.complete {
        eprintln!(
            "Partial scan: {} issues; cancelled={}. See JSON summary for details.",
            snapshot.summary.issue_count, snapshot.summary.cancelled
        );
        for issue in &snapshot.summary.issues {
            eprintln!("{}: {}", issue.path.display(), issue.message);
        }
        if !table && let Some(hint) = elevation_hint(&snapshot.summary) {
            eprintln!("{hint}");
        }
    }
    signal.abort();
    Ok(exit_code)
}

fn write_summary(writer: &mut dyn Write, snapshot: &Snapshot) -> Result<()> {
    let summary = &snapshot.summary;
    writeln!(writer, "RillDisk | {}", summary.root.display())?;
    writeln!(
        writer,
        "{} files | {} directories | {} ms | {}",
        summary.files, summary.directories, summary.elapsed_ms, summary.backend
    )?;
    writeln!(
        writer,
        "Logical: {} | Unique: {} | Allocated: {}",
        human(summary.logical_bytes),
        human(summary.unique_logical_bytes),
        summary
            .allocated_bytes
            .map(human)
            .unwrap_or("unknown".into())
    )?;
    if summary.skipped_mounts > 0 {
        writeln!(
            writer,
            "Not entered: {} mount points (sizes excluded). Virtual filesystems such as /proc are always skipped; --cross-filesystems includes other disks",
            summary.skipped_mounts
        )?;
    }
    if !summary.complete {
        writeln!(
            writer,
            "PARTIAL: {} issues ({} access denied); cancelled={}",
            summary.issue_count, summary.access_denied_count, summary.cancelled
        )?;
    }
    Ok(())
}

/// How to read the folders that were refused, when rerunning with more rights would
/// help. Never elevates by itself.
fn elevation_hint(summary: &Summary) -> Option<String> {
    let count = summary.access_denied_count;
    let macos = cfg!(target_os = "macos");
    // Elevated already: only macOS privacy protection can still be in the way.
    if count == 0 || (summary.elevated && !macos) {
        return None;
    }
    Some(hint_text(
        count,
        summary.elevated,
        &rerun_command(std::env::args().skip(1)),
    ))
}

fn hint_text(count: u64, elevated: bool, command: &str) -> String {
    let folders = if count == 1 {
        "1 protected folder".to_string()
    } else {
        format!("{count} protected folders")
    };
    if cfg!(windows) {
        format!("Hint: run from an elevated terminal (Run as administrator) to read {folders}.")
    } else if cfg!(target_os = "macos") {
        let disk_access = "folders protected by macOS privacy controls also need Full Disk Access for your terminal (System Settings > Privacy & Security > Full Disk Access), then a new terminal";
        if elevated {
            format!("Hint: {folders} could not be read even as root; {disk_access}.")
        } else {
            format!("Hint: run `sudo {command}` to read {folders}; {disk_access}.")
        }
    } else {
        format!("Hint: run `sudo {command}` to read {folders}.")
    }
}

/// `rilldisk` and the given arguments, quoted for a POSIX shell.
fn rerun_command(args: impl Iterator<Item = String>) -> String {
    let mut command = String::from("rilldisk");
    for arg in args {
        command.push(' ');
        if !arg.is_empty()
            && arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+%".contains(c))
        {
            command.push_str(&arg);
        } else {
            command.push('\'');
            command.push_str(&arg.replace('\'', r"'\''"));
            command.push('\'');
        }
    }
    command
}

fn write_scan(
    writer: &mut dyn Write,
    snapshot: &Snapshot,
    format: Format,
    top: usize,
) -> Result<()> {
    match format {
        Format::Json => {
            serde_json::to_writer(&mut *writer, snapshot)?;
            writeln!(writer)?;
        }
        Format::Jsonl => {
            serde_json::to_writer(
                &mut *writer,
                &serde_json::json!({"type":"summary", "data": snapshot.summary}),
            )?;
            writeln!(writer)?;
            for entry in &snapshot.entries {
                serde_json::to_writer(
                    &mut *writer,
                    &serde_json::json!({"type":"entry", "data": snapshot.view(entry.id)}),
                )?;
                writeln!(writer)?;
            }
        }
        Format::Table => {
            write_summary(writer, snapshot)?;
            writeln!(writer, "\n{:>12}  {:>12}  PATH", "LOGICAL", "ALLOCATED")?;
            for entry in snapshot.query(None, None, true, 0, top).1 {
                writeln!(
                    writer,
                    "{:>12}  {:>12}  {}{}",
                    human(entry.logical_bytes),
                    entry.allocated_bytes.map(human).unwrap_or("unknown".into()),
                    snapshot.path(entry.id).display(),
                    if entry.hard_link_duplicate {
                        " [hard link: already counted]"
                    } else {
                        ""
                    }
                )?;
            }
            let report = find_reclaimable(snapshot, now_ms());
            if report.candidate_count > 0 {
                writeln!(
                    writer,
                    "\nReclaimable: frees at least {} ({} total size) in {} candidates. Run `rilldisk reclaim` on this path for details.",
                    human(report.total_exclusive_bytes),
                    human(report.total_logical_bytes),
                    report.candidate_count
                )?;
            }
            if let Some(hint) = elevation_hint(&snapshot.summary) {
                writeln!(writer, "\n{hint}")?;
            }
        }
    }
    Ok(())
}

fn write_reclaim(
    writer: &mut dyn Write,
    snapshot: &Snapshot,
    report: &ReclaimReport,
    min_size: u64,
) -> Result<()> {
    write_summary(writer, snapshot)?;
    let outer = report.candidates.iter().filter(|c| c.nested_in.is_none());
    let size: u64 = outer.clone().map(|c| c.logical_bytes).sum();
    let frees: u64 = outer.map(|c| c.exclusive_bytes).sum();
    writeln!(
        writer,
        "\nReclaimable: frees at least {} on disk ({} total size) in {} candidates (>= {}).",
        human(frees),
        human(size),
        report.candidates.len(),
        human(min_size)
    )?;
    writeln!(writer, "{}\n", report.notice)?;
    writeln!(
        writer,
        "{:>10}  {:>10}  {:<12} {:>6}  {:<26} PATH",
        "SIZE", "FREES", "SAFETY", "IDLE", "KIND"
    )?;
    for c in &report.candidates {
        writeln!(
            writer,
            "{:>10}  {:>10}  {:<12} {:>6}  {:<26} {}{}",
            human(c.logical_bytes),
            human(c.exclusive_bytes),
            c.safety.label(),
            c.idle_days.map(|d| format!("{d}d")).unwrap_or("?".into()),
            c.label,
            c.path.display(),
            if c.nested_in.is_some() {
                "  (inside another candidate)"
            } else {
                ""
            }
        )?;
        writeln!(writer, "{:>22}  -> {}", "", c.suggested_action)?;
    }
    if let Some(hint) = elevation_hint(&snapshot.summary) {
        writeln!(writer, "\n{hint}")?;
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn options(threads: Option<u8>, max_entries: usize, cross_filesystems: bool) -> ScanOptions {
    ScanOptions {
        threads: threads.map_or_else(|| ScanOptions::default().threads, usize::from),
        max_entries,
        cross_filesystems,
    }
}

fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("invalid size: {text}"))?;
    let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        other => return Err(format!("unknown size unit: {other}")),
    };
    Ok((number * multiplier as f64) as u64)
}

pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerun_command_quotes_for_a_shell() {
        let args = ["scan", "/", "--format", "table"].map(String::from);
        assert_eq!(
            rerun_command(args.into_iter()),
            "rilldisk scan / --format table"
        );
        let args = ["scan", "/Volumes/My Disk", "it's"].map(String::from);
        assert_eq!(
            rerun_command(args.into_iter()),
            r"rilldisk scan '/Volumes/My Disk' 'it'\''s'"
        );
    }

    #[test]
    fn hint_names_the_platform_route() {
        let hint = hint_text(211, false, "rilldisk scan /");
        assert!(hint.contains("211 protected folders"), "{hint}");
        if cfg!(windows) {
            assert!(hint.contains("Run as administrator"), "{hint}");
        } else {
            assert!(hint.contains("sudo rilldisk scan /"), "{hint}");
        }
        if cfg!(target_os = "macos") {
            assert!(hint_text(1, true, "x").contains("Full Disk Access"));
        }
        assert!(hint_text(1, false, "x").contains("1 protected folder"));
    }
}
