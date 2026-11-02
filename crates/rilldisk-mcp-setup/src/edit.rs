// SPDX-License-Identifier: AGPL-3.0-only
//! Format-preserving edits of RillDisk's entry. JSON files are edited as JSONC
//! through a concrete syntax tree and TOML through `toml_edit`, so comments,
//! key order, indentation and every other entry stay exactly as they were.
use crate::{Format, Launch, SERVER_NAME, Style};
use anyhow::{Context, Result, bail};
use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstObject, CstRootNode},
};
use serde_json::Value;

/// RillDisk's entry as the client reads it, if there is one.
pub fn read_entry(format: Format, text: &str) -> Result<Option<Launch>> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    match format {
        Format::Json(key, style) => {
            let value: Value = jsonc_parser::parse_to_serde_value(text, &ParseOptions::default())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if !value.is_object() {
                bail!("top level is not a JSON object");
            }
            let Some(entry) = value.get(key).and_then(|v| v.get(SERVER_NAME)) else {
                return Ok(None);
            };
            let strings = |v: Option<&Value>| -> Vec<String> {
                v.and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let (command, args) = match style {
                Style::OpenCode => {
                    let mut parts = strings(entry.get("command"));
                    let command = if parts.is_empty() {
                        String::new()
                    } else {
                        parts.remove(0)
                    };
                    (command, parts)
                }
                _ => (
                    entry
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    strings(entry.get("args")),
                ),
            };
            let env_key = if matches!(style, Style::OpenCode) {
                "environment"
            } else {
                "env"
            };
            let env = entry
                .get(env_key)
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Some(Launch { command, args, env }))
        }
        Format::CodexToml => {
            let doc: toml_edit::DocumentMut = text.parse().context("invalid TOML")?;
            let Some(entry) = doc
                .get("mcp_servers")
                .and_then(|s| s.get(SERVER_NAME))
                .and_then(|e| e.as_table_like())
            else {
                return Ok(None);
            };
            let command = entry
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let args = entry
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let env = entry
                .get("env")
                .and_then(|v| v.as_table_like())
                .map(|t| {
                    t.iter()
                        .filter_map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Some(Launch { command, args, env }))
        }
    }
}

/// The JSON entry a client expects, in the key order its docs use.
fn json_entry(style: Style, launch: &Launch) -> Vec<(&'static str, Value)> {
    let command = Value::from(launch.command.as_str());
    let args = Value::from(launch.args.clone());
    let env = || Value::Object(Default::default());
    match style {
        Style::Plain => vec![("command", command), ("args", args)],
        Style::TypedStdio => vec![
            ("type", "stdio".into()),
            ("command", command),
            ("args", args),
        ],
        Style::ClaudeCode => vec![
            ("type", "stdio".into()),
            ("command", command),
            ("args", args),
            ("env", env()),
        ],
        Style::WithEnv => vec![("command", command), ("args", args), ("env", env())],
        Style::OpenCode => {
            let mut parts = vec![launch.command.clone()];
            parts.extend(launch.args.iter().cloned());
            vec![
                ("type", "local".into()),
                ("command", parts.into()),
                ("enabled", true.into()),
            ]
        }
    }
}

fn object(entry: &[(&str, Value)]) -> CstInputValue {
    CstInputValue::Object(
        entry
            .iter()
            .map(|(k, v)| (k.to_string(), input(v)))
            .collect(),
    )
}

