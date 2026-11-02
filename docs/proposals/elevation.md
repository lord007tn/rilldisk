# Proposal: reading protected folders (elevation)

Status: implemented, November 2, 2026, in the core (`rilldisk_core::elevation`,
`enable_backup_privilege`, issue kinds), the CLI (hint and JSON fields) and the
desktop (restart offer, Administrator badge, Issues grouped by kind).

AGENTS.md requires a scoped proposal before RillDisk uses elevation. This document
is that scope. Anything not described here is out of scope.

## Problem

A normal-user scan of `C:\` reports about 210 "Access is denied (os error 5)"
issues: `System Volume Information`, `ProgramData\Microsoft\Windows\WER`, parts of
`Windows\`, other users' profiles and similar folders. The result is correctly
marked partial, but the totals miss that data and nothing tells the user how to
get it. Linux has the same problem for `/root`, `/var/lib/*` and other users'
homes. macOS adds privacy protection (TCC), which refuses even root until the
app has Full Disk Access.

## Scope

1. **Issue kinds.** Every `ScanIssue` gets a machine-readable `kind`
   (`access_denied`, `not_found`, `in_use`, `stalled`, `entry_limit`, `other`) and
   the raw `os_error` when there is one. `message` is unchanged. The summary
   gains `access_denied_count` (counted over every issue, not only the 100 listed)
   and `elevated`. The reclaim report repeats `issue_count`,
   `access_denied_count` and `elevated`. All fields are additive; the schema
   version stays 2.
2. **Detection.** `rilldisk_core::elevation()` returns `is_elevated`,
   `can_elevate` and `backup_privilege_enabled`. Windows reads `TokenElevation`
   and checks `SeBackupPrivilege` with `PrivilegeCheck`. Unix compares the
   effective UID with 0 and looks for `sudo` or `pkexec` on `PATH`. It is public
   so other backends (such as a direct NTFS reader that needs administrator rights)
   reuse it instead of probing on their own.
3. **Backup privilege (Windows).** `rilldisk_core::enable_backup_privilege()`
   flips the *enabled* flag of `SeBackupPrivilege` on the process token when the
   token is already elevated. Directory handles are already opened with
   `FILE_FLAG_BACKUP_SEMANTICS` (required for directory handles), so with the
   privilege enabled the existing read-only open
   (`FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES`) bypasses the folder's read ACL.
   The CLI `scan`/`reclaim` commands and the desktop call it at start. The MCP
   server never does.
4. **CLI hint.** When access was denied and the process is not elevated, table
   output ends with one line: Windows "run from an elevated terminal (Run as
   administrator) to read N protected folders"; Linux `sudo rilldisk …` with the
   same arguments; macOS the same plus a Full Disk Access note (the note alone
   when already root). JSON formats print it on stderr. The CLI never elevates or
   relaunches itself.
5. **Desktop restart.** When access was denied and the app is not elevated, the
   header and the top of the Issues tab offer a restart. Every route starts from
   a click:
   * **Windows:** `ShellExecuteExW` with the verb `runas` on the app's own
     executable (`current_exe`), with the scan root and the current theme as
     arguments, so the elevated instance rescans the same folder. The current
     window closes only when the call succeeds, which means the user accepted the
     UAC prompt and the process started. If the user declines
     (`ERROR_CANCELLED`), a quiet note says nothing changed.
   * **Linux:** if `pkexec` is on `PATH`, run
     `pkexec env DISPLAY=… XAUTHORITY=… WAYLAND_DISPLAY=… XDG_RUNTIME_DIR=… <exe> <root>`
     (only the variables that are set). The window stays open; a declined or
     failed prompt (exit 126/127) shows the same note. Without `pkexec`, the app
     shows a `sudo --preserve-env=… <exe> <root>` command to copy.
   * **macOS:** when a folder fails with `EPERM` (privacy protection), offer
     **Grant Full Disk Access**, which opens
     `x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles`
     and explains to quit, reopen and rescan. RillDisk does not relaunch itself as
     root on macOS.
   An elevated app shows an **Administrator** badge in the header.
6. **Issues view.** Issues are grouped by kind, access denied first, with the
   offer at the top.

## What runs elevated

Only a RillDisk executable the user chose to restart through the OS consent UI,
or a CLI the user started from an elevated terminal or with `sudo`. Elevated, it
does exactly what it does unelevated: list directory metadata and show it. With
the backup privilege it can list folders whose ACLs deny the user.

## What never happens

* No automatic elevation, from any program. No elevation prompt without a click.
* No ACL changes, no taking ownership, no `SeRestorePrivilege`,
  `SeTakeOwnershipPrivilege` or `SeSecurityPrivilege`, no writes of any kind.
* No file contents are read. The backup privilege is used only through the
  existing directory-listing open; files are never opened.
* No elevated helper, service, scheduled task or persistent setting. Closing the
  elevated window ends elevation.
* The MCP server stays unprivileged: it never calls `enable_backup_privilege`,
  never relaunches and offers no elevation tool. Running it elevated is still
  discouraged (SECURITY.md).
* No shell. The restart and settings routes start fixed programs (`ShellExecuteExW`
  on our own executable, `pkexec`, `open`) with fixed arguments plus the scan
  root as one argument. Nothing is interpolated into a command line that a shell
  parses; the Windows argument is quoted for `CommandLineToArgvW` and unit-tested,
  and the copyable Linux command is single-quoted.
* No network, telemetry or other behaviour changes.

## Threat model

* **Path injection into the elevated instance.** The only data passed is the scan
  root, which the user chose, canonicalized and quoted as one argument. The
  elevated instance treats it like any `PATH` argument: a folder to list.
* **A tampered executable.** `current_exe` is the binary already running. If it
  can be replaced, the attacker already controls the user's session; UAC still
  shows the publisher (unsigned today) and path, and the user decides.
* **Confused deputy via MCP.** An AI client cannot reach elevation: the MCP
  server neither enables the privilege nor exposes a restart.
* **Information exposure.** An elevated scan shows names and sizes from other
  users' profiles and system folders to whoever runs it. That person is an
  administrator who could read them anyway. Scan exports (`--output`) may contain
  those names; AGENTS.md already forbids committing private inventories.
* **Privilege lingering.** The privilege lives only in the elevated process's
  token and ends with it.

## Testing

* Unit tests classify raw OS errors per platform (Windows 5, 1314, 2, 3, 32, 33
  and 1 as "other"; Unix 1, 13, 2, 20, 16 and 5 as "other") and the portable
  `io::ErrorKind`s.
* A Windows test denies the current user `RD` on a temporary folder with
  `icacls`, scans it and expects one `access_denied` issue with `os_error` 5.
  When the test process is elevated (GitHub's Windows runners), it then enables
  the backup privilege and expects the same folder to be read completely. The ACE
  is removed before asserting. The test skips if the ACE cannot be set.
* The desktop tests the `runas` argument quoting (`C:\` must become `"C:\\"`),
  the `pkexec` command (only set display variables) and when each offer appears.
  No test triggers a real UAC, polkit or settings prompt.
* `scripts/test_mcp.py` keeps exercising the unprivileged MCP server.
