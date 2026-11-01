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
