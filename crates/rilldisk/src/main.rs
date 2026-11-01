// SPDX-License-Identifier: AGPL-3.0-only
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use rilldisk_core::{
    ReclaimReport, ScanControl, ScanOptions, Snapshot, find_reclaimable, scan, volumes,
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

mod mcp;

#[derive(Parser)]
#[command(
    version,
    about = "Windows-first disk intelligence. Local, read-only, native Rust.",
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
    /// folders and caches. Suggests actions; never deletes anything.
    Reclaim {
        #[command(flatten)]
        args: ScanArgs,
        #[arg(long, value_enum, default_value = "table")]
        format: ReportFormat,
        /// Hide candidates smaller than this (e.g. 500MB, 2GiB, 0).
        #[arg(long, default_value = "10MiB", value_parser = parse_size)]
        min_size: u64,
    },
    /// List local drives with capacity and free space.
    Drives {
        #[arg(long, value_enum, default_value = "table")]
        format: ReportFormat,
    },
    /// Run the local MCP server over stdio (stdout is reserved for the protocol).
    Mcp {
        /// Explicitly allow scans under this directory. Repeat for more roots.
        #[arg(long, required = true, num_args = 1)]
        root: Vec<PathBuf>,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
        threads: Option<u8>,
        #[arg(long, default_value_t = rilldisk_core::DEFAULT_MAX_ENTRIES)]
        max_entries: usize,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Table,
    Json,
    Jsonl,
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
    Table,
    Json,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Mcp {
            root,
            threads,
            max_entries,
        } => {
            mcp::serve(root, options(threads, max_entries)).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Drives { format } => {
            let drives = volumes();
            if drives.is_empty() && !cfg!(windows) {
                anyhow::bail!("drive listing is not available on this platform yet");
            }
            let mut out = io::stdout().lock();
            match format {
                ReportFormat::Json => writeln!(out, "{}", serde_json::to_string(&drives)?)?,
                ReportFormat::Table => {
                    writeln!(
                        out,
                        "{:<6} {:>11} {:>11} {:>6}  {:<6} LABEL",
                        "DRIVE", "USED", "TOTAL", "USED%", "FS"
                    )?;
                    for d in &drives {
                        let used = d.total_bytes.saturating_sub(d.free_bytes);
                        writeln!(
                            out,
                            "{:<6} {:>11} {:>11} {:>5.1}%  {:<6} {}",
                            d.root.display(),
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
            run(args, |writer, snapshot| {
                write_scan(writer, snapshot, format, top as usize)
            })
            .await
        }
        Command::Reclaim {
            args,
            format,
            min_size,
        } => {
            run(args, |writer, snapshot| {
                let mut report = find_reclaimable(snapshot, now_ms());
                report.retain(|c| c.logical_bytes >= min_size);
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
/// snapshot to `write`.
async fn run(
    args: ScanArgs,
    write: impl FnOnce(&mut dyn Write, &Snapshot) -> Result<()>,
) -> Result<ExitCode> {
    let control = Arc::new(ScanControl::default());
    let exporting = Arc::new(AtomicBool::new(false));
    let signal_exporting = exporting.clone();
    let signal_control = control.clone();
    let signal = tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
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
    } = args;
    let snapshot =
        tokio::task::spawn_blocking(move || scan(&path, options(threads, max_entries), control))
            .await;
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
    if !summary.complete {
        writeln!(
            writer,
            "PARTIAL: {} issues; cancelled={}",
            summary.issue_count, summary.cancelled
        )?;
    }
    Ok(())
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
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn options(threads: Option<u8>, max_entries: usize) -> ScanOptions {
    ScanOptions {
        threads: threads.map_or_else(|| ScanOptions::default().threads, usize::from),
        max_entries,
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

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}
