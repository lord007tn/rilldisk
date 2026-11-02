// SPDX-License-Identifier: AGPL-3.0-only
//! `rilldisk mcp setup` and `rilldisk mcp doctor`: add RillDisk to AI clients
//! and check that what they would run really answers. Scope and safeguards:
//! docs/proposals/mcp-setup.md.
use anyhow::{Context, Result, bail};
use rilldisk_mcp_setup::{
    self as setup, Change, ClientId, Detection, Env, Server, Status, check, detect, plan,
};
use serde_json::json;
use std::{
    io::{self, BufRead, IsTerminal, Write},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

pub struct SetupArgs {
    pub clients: Vec<String>,
    pub all: bool,
    pub dry_run: bool,
    pub remove: bool,
    pub yes: bool,
    pub roots: Vec<PathBuf>,
}

fn this_server(env: &Env, roots: &[PathBuf]) -> Result<Server> {
    let exe = std::env::current_exe().context("cannot locate the rilldisk executable")?;
    let roots = if roots.is_empty() {
        vec![env.home.clone()]
    } else {
        roots
            .iter()
            .map(|r| std::path::absolute(r).with_context(|| format!("bad root {}", r.display())))
            .collect::<Result<_>>()?
    };
    Ok(Server::rilldisk(exe, &roots))
}

pub fn run_setup(args: SetupArgs) -> Result<ExitCode> {
    let env = Env::from_system();
    let server = this_server(&env, &args.roots)?;
    let change = if args.remove {
        Change::Remove
    } else {
        Change::Add
    };
    let detections = detect(&env, &server);
    let targets: Vec<ClientId> = if args.all {
        detections
            .iter()
            .filter(|d| match change {
                Change::Add => d.status != Status::NotInstalled,
                Change::Remove => d.status.has_entry(),
            })
            .map(|d| d.client)
            .collect()
    } else if args.clients.is_empty() {
        let found: Vec<_> = detections
            .iter()
            .filter(|d| d.status != Status::NotInstalled)
            .map(|d| {
                format!(
                    "  {:<16}{:<16}{}",
                    d.client.key(),
                    d.status.label(),
                    env.display(&d.config)
                )
            })
            .collect();
        bail!(
            "choose clients with --client <name> or use --all.\nDetected:\n{}",
            if found.is_empty() {
                "  (none)".to_string()
            } else {
                found.join("\n")
            }
        );
    } else {
        args.clients
            .iter()
            .map(|name| {
                ClientId::parse(name).with_context(|| {
                    format!(
                        "unknown client {name:?}; known: {}",
                        ClientId::ALL.map(ClientId::key).join(", ")
                    )
                })
            })
            .collect::<Result<_>>()?
    };
    if targets.is_empty() {
        println!("No installed AI clients to change.");
        return Ok(ExitCode::SUCCESS);
    }

    let mut out = io::stdout().lock();
    writeln!(
        out,
        "Server: {} {}",
        server.command.display(),
        server.args.join(" ")
    )?;
    let mut plans = Vec::new();
    for client in targets {
        let p = plan(&env, client, &server, change)?;
        writeln!(out, "\n{} ({})", client.name(), env.display(&p.path))?;
        if !p.changes_anything() {
            writeln!(
                out,
                "  {}",
                match change {
                    Change::Add => "already configured; nothing to change",
                    Change::Remove => "no RillDisk entry; nothing to change",
                }
            )?;
            continue;
        }
        if let Some(command) = &p.command {
            writeln!(out, "  Runs: {}", command.join(" "))?;
            writeln!(out, "  Expected change:")?;
        }
        write!(out, "{}", p.diff)?;
        plans.push(p);
    }
    if plans.is_empty() || args.dry_run {
        if args.dry_run && !plans.is_empty() {
            writeln!(out, "\nDry run: nothing was written.")?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    out.flush()?;
    if !args.yes && !confirm()? {
        eprintln!("Cancelled; nothing was changed.");
        return Ok(ExitCode::from(1));
    }
    let mut failed = false;
    for p in &plans {
        match setup::apply(p) {
            Ok(applied) => {
                write!(
                    out,
                    "\n{}: updated {}",
                    p.client.name(),
                    env.display(&p.path)
                )?;
                match &applied.backup {
                    Some(backup) => writeln!(out, " (backup: {})", env.display(backup))?,
                    None => writeln!(out)?,
                }
                if change == Change::Add {
                    let launch = setup::detect_one(&env, p.client, &server).launch;
                    match launch.map(|l| check(&l, setup::DEFAULT_TIMEOUT)) {
                        Some(Ok(c)) => writeln!(out, "  Connection: {}", describe(&c))?,
                        Some(Err(e)) => {
                            failed = true;
                            writeln!(out, "  Connection failed: {e}")?;
                        }
                        None => {
                            failed = true;
                            writeln!(out, "  Entry not found after the change")?;
                        }
                    }
                }
            }
            Err(error) => {
                failed = true;
                writeln!(out, "\n{}: {error:#}", p.client.name())?;
            }
        }
    }
    if change == Change::Add && !failed {
        writeln!(
            out,
            "\nRestart the clients (or reload their MCP servers) to pick up RillDisk."
        )?;
    }
    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn describe(c: &setup::Check) -> String {
    format!(
        "ok · {} tools · {} ms · {}",
        c.tools.len(),
        c.latency_ms,
        c.server
    )
}

pub fn run_doctor(json_output: bool, timeout: Duration) -> Result<ExitCode> {
    let env = Env::from_system();
    let server = this_server(&env, &[])?;
    let detections = detect(&env, &server);
    // Test what each client would run, in parallel: one slow entry should not
    // multiply the wait.
    let checks: Vec<Option<Result<setup::Check, setup::CheckError>>> = std::thread::scope(|s| {
        let handles: Vec<_> = detections
            .iter()
            .map(|d| {
                s.spawn(move || {
                    d.launch
                        .as_ref()
                        .filter(|_| d.status.has_entry())
                        .map(|l| check(l, timeout))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(None))
            .collect()
    });
    let failed = checks.iter().any(|c| matches!(c, Some(Err(_))));
    if json_output {
        let clients: Vec<_> = detections
            .iter()
            .zip(&checks)
            .map(|(d, c)| client_json(&env, d, c.as_ref()))
            .collect();
        let value = json!({
            "server": {"command": server.command, "args": server.args},
            "clients": clients,
        });
        println!("{value}");
    } else {
        let mut out = io::stdout().lock();
        writeln!(out, "RillDisk MCP server: {}", server.command.display())?;
        writeln!(
            out,
            "\n{:<16}{:<22}{:<34}CONFIG",
            "CLIENT", "STATUS", "CONNECTION"
        )?;
        for (d, c) in detections.iter().zip(&checks) {
            let connection = match c {
                Some(Ok(c)) => format!("ok · {} tools · {} ms", c.tools.len(), c.latency_ms),
                Some(Err(e)) => format!("FAILED: {e}"),
                None => "-".into(),
            };
            let config = if d.status == Status::NotInstalled {
                "-".to_string()
            } else {
                env.display(&d.config)
            };
            writeln!(
                out,
                "{:<16}{:<22}{:<34}{}",
                d.name,
                d.status.label(),
                connection,
                config
            )?;
            if let Status::Stale { command, .. } = &d.status {
                writeln!(out, "{:<16}  points to {command}", "")?;
            }
            if let Status::Invalid { error } = &d.status {
                writeln!(out, "{:<16}  {error}", "")?;
            }
        }
        let fix: Vec<_> = detections
            .iter()
            .filter(|d| matches!(d.status, Status::NotConfigured | Status::Stale { .. }))
            .map(|d| d.client.key())
            .collect();
        if !fix.is_empty() {
            writeln!(
                out,
                "\nConnect with: rilldisk mcp setup --client {} --dry-run",
                fix.join(" --client ")
            )?;
        }
    }
    Ok(if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn client_json(
    env: &Env,
    d: &Detection,
    check: Option<&Result<setup::Check, setup::CheckError>>,
) -> serde_json::Value {
    json!({
        "client": d.client,
        "name": d.name,
        "config": d.config,
        "config_display": env.display(&d.config),
        "status": d.status,
        "launch": d.launch,
        "check": match check {
            Some(Ok(c)) => json!({"ok": true, "server": c.server, "protocol_version": c.protocol_version,
                "tools": c.tools, "latency_ms": c.latency_ms}),
            Some(Err(e)) => json!({"ok": false, "message": e.to_string(), "detail": e}),
            None => serde_json::Value::Null,
        },
    })
}

fn confirm() -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("refusing to change client configs without confirmation; re-run with --yes");
    }
    eprint!("\nApply these changes? Backups are kept next to each file. [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES"))
}
