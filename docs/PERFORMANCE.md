# Performance measurements

The Windows backend is `win32-batched-directory`. It does not read the MFT. Linux
and macOS use `unix-readdir` (see [ARCHITECTURE.md](ARCHITECTURE.md)).
Do not describe it as faster than another product without comparable measurements.

## Reproduce the synthetic benchmark

```powershell
cargo build --release --locked
python scripts/benchmark.py target/release/rilldisk.exe --files 50000 --runs 5
```

The fixture contains 50,000 one-KiB files across 256 directories. It is freshly
written, so metadata is warm; no cache eviction is performed. Each configuration
uses the same fixture and five separate processes, with 1 and 8 scanner threads.
The report includes all timings, medians, counts and both the core scanner time
and the whole process time. Process time includes startup, queries, formatting
and shutdown. Neither time includes fixture creation. Peak RAM is not measured
by this script.

Synthetic small-file timings do not predict whole-drive scan latency. Match scan
scope, privileges, reparse behavior, file counts and size semantics before
interpreting a speed difference. Record filesystem, hardware, storage, versions,
scan order, warm/cold conditions and background activity. Never publish personal
filenames or raw inventories.

## Initial local observation

November 1, 2026, source `f28860e`, release build, Windows 11 build 26200,
Core Ultra 9 285HX, Phison 2 TB NVMe, NTFS. A separate comparison harness used a
previously visited 50,000-file fixture, rotated scan order, ordinary user privileges,
and five fresh processes. Cache state and background activity were not controlled.

Core enumeration/aggregation times: **62, 32, 112, 152, 31 ms** (median **62 ms**).
Process times: **9780.08, 82.28, 247.07, 263.38, 80.19 ms** (median **247.07 ms**).
The large first-process delay occurred outside measured core enumeration; its
cause is unknown and the sample is retained. Core timing excludes root resolution,
volume setup, report formatting and process startup/shutdown.

These observations do not establish a whole-drive speed advantage. Comparisons
against installed products and live private directories are retained separately
without publishing their inventories. WizTree's elevated direct-MFT mode was not
measured. Whole-volume qualification and memory/startup profiling remain in
[issue 3](https://github.com/lord007tn/rilldisk/issues/3).

## Desktop release measurements

November 1, 2026, same hardware, release build, default threads (24 on this
machine), ordinary user privileges. The workstation was busy (WSL, Node and
several coding agents), so ranges are reported and no samples were dropped.

| Dataset | Result |
| --- | --- |
| `scripts/benchmark.py --files 50000 --runs 5`, 8 threads | scanner median 24 ms (5 runs: 23, 24, 22, 23, 25); end-to-end median 68 ms |
| Same, 1 thread | scanner median 147 ms; end-to-end median 225 ms |
| Developer workspace, 3,127,882 files / 403,769 folders, warm | 8,871 / 4,556 / 8,627 ms; peak working set 556–620 MB |
| `C:\Users`, 17,965,634 files / 1,889,086 folders | 86.6 s scanner, 92 s wall; reached the then-default 20M entry bound |
| `C:\`, 26,953,443 files / 2,889,616 folders | 177 s wall for `reclaim --format json`; 195 permission-denied folders |

Changes measured along the way, same workspace unless noted:

* Group barrier of 16 folders (first prototype design), 8 threads: 14.5 s.
* Barrier of 4,096 folders, 24 threads, volume identity read once: about 5.5 s,
  but one unresponsive folder under `C:\Users` stalled all progress for minutes
  (422 s total).
* Worker-driven pipeline (current): 4.6–8.9 s here and 92 s for `C:\Users`. A
  whole-scan stall guard stops waiting only after 60 s in which no folder result
  arrives and the entry budget does not change; outstanding folders are then
  reported.
* Peak memory per entry: about 357 bytes with full paths stored (5M entries,
  1.78 GB), about 218 bytes after storing names only (19.9M entries, 4.37 GB),
  about 160–175 bytes on the workspace above.

Still not measured: cold-cache runs after cache eviction, elevated runs, and a
like-for-like comparison with WizTree's MFT mode.

## Linux (WSL2) observation

November 1, 2026, release build, WSL2 Ubuntu (kernel 6.18, glibc 2.43) on the same
Core Ultra 9 285HX, 24 logical CPUs, ext4 on the WSL virtual disk, ordinary user,
default settings. Warm cache: the page cache was not dropped (no root), and the
machine was busy with other builds. macOS is built and tested in CI but has no
timing yet.

Synthetic benchmark (`scripts/benchmark.py`, 50,000 one-KiB files in 257 folders,
fixture on ext4 via `TMPDIR`, five fresh processes each):

| Threads | Scanner times (ms) | Median | Process median |
| ---: | --- | ---: | ---: |
| 1 | 77, 79, 86, 81, 78 | 79 ms | 92.9 ms |
| 8 | 30, 10, 14, 12, 11 | 12 ms | 22.5 ms |

With the fixture on tmpfs (`/tmp`) the 8-thread median was also 12 ms.

Real trees, `rilldisk scan ROOT --format json --output FILE`, three runs:

| Root | Files / folders | Scanner time | Process wall time | Peak RSS |
| --- | ---: | ---: | ---: | ---: |
| `/usr` | 36,589 / 4,510 | 24 ms | 0.05–0.07 s | ~14 MB |
| `/` (one filesystem) | 329,291 / 33,236 | 274 ms | 0.95–1.12 s warm; 26.8 s first run | ~74–89 MB |

Process time for `/` is dominated by writing the 360,000-entry JSON file. The first
`/` run read most of the tree for the first time since boot; it is not a controlled
cold-cache measurement.

Accounting checks on `/usr`:

* Files and folders equal `find /usr -xdev -type f` / `-type d` (36,589 / 4,510).
* Unique bytes 1,455,154,218 plus 51,917 bytes of symlink targets equal
  `du -sx --apparent-size -B1` (1,455,206,135) exactly.
* Allocated bytes 1,540,386,816 plus 19,603,456 bytes of directory blocks and 4,096
  bytes of symlink blocks equal `du -sxB1` (1,559,994,368) exactly.
* 116 hard-link duplicates; three mount points below `/usr` (WSL driver and library
  mounts) were recorded and not entered.

On `/`, a sorted path-by-path comparison with `find / -xdev -type f` differed only
in files another process was creating and deleting in a build directory during the
runs. The 27 issues were folders that an ordinary user cannot read.
