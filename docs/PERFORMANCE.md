# Performance measurements

RillDisk has several enumeration backends behind one collector, so every backend
reports the same accounting (see [ARCHITECTURE.md](ARCHITECTURE.md#scan-backends)).
`Summary.backend` names the one that ran; `--backend auto|mft|dir` chooses.

See [the bounded parser/accounting investigation](PERFORMANCE-INVESTIGATION.md)
for the November 2 follow-up, paired samples, and limits on the reported
six-minute scan and Explorer size discrepancy.

| Backend | Where | How |
| --- | --- | --- |
| `ntfs-mft` | Windows, NTFS, administrator | Reads the Master File Table raw, in parallel 4 MiB blocks |
| `nt-directory` | Windows (default walk) | `NtCreateFile` relative to the parent's handle, `NtQueryDirectoryFile` into a reused 64 KiB buffer |
| `win32-batched-directory` | Windows (`Legacy`, reference) | RillDisk 0.1: `CreateFileW` by full path, attribute check, `GetFileInformationByHandleEx` |
| `unix-readdir` | Linux, macOS (default) | `std::fs::read_dir` + one no-follow `stat` per file and folder |
| `linux-getdents` / `unix-openat` | Linux / macOS (`--backend dir`) | `openat` relative to the parent descriptor, `getdents64` (Linux) or `readdir`, `fstatat` |

`auto` uses `ntfs-mft` only for a **whole NTFS volume** when the process can
read it (administrator), and falls back to `nt-directory` whenever that is not
possible or the MFT attempt returns an error without cancellation. The failed
attempt's collector and progress are discarded before the walk. A folder never uses the MFT under
`auto`: the backend reads the whole volume's table, which costs more than walking
a small folder (measured below). `--backend mft` forces it for a large folder.

Do not describe RillDisk as faster than another product without comparable
measurements. Never publish personal filenames or raw inventories.

## Recommendation

* **Whole NTFS drive, elevated:** the MFT. On the CI runner's system drive it
  took 4.3–5.0 s against 310–403 s for a cold directory walk (3.9–6.1 s once
  warm), with identical counts (table below). It does not depend on the cache,
  which is what a first scan of a large drive needs. It reads each file's MFT record, so allocation is exact where
  directory listings are not (see "Listings lag the MFT").
* **Windows otherwise (not elevated, a folder, ReFS, FAT, network):**
  `nt-directory`. Warm, it is 6–16 % faster than 0.1's walk on real developer
  trees and ~10 % on the 1M-file fixture; it removes a per-folder attribute
  query, path rebuilding and a zeroed 64 KiB allocation per folder, and reads
  128-bit file IDs (ReFS). The listing APIs themselves are within ~15 % of each
  other: the per-folder kernel cost (~63 µs per folder single-threaded, warm)
  sets the floor for any directory walk.
* **Linux and macOS:** keep `std::fs::read_dir` (`unix-readdir`). The
  descriptor-relative walk was 11 % faster on the GitHub Linux runner but not
  faster in WSL2 (warm equal, cold median 8.6 s vs 7.6 s), and much slower on
  the (noisy) macOS runner. It stays available as `--backend dir`.
* **Cold caches dominate real-drive time on Windows.** A first scan of a real
  600k-file tree took 4.1 s, the second 0.63 s; the runner's whole drive took
  321 s cold and 6 s warm with the directory walk. Only the MFT avoids that:
  it streams the table instead of opening every folder.

## Where the time goes (profiling)

All Windows numbers on this page, unless marked CI: Core Ultra 9 285HX (8 P + 16 E
cores, 24 threads), 64 GB RAM, Phison 2 TB NVMe, NTFS (4 KiB clusters), Windows 11
Pro build 26200, Defender real-time protection on, **not elevated**, release
build, a busy workstation (other agents, WSL). November 2, 2026.

**Listing APIs** (`examples/list_bench`: a bare parallel walk that only counts,
no collector; 1M-file fixture, 169,241 folders, warm; median of 3 runs, ms):

| Strategy (24 threads) | 64 KiB | 256 KiB | 1 MiB |
| --- | ---: | ---: | ---: |
| 0.1: `CreateFileW` + attribute query + `FileIdBothDirectoryInfo`, fresh zeroed buffer | 1,308 | | |
| same, buffer reused | 1,206 | | |
| `CreateFileW` path, no attribute query | 1,362 | | |
| `FindFirstFileExW` (`FindExInfoBasic`, `LARGE_FETCH`) | 1,455 | | |
| NT path open, `FileIdBothDirectoryInformation` | 1,301 | 1,257 | 1,407 |
| NT path open, `FileIdFullDirectoryInformation` | 1,228 | 1,180 | 1,222 |
| NT path open, `FileIdExtdDirectoryInformation` | 1,252 | 1,103 | 1,346 |
| NT **relative** open, `FileIdBothDirectoryInformation` | 1,274 | 1,359 | 1,339 |
| NT relative open, `FileIdFullDirectoryInformation` | 1,267 | 1,209 | 1,262 |
| NT relative open, `FileIdExtdDirectoryInformation` (chosen) | **1,111** | 1,190 | 1,273 |

Before the buffer was made per-thread, a fresh zeroed 1 MiB buffer per folder
took 14.3 s (8 threads): allocation, not listing, dominated. Thread scaling of
the bare walk (ms): 1 thread 10,721 (relative) / 11,506 (path) / 13,513 (0.1);
4 threads 3,413 / 3,006 / 3,918; 12 threads 1,442 / 1,380 / 1,855; 24 and 64
threads ~1,100–1,300. 10.7 s / 169,241 folders is **~63 µs of kernel time per
folder** with one thread, warm: four system calls (open, two queries, close)
through the filter stack.

**Full scans** (`examples/scan_bench`, same fixture, `nt-directory`, warm,
median of 3, ms): 8 threads 2,934; 16 → 1,816; 24 → 2,167; 48 → 1,892; 64 →
2,039. More threads than cores did not help warm. Cold behaviour could not be
isolated without administrator rights (see "Cold caches"), so the default
(available cores, clamped to 8–32) is unchanged.

**After the listing** (`RILLDISK_TRACE_PHASES=1`, 1.17M entries): rollup
26–58 ms, child index (now sorted in parallel) 52–78 ms.

**MFT backend phases** (the 1M-file NTFS image below, cached; ms):

| Phase | Sequential emit (first version) | Parallel build (landed) |
| --- | ---: | ---: |
| Read and parse 1.17M records (24 threads) | 799–1,382 | 672–1,776 |
| Merge extension records, filter, sort names | 100–120 | 90–120 |
| Plan the tree (ids, budget, hard links) | 919–1,003 (one step) | 104–137 |
| Build entries (names, sizes) | | 105–213 |
| Total `scan()` | 2,324–2,643 | 1,080–1,525 |

Planning is sequential on indexes only; entries are built in parallel and then
pass one at a time through `Collector::accept`, the single place every backend
retains an entry (where the live view is fed).

**Memory:** the hard-link table no longer allocates a slot per MFT record
number for small scans: a 20k-file folder on this drive peaked at 276 MiB RSS
before, 17 MiB after. It switches to the dense table once that is smaller than
the hash map (millions of files). Peak RSS for the 1M-file fixture: 312–328
MiB (walks); 483 MiB for the MFT backend on the 1.17M-record image.

## Results

### Synthetic 1M-file fixture (Windows, warm)

`python scripts/benchmark.py target/release/examples --fixture FX --runs 5`
(1,000,403 files, 169,241 folders, 119,685 hard-link duplicates, one locked
folder; backends in rotating order; the first legacy run was colder):

| Backend | Runs (ms) | Median | Files/s | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| `win32-batched-directory` (0.1) | 9,936, 2,150, 2,350, 2,092, 2,275 | 2,275 | 439,765 | 312 MiB |
| `nt-directory` | 2,058, 2,115, 2,135, 1,834, 1,992 | **2,058** | 486,230 | 328 MiB |

Counts identical across backends and runs, and equal to the generator's manifest
(minus the locked folder, reported as one issue).

### Real developer trees (Windows, warm, counts only)

Two pnpm worktrees (72 % of files are hard links), 4 runs each, rotating order:

| Tree | Files / folders | 0.1 median | `nt-directory` median |
| --- | ---: | ---: | ---: |
| A | 600,464 / 53,918 | 687 ms | **644 ms** |
| B | 226,856 / 24,841 | 237 ms | **200 ms** |

Counts identical between backends. The first (cold) runs were 5,288 and 1,795 ms.

### NTFS MFT backend

**Real NTFS volume, CI** (GitHub `windows-latest`, Windows Server 2025, 4 vCPU,
elevated; [run 41432097246](https://github.com/lord007tn/rilldisk/actions/runs/41432097246)):

Whole system drive (`--path C:\`, counts only; 2 runs each, the first walk run
cold):

| Backend | Runs (ms) | Files / folders / hard-link dups / links |
| --- | --- | --- |
| `nt-directory` | **309,720** (cold), 3,864 | 1,208,150 / 210,636 / 65,037 / 100 |
| `ntfs-mft` | 4,976, 4,345 | 1,208,150 / 210,636 / 65,037 / 100 |

Counts are identical. Logical bytes differ by ~15 MB of 167 GB: the system
changes during the scan, and listings report sizes cached in folder entries.
On a cold cache the MFT was **~65× faster**; warm, on this 4-vCPU runner, the
walk was slightly faster (3.9 s vs 4.3–5.0 s). Earlier runs: walk 321 s and
403 s cold, 4.5–6.1 s warm; MFT 4.4–8.4 s. Peak RSS 524 MiB (MFT) vs 299 MiB.

The comparison found two accounting bugs before this result, both fixed:
reparse points that filter drivers disguise in listings (WOF/CompactOS, dedup,
cloud placeholders, ProjFS: 154,889 entries had been links instead of files),
and reparse tags stored in MFT extension records of heavily hard-linked files
(9,501 entries).

**Synthetic fixture in a folder, CI** (300,094 files; `--backend mft` reads
the runner's whole table to scan one folder):

| Backend | Runs (ms) | Median |
| --- | --- | ---: |
| `win32-batched-directory` | 6,118, 1,577, 1,596 | 1,596 |
| `nt-directory` | 1,351, 1,325, 1,322 | **1,325** |
| `ntfs-mft` (whole table) | 9,047, 4,262, 4,241 | 4,262 |

All three identical counts; the MFT's allocated bytes equal the measured truth.

**1M-file NTFS image** (made with `scripts/make_ntfs_image.sh`: mkntfs + ntfs-3g
in WSL; read through the file cache, so this is parse + build speed, not disk
speed): 1,169,652 entries in 1.08–1.53 s; files, folders, logical and unique
bytes and hard links equal the fixture manifest; allocation equals ntfs-3g's
own figure for every file of at least one cluster.

### Linux (WSL2, ext4, 24 threads)

Same 1M-file fixture: warm 5 runs each, `unix-readdir` 1,596 ms vs
`linux-getdents` 1,649 ms (medians); cold (page cache dropped before every run,
7 runs each), medians 7.6 s vs 8.6 s, ranges 6.6–10.8 s vs 6.9–10.2 s. The
host's cache of the virtual disk was not controlled. Gate passed (allocation
equal to `lstat` truth). GitHub `ubuntu-latest` (4 vCPU, 300k files, warm):
438 vs 391 ms. GitHub `macos-latest` (3 vCPU, noisy): `unix-readdir` 7,479 ms
vs `unix-openat` 30,416 ms medians.

## Correctness

`scripts/benchmark.py` compares every requested backend on the same tree and
fails (`--require-identical`) unless every run succeeded and all totals agree:
files, folders, logical, unique and allocated bytes, hard-link duplicates, links
and mounts not followed, issue counts. Backends that read different folders (an
elevated MFT scan reads the fixture's locked folder; a walk reports it as one
issue) must each match the generator's manifest for what they read.
`--verify-allocation` measures every file's allocation from its own handle
(`lstat` on Unix) as an independent truth.

**Listings lag the MFT.** A directory listing on NTFS reports the size and
allocation cached in each folder entry. For some files that copy is stale (for
example a small file whose data moved out of its MFT record when hard links were
added), and which value a listing returns depends on whether NTFS has the file's
record in memory: on the 1M-file fixture **18,240 of 1,000,403 files (1.8 %)
changed allocation between two consecutive listings** of an untouched tree,
while the file's own handle reported the true value. Listings also report 0
allocated bytes for WOF-compressed files (CompactOS), whose data lives in a
hidden stream: a 900,000-byte test file occupies 61,440 bytes. The MFT backend
reads the record and the compressed stream and matches the truth exactly (CI);
the walks are reported as a validated exception with their deviation
(−0.000 % to −0.05 % on the fixtures).

**Reviewed independently.** Two GPT models (via Codex) reviewed the walks and
the MFT parser; their findings (record reuse in the hard-link table, short
final records, descriptor exhaustion on deep Unix trees, truncated attribute
lists, holes in the `$MFT` runs, partial fixup arrays, resident allocation on
512-byte clusters, cancellation inside huge folders, the MFT's memory bound, and
a too-lax benchmark gate) are fixed with tests.

**Not measured:** elevated runs on this workstation (MFT on the owner's 27M-file
drive), cold Windows caches in a controlled way (dropping the cache needs
administrator rights; a natural cold run of the 1M fixture after ~40 idle
minutes took 42 s against 1.9 s warm), macOS beyond CI, io_uring `statx`
batching and `getattrlistbulk` (not implemented).

## Reproduce

```text
cargo build --release -p rilldisk-core --examples
python scripts/benchmark.py target/release/examples                      # generates 1M files (~2.5 GB reserved)
python scripts/benchmark.py target/release/examples --files 300000 --flush --verify-allocation --require-identical
python scripts/benchmark.py target/release/examples --path D:\some\tree --backends directory mft   # counts only
python scripts/benchmark.py target/release/examples --ntfs-image fx.img  # MFT parser without admin
target/release/examples/list_bench FX --threads 1,8,24,64               # listing APIs (Windows)
RILLDISK_TRACE_PHASES=1 target/release/examples/scan_bench FX --backend directory
```

The generator (`examples/make_fixture`) is deterministic: pnpm-style
`node_modules`, a content store of hard-link targets, sparse and large files,
WOF-compressed files (Windows), a junction or symlink, Unicode and trailing dot
or space names, a path over `MAX_PATH` and a folder the user cannot list. Its
manifest of expected totals is written next to the fixture, outside it.
`make_fixture --remove FX` unlocks and deletes it. The scan-benchmark workflow
runs the same on GitHub's Windows (elevated), Linux and macOS runners for
scanner changes; results are in the job summary.

## Earlier measurements (RillDisk 0.1, `win32-batched-directory`)

Kept for history. The 0.1 `scripts/benchmark.py` they mention was replaced by the harness above.

### Initial local observation

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

### Desktop release measurements

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

### Linux (WSL2) observation

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
