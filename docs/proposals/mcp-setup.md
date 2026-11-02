# Proposal: connect RillDisk to AI clients

Status: implemented, November 2, 2026, in the CLI (`rilldisk mcp setup`,
`rilldisk mcp doctor`) and the desktop (the **AI assistants** card on Home).
The shared code is the `rilldisk-mcp-setup` crate.

Phase one is read-only, and AGENTS.md requires a scoped proposal before
RillDisk writes files or starts processes. This document is that scope.
Anything not described here is out of scope.

## What changes

Until now the desktop offered a Copy button with a generic JSON snippet, which
named `rilldisk` without a path and didn't pass the required `--root`. Users had
to work out which file their client reads, merge the snippet by hand and guess
whether it worked. RillDisk now:

1. **Detects** supported clients from their user config folder or the client
   executable on `PATH`.
2. **Shows** each client's status: not installed, not configured, configured
   (runs this RillDisk), stale (runs a RillDisk that is missing or is another
   copy), or unreadable config.
3. **Connects** a client after showing the exact change, and can **remove**
   the entry again.
4. **Checks** the connection by starting the configured command the way the
   client would and performing an MCP handshake.

## Supported clients and the only files written

All are user-level files. Project files (`.mcp.json`, `.vscode/mcp.json`,
`.cursor/mcp.json` in a repository) are never touched.

| Client | File | Entry |
| --- | --- | --- |
| Claude Code | `~/.claude.json` (`$CLAUDE_CONFIG_DIR/.claude.json` when set) | `mcpServers.rilldisk` `{type: "stdio", command, args, env: {}}` |
| Claude Desktop | `%APPDATA%\Claude\`, `~/Library/Application Support/Claude/`, `~/.config/Claude/` + `claude_desktop_config.json` | `mcpServers.rilldisk` `{command, args}` |
| Codex CLI | `~/.codex/config.toml` (`$CODEX_HOME/config.toml` when set) | `[mcp_servers.rilldisk]` `command`, `args` |
| Cursor | `~/.cursor/mcp.json` | `mcpServers.rilldisk` `{type: "stdio", command, args}` |
| VS Code | `%APPDATA%\Code\User\mcp.json`, `~/Library/Application Support/Code/User/mcp.json`, `~/.config/Code/User/mcp.json` | `servers.rilldisk` `{type: "stdio", command, args}` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | `mcpServers.rilldisk` `{command, args}` |
| Gemini CLI | `~/.gemini/settings.json` | `mcpServers.rilldisk` `{command, args}` |
| Zed | `%APPDATA%\Zed\settings.json`, `~/.config/zed/settings.json` | `context_servers.rilldisk` `{command, args, env: {}}` |
| OpenCode | `~/.config/opencode/opencode.json` (or `opencode.jsonc`) | `mcp.rilldisk` `{type: "local", command: [exe, ...args], enabled: true}` |
| Cline (VS Code extension) | `<VS Code user>/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json` | `mcpServers.rilldisk` `{command, args}` |

Formats were checked against each tool's documentation on November 2, 2026.
The entry always uses the **absolute path** of the `rilldisk` CLI. The desktop
uses the copy shipped next to it, which every package installs, and the CLI uses
its own path. The args are `["mcp", "--root", <home>]`; `rilldisk mcp setup
--root DIR` picks other roots. No shell wrapper, no `npx`, no environment
variables.

Besides the config file, RillDisk creates exactly two kinds of file next to it:

- a **backup**, `<name>.rilldisk-backup-YYYYMMDD-HHMMSS` (UTC), a byte-for-byte
  copy taken before every change to an existing file. Backups are never
  deleted automatically.
- a short-lived temporary file, `.<name>.rilldisk-<pid>.tmp`, which is
  renamed over the original (an atomic replace on the same volume). It is
  removed if anything fails.

A missing config file is created only when the client is installed (its folder
or executable exists) and the user asked to connect it.

## How a change is made

- **Only the `rilldisk` entry** is added, updated or removed. Other servers,
  keys, comments, ordering, indentation, trailing commas and line endings are
  preserved: JSON/JSONC files are edited through a concrete syntax tree
  (`jsonc-parser`) and TOML through `toml_edit`. Updating an existing entry
  sets `command`, `args` and `type` and keeps the user's other keys (`env`,
  `trust`, timeouts).
- **Preview first.** Every change is computed in memory and shown as a unified
  diff: `--dry-run` in the CLI, or an inline preview with Apply/Cancel in the
  desktop. Nothing is written before that.
- **Consent.** The desktop writes only after Apply is clicked. The CLI asks
  `Apply these changes? [y/N]` and refuses without a terminal unless `--yes` is
  given. `--all` only selects installed clients; it doesn't skip the preview
  or the question.
- **No lost updates.** Apply re-reads the file and refuses if it changed since
  the preview.
- **Unreadable files are left alone.** A config that doesn't parse, or whose
  top level or container key isn't an object/table, is reported and never
  rewritten.
- **Claude Code's own CLI.** When `claude` is on `PATH`, RillDisk runs
  `claude mcp add --scope user rilldisk -- <exe> mcp --root <home>` (preceded by
  `claude mcp remove --scope user rilldisk` when an entry exists) instead of
  editing `~/.claude.json`, which Claude Code rewrites itself. The preview shows
  that command and the expected change, and the backup is still taken.

## Processes RillDisk starts

- The **connection check** starts the command and arguments read from the
  client's config, with the `env` the entry declares. It writes
  `initialize`, `notifications/initialized` and `tools/list` to stdin, reads
  the answers from stdout with a timeout (10 s by default, `--timeout` in the
  CLI), then kills the process and waits for it. On Windows it uses
  `CREATE_NO_WINDOW`. It reports OK with the server name, tool count and
  latency, or one of: executable missing, could not start, exited (with the
  last stderr line), timeout, protocol error. `doctor` checks only entries that
  exist (configured or stale).
- `claude mcp add` / `claude mcp remove`, as above, only after consent.

Nothing else is started. No shell is used: arguments go straight to the
program (Rust's `Command`).

## What is never done

- No other client's settings and no other MCP server entries are changed.
- No files are deleted. Remove deletes only the `rilldisk` key; backups and
  configs stay.
- No elevation, no network access, no telemetry, no listener.
- Nothing happens automatically: the desktop only *detects* on its own; the CLI
  `doctor` only reads and checks.
- No secrets or environment variables are written into client configs.
- Tests never touch real configs: they build an isolated environment (temporary
  home, app data and `PATH`, with `CLAUDE_CONFIG_DIR`/`CODEX_HOME` removed).

## Known limits

- Claude Desktop from the Microsoft Store (MSIX) keeps its config in a
  virtualized folder under `%LOCALAPPDATA%\Packages`; only the standard
  `%APPDATA%\Claude` location is supported.
- VS Code Insiders, VSCodium, Cline installed inside Cursor or Windsurf, and
  Windsurf's successor (Devin Desktop, `~/.config/devin/mcp_config.json`) are
  not covered yet.
- After a TOML edit, a Codex config with mixed line endings comes back all
  CRLF if any line used CRLF.
- Clients load MCP servers at start-up. After connecting, restart the client
  (or reload its MCP servers).
