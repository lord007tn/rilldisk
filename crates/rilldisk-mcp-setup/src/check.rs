// SPDX-License-Identifier: AGPL-3.0-only
//! Connection check: start the configured server exactly as a client would and
//! speak MCP over stdio (`initialize`, `notifications/initialized`,
//! `tools/list`). The child is always killed afterwards.
use crate::Launch;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    /// Server name and version from `initialize`.
    pub server: String,
    pub protocol_version: String,
    pub tools: Vec<String>,
    /// From spawning the process to the `tools/list` result.
    pub latency_ms: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum CheckError {
    /// The configured executable does not exist.
    Missing { command: String },
    /// The process could not be started.
    Spawn { message: String },
    /// The process exited before answering.
    Exited { code: Option<i32>, stderr: String },
    /// No answer within the timeout.
    Timeout { seconds: u64, during: &'static str },
    /// It answered, but not with valid MCP.
    Protocol { message: String },
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Missing { command } => write!(f, "executable not found: {command}"),
            CheckError::Spawn { message } => write!(f, "cannot start: {message}"),
            CheckError::Exited { code, stderr } => {
                match code {
                    Some(code) => write!(f, "exited with code {code}")?,
                    None => write!(f, "exited")?,
                }
                match stderr.trim().lines().last() {
                    Some(line) => write!(f, ": {line}"),
                    None => Ok(()),
                }
            }
            CheckError::Timeout { seconds, during } => {
                write!(f, "no answer to {during} within {seconds} s")
            }
            CheckError::Protocol { message } => write!(f, "protocol error: {message}"),
        }
    }
}

impl std::error::Error for CheckError {}

/// Hides the console window a GUI parent would otherwise open for the child.
pub(crate) fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Kills the child when the check ends, however it ends.
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn check(launch: &Launch, timeout: Duration) -> Result<Check, CheckError> {
    let path = std::path::Path::new(&launch.command);
    if path.is_absolute() && !path.is_file() {
        return Err(CheckError::Missing {
            command: launch.command.clone(),
        });
    }
    let started = Instant::now();
    let deadline = started + timeout;
    let mut command = Command::new(&launch.command);
    command
        .args(&launch.args)
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(&mut command);
    let child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CheckError::Missing {
                command: launch.command.clone(),
            }
        } else {
            CheckError::Spawn {
                message: e.to_string(),
            }
        }
    })?;
    let mut guard = Guard(child);
    let mut stdin = guard.0.stdin.take().expect("piped stdin");
    let stdout = guard.0.stdout.take().expect("piped stdout");
    let mut stderr = guard.0.stderr.take().expect("piped stderr");

    // Keep the last few KiB of stderr for error messages, draining the pipe so
    // a chatty server never blocks.
    let errors = Arc::new(Mutex::new(String::new()));
    let sink = errors.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut text = sink.lock().unwrap_or_else(|e| e.into_inner());
            text.push_str(&String::from_utf8_lossy(&buf[..n]));
            if text.len() > 4096 {
                let cut = text.len() - 4096;
                let cut = (cut..text.len())
                    .find(|&i| text.is_char_boundary(i))
                    .unwrap_or(0);
                text.drain(..cut);
            }
        }
    });
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let exited = |guard: &mut Guard| {
        // Give the stderr reader a moment to collect the final message.
        let code = (0..20).find_map(|_| match guard.0.try_wait() {
            Ok(Some(status)) => Some(status.code()),
            _ => {
                std::thread::sleep(Duration::from_millis(10));
                None
            }
        });
        std::thread::sleep(Duration::from_millis(20));
        CheckError::Exited {
            code: code.flatten(),
            stderr: errors.lock().map(|s| s.clone()).unwrap_or_default(),
        }
    };
    let mut send = |guard: &mut Guard, message: Value| -> Result<(), CheckError> {
        let mut line = message.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|_| exited(guard))
    };
    let wait = |guard: &mut Guard, id: u64, during: &'static str| -> Result<Value, CheckError> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match rx.recv_timeout(left) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(CheckError::Timeout {
                        seconds: timeout.as_secs().max(1),
                        during,
                    });
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(exited(guard)),
            };
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(&line).map_err(|_| CheckError::Protocol {
                message: format!("stdout is not JSON-RPC: {}", truncate(&line)),
            })?;
            // Skip notifications and requests from the server.
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(CheckError::Protocol {
                    message: format!(
                        "{during} failed: {}",
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                    ),
                });
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| CheckError::Protocol {
                    message: format!("{during} response has no result"),
                });
        }
    };

    send(
        &mut guard,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "rilldisk-doctor", "version": env!("CARGO_PKG_VERSION")}
        }}),
    )?;
    let init = wait(&mut guard, 1, "initialize")?;
    let info = init.get("serverInfo");
    let field = |key: &str| {
        info.and_then(|i| i.get(key))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    let server = format!("{} {}", field("name"), field("version"));
    let protocol_version = init
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| CheckError::Protocol {
            message: "initialize result has no protocolVersion".into(),
        })?
        .to_string();
    send(
        &mut guard,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )?;
    send(
        &mut guard,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )?;
    let listed = wait(&mut guard, 2, "tools/list")?;
    let tools = listed
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| CheckError::Protocol {
            message: "tools/list result has no tools".into(),
        })?
        .iter()
        .filter_map(|t| t.get("name")?.as_str().map(String::from))
        .collect();
    let latency_ms = started.elapsed().as_millis() as u64;
    drop(guard);
    Ok(Check {
        server,
        protocol_version,
        tools,
        latency_ms,
    })
}

fn truncate(text: &str) -> String {
    let mut short: String = text.chars().take(120).collect();
    if short.len() < text.len() {
        short.push('…');
    }
    short
}
