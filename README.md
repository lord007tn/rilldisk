# RillDisk

**Know your disk. Give your tools the same view.**

A Windows-first disk analyzer written in Rust, with a command line and a local
[Model Context Protocol](https://modelcontextprotocol.io/) server. Open source from
the first commit, under **AGPL-3.0-only**.

Phase one is a working **CLI and MCP foundation**. A native, GPU-rendered Rust
desktop interface is the next phase. There is no JavaScript, Tauri, Electron,
webview, embedded model, cloud account, or telemetry.

## Get started

Build on Windows x64 with [Rust stable](https://rustup.rs/), Visual Studio C++ Build
Tools and the Windows SDK. Use a Developer PowerShell if Cargo cannot find the linker.

```powershell
cargo build --release --locked
.\target\release\rilldisk.exe scan C:\Users\YourName\Downloads
.\target\release\rilldisk.exe scan C:\ --threads 8 --top 30
.\target\release\rilldisk.exe scan D:\ --format json --output disk-report.json
.\target\release\rilldisk.exe scan D:\Projects --format jsonl --output inventory.jsonl
```

Normal scans need no elevation. Protected directories appear as issues in a
partial result. Exit status **0** means complete, **2** means partial, **130** means
cancelled, and **1** means failure. Ctrl+C requests cooperative cancellation and
preserves the discovered inventory. Output files must be new: existing files are
never overwritten. Progress goes to stderr; JSON stdout stays machine readable.

## Connect an AI assistant

Launch `rilldisk mcp --root <directory>` as a stdio MCP server. Set one or more
explicit roots; repeat `--root` to allow additional directories.

```json
{
  "mcpServers": {
    "rilldisk": {
      "command": "C:\\Tools\\RillDisk\\rilldisk.exe",
      "args": ["mcp", "--root", "C:\\Users\\YourName\\Downloads"]
    }
  }
}
```

| Tool | Purpose |
| --- | --- |
| `allowed_roots` | See allowed paths and limits |
| `start_scan` | Start a background scan; receive a scan ID immediately |
| `scan_status` | Poll progress and retrieve the final summary and issues |
| `query_entries` | Browse children, find largest files, or search paths; pages of up to 500 |
| `cancel_scan` | Request cancellation and keep the partial inventory |

Example workflow: scan Downloads, poll until finished, then query
`{"scan_id":1,"files_only":true,"limit":20}`. Ask an assistant to explain what uses
space and propose actions. Phase one exposes **read-only metadata**; it cannot
delete, move, execute commands, or read file contents. See [MCP details](docs/MCP.md).

## How the scanner works

On Windows, a bounded worker pool enumerates directory metadata in **64 KiB Win32
batches** using `GetFileInformationByHandleEx`. Each batch includes names, file IDs,
logical sizes and allocation sizes. The scanner opens directories, avoiding an
extra file open/stat for every ordinary file. It never reads file contents.

The engine produces one flat tree with parent IDs. The CLI and MCP share the same
scanner and query logic, and the desktop app will reuse it.

* Hidden and system entries are included when readable.
* Junctions, symlinks and other reparse entries are listed but not traversed.
* `logical_bytes` counts every discovered filename; `unique_logical_bytes` and
  `allocated_bytes` count a hard-linked file once, using volume and file identity.
  The first encountered link gets the unique bytes; directory attribution may
  differ between scans. Sub-cluster resident allocation is reported as zero
  separately allocated data clusters, excluding its MFT storage.
* Errors, cancellation and entry limits produce explicitly partial results. The
  first 100 error details are retained; `issue_count` counts all errors.
* `--threads` accepts 1–64; the default is up to 8. `--max-entries` defaults to
  5,000,000. This bounds retained records, not total RAM in bytes.

**This is not yet an MFT scanner and has not demonstrated WizTree-level whole-drive
speed.** Direct NTFS metadata scanning is a separate milestone with correctness
gates. See [architecture](docs/ARCHITECTURE.md), [benchmarks](docs/PERFORMANCE.md),
and the [roadmap](ROADMAP.md).

## Limits to understand

The current Windows backend targets local NTFS volumes. Unsupported filesystem
information classes fail visibly; FAT/exFAT, ReFS and network shares are not yet
qualified. Reported allocation is **not total volume usage**: alternate data
streams, filesystem metadata, reserved space, directory allocation, compression,
deduplication, snapshots and concurrent filesystem changes can cause differences.
This is a live scan, not an atomic filesystem snapshot. Reparse files such as cloud
placeholders are skipped, so their sizes are not included.

An ordinary portable backend exists for tests and future development. Linux CI
checks it, but macOS/Linux releases and feature parity are later milestones.

Paths are local metadata and may be sensitive. The server itself does not send
them over a network; a connected AI client may send tool results to its provider.
The allowed-root check is an application guard, not an OS sandbox against hostile
concurrent directory changes. Run with ordinary user privileges and choose narrow
roots. JSON reports store filenames as text; non-Unicode paths may be lossy.

## Development

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked
python scripts/test_mcp.py target/release/rilldisk.exe
python scripts/benchmark.py target/release/rilldisk.exe
```

Python is only used for development acceptance tests and benchmark fixtures. The
distributed program is a Rust executable. CI checks Windows and Linux and never
builds Docker images. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Copyright © 2026 Raed Bahri and RillDisk contributors. Licensed under
[GNU Affero General Public License v3.0 only](LICENSE), consistent with the
maintainer's other AGPL projects. Contributions use the same license.
