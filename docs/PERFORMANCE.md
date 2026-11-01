# Performance measurements

The phase-one backend is `win32-batched-directory`. It does not read the MFT.
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

* Group barrier of 16 folders (v0.1.0-alpha.1 design), 8 threads: 14.5 s.
* Barrier of 4,096 folders, 24 threads, volume identity read once: about 5.5 s,
  but one unresponsive folder under `C:\Users` stalled all progress for minutes
  (422 s total).
* Worker-driven pipeline (current): 4.6–8.9 s here and 92 s for `C:\Users`, with a
  60 s per-folder stall guard.
* Peak memory per entry: about 357 bytes with full paths stored (5M entries,
  1.78 GB), about 218 bytes after storing names only (19.9M entries, 4.37 GB),
  about 160–175 bytes on the workspace above.

Still not measured: cold-cache runs after cache eviction, elevated runs, and a
like-for-like comparison with WizTree's MFT mode.
