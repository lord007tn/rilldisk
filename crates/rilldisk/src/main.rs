// SPDX-License-Identifier: AGPL-3.0-only
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use rilldisk_core::{ScanControl, ScanOptions, scan};
use std::{
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
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

#[derive(Subcommand)]
enum Command {
    /// Scan a directory or drive without reading file contents.
    Scan {
        path: PathBuf,
        #[arg(long, value_enum, default_value = "table")]
        format: Format,
        /// Write to a NEW file. Existing files are never overwritten.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=500))]
        top: u16,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
        threads: Option<u8>,
        #[arg(long, default_value_t = 5_000_000)]
        max_entries: usize,
    },
    /// Run the local MCP server over stdio (stdout is reserved for the protocol).
    Mcp {
        /// Explicitly allow scans under this directory. Repeat for more roots.
        #[arg(long, required = true, num_args = 1)]
        root: Vec<PathBuf>,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
        threads: Option<u8>,
        #[arg(long, default_value_t = 5_000_000)]
        max_entries: usize,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Table,
    Json,
    Jsonl,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<std::process::ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Command::Mcp {
            root,
            threads,
            max_entries,
        } => {
            mcp::serve(root, options(threads, max_entries)).await?;
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Scan {
            path,
            format,
            output,
            top,
            threads,
            max_entries,
        } => {
            let control = Arc::new(ScanControl::default());
            let signal_control = control.clone();
            let signal = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_control.cancel();
                }
            });
            let progress_control = control.clone();
            let progress = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    eprintln!(
                        "Scanning: {} files, {} directories",
                        progress_control.files.load(Ordering::Relaxed),
                        progress_control.directories.load(Ordering::Relaxed)
                    );
                }
            });
            let snapshot = tokio::task::spawn_blocking(move || {
                scan(&path, options(threads, max_entries), control)
            })
            .await;
            signal.abort();
            progress.abort();
            let snapshot = snapshot??;
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
            let result = (|| -> Result<()> {
                match format {
                    Format::Json => {
                        serde_json::to_writer(&mut writer, &snapshot)?;
                        writeln!(writer)?;
                    }
                    Format::Jsonl => {
                        serde_json::to_writer(
                            &mut writer,
                            &serde_json::json!({"type":"summary", "data": snapshot.summary}),
                        )?;
                        writeln!(writer)?;
                        for entry in &snapshot.entries {
                            serde_json::to_writer(
                                &mut writer,
                                &serde_json::json!({"type":"entry", "data": entry}),
                            )?;
                            writeln!(writer)?;
                        }
                    }
                    Format::Table => {
                        writeln!(writer, "RillDisk | {}", snapshot.summary.root.display())?;
                        writeln!(
                            writer,
                            "{} files | {} directories | {} ms | {}",
                            snapshot.summary.files,
                            snapshot.summary.directories,
                            snapshot.summary.elapsed_ms,
                            snapshot.summary.backend
                        )?;
                        writeln!(
                            writer,
                            "Logical: {} | Unique: {} | Allocated: {}",
                            human(snapshot.summary.logical_bytes),
                            human(snapshot.summary.unique_logical_bytes),
                            snapshot
                                .summary
                                .allocated_bytes
                                .map(human)
                                .unwrap_or("unknown".into())
                        )?;
                        writeln!(writer, "\n{:>12}  {:>12}  PATH", "LOGICAL", "ALLOCATED")?;
                        for entry in snapshot.query(None, None, true, 0, top as usize).1 {
                            writeln!(
                                writer,
                                "{:>12}  {:>12}  {}{}",
                                human(entry.logical_bytes),
                                entry.allocated_bytes.map(human).unwrap_or("unknown".into()),
                                entry.path.display(),
                                if entry.hard_link_duplicate {
                                    " [hard link: already counted]"
                                } else {
                                    ""
                                }
                            )?;
                        }
                    }
                }
                writer.flush()?;
                Ok(())
            })();
            if let Err(error) = result {
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
                    || error
                        .downcast_ref::<serde_json::Error>()
                        .is_some_and(|e| e.io_error_kind() == Some(io::ErrorKind::BrokenPipe))
                {
                    return Ok(std::process::ExitCode::SUCCESS);
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
                return Ok(std::process::ExitCode::from(
                    if snapshot.summary.cancelled { 130 } else { 2 },
                ));
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
    }
}

fn options(threads: Option<u8>, max_entries: usize) -> ScanOptions {
    ScanOptions {
        threads: threads.map_or_else(|| ScanOptions::default().threads, usize::from),
        max_entries,
    }
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
