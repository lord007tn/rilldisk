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

## Measurement status

Local product comparisons and benchmark research are scheduled after development
validation. Results will be recorded separately with their actual scope and limits.