/// `before` with RillDisk's entry added, or updated to `launch`.
pub fn add(format: Format, before: Option<&str>, launch: &Launch) -> Result<String> {
    let before = before.unwrap_or("");
    match format {
        Format::Json(key, style) => {
            let entry = json_entry(style, launch);
            let root = parse_json(if before.trim().is_empty() {
                "{}\n"
            } else {
                before
            })?;
            let top = root
                .object_value()
                .context("top level is not a JSON object")?;
            let servers = top
                .object_value_or_create(key)
                .with_context(|| format!("`{key}` is not a JSON object"))?;
            match servers.get(SERVER_NAME) {
                Some(prop) => match prop.object_value() {
                    // Update in place: keep the user's extra keys and their values.
                    Some(existing) => merge(&existing, &entry),
                    None => prop.set_value(object(&entry)),
                },
                None => {
                    servers.append(SERVER_NAME, object(&entry));
                }
            }
            Ok(root.to_string())
        }
        Format::CodexToml => {
            use toml_edit::{Array, DocumentMut, Item, Table, value};
            let mut doc: DocumentMut = before.parse().context("invalid TOML")?;
            let servers = doc.entry("mcp_servers").or_insert_with(|| {
                let mut table = Table::new();
                table.set_implicit(true);
                Item::Table(table)
            });
            let servers = servers
                .as_table_like_mut()
                .context("`mcp_servers` is not a table")?;
            if servers.get(SERVER_NAME).is_none() {
                servers.insert(SERVER_NAME, Item::Table(Table::new()));
            }
            let entry = servers
                .get_mut(SERVER_NAME)
                .and_then(Item::as_table_like_mut)
                .context("`mcp_servers.rilldisk` is not a table")?;
            let args: Array = launch.args.iter().map(String::as_str).collect();
            set_toml(entry, "command", value(launch.command.as_str()));
            set_toml(entry, "args", value(args));
            Ok(same_newlines(before, doc.to_string()))
        }
    }
}

/// Replaces a TOML value, keeping its surrounding decoration (comments).
fn set_toml(table: &mut dyn toml_edit::TableLike, key: &str, mut item: toml_edit::Item) {
    if let (Some(old), Some(new)) = (
        table.get(key).and_then(|i| i.as_value()),
        item.as_value_mut(),
    ) {
        *new.decor_mut() = old.decor().clone();
    }
    table.insert(key, item);
}

/// `before` without RillDisk's entry. Other entries and the file stay as they were.
pub fn remove(format: Format, before: Option<&str>) -> Result<String> {
    let Some(before) = before.filter(|t| !t.trim().is_empty()) else {
        return Ok(before.unwrap_or("").to_string());
    };
    match format {
        Format::Json(key, _) => {
            let root = parse_json(before)?;
            let top = root
                .object_value()
                .context("top level is not a JSON object")?;
            if let Some(prop) = top.object_value(key).and_then(|s| s.get(SERVER_NAME)) {
                prop.remove();
            }
            Ok(root.to_string())
        }
        Format::CodexToml => {
            let mut doc: toml_edit::DocumentMut = before.parse().context("invalid TOML")?;
            if let Some(servers) = doc
                .get_mut("mcp_servers")
                .and_then(toml_edit::Item::as_table_like_mut)
            {
                servers.remove(SERVER_NAME);
            }
            Ok(same_newlines(before, doc.to_string()))
        }
    }
}

/// `toml_edit` writes `\n`; keep a CRLF file CRLF.
fn same_newlines(before: &str, after: String) -> String {
    if before.contains("\r\n") {
        after.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        after
    }
}

fn parse_json(text: &str) -> Result<CstRootNode> {
    CstRootNode::parse(text, &ParseOptions::default()).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Sets our keys on an existing entry. Objects (such as `env`) are only added
/// when missing, so the user's environment variables survive a reconnect.
fn merge(existing: &CstObject, entry: &[(&str, Value)]) {
    for (key, value) in entry {
        match existing.get(key) {
            Some(_) if value.is_object() => {}
            Some(prop) => prop.set_value(input(value)),
            None => {
                existing.append(key, input(value));
            }
        }
    }
}

fn input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(a) => CstInputValue::Array(a.iter().map(input).collect()),
        Value::Object(o) => {
            CstInputValue::Object(o.iter().map(|(k, v)| (k.clone(), input(v))).collect())
        }
    }
}
