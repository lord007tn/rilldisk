# Scanner performance and accounting investigation, 2026-11-02

Base: `e6c6814f67c60e412eb1bd1256a84602f124460a`. Scope: core and benchmark
harness only. These measurements use fictional data, not a personal inventory.
The reported six-minute drive scan was not reproduced or profiled here.

## Change and measurements

Windows directory record decoding previously allocated a UTF-16 vector for
every record, then copied the name into an owned `OsString`. It now reuses one
scratch vector per listing buffer. Directory tasks still copy the name before
the buffer is reused. Validation, record layouts, identities, byte accounting,
entry limits, cancellation and reparse checks are unchanged. Both directory
backends share this parser. No dependencies, scanner options or privileges added.

Windows 11 build 26200, 24 logical CPUs, unelevated, Rust 1.99.1, release build,
8 scan workers. Fixture seed 20261102, requested 20,000 files; readable scan:
20,088 files, 4,272 directories, 2,302 hard-link duplicates and one skipped link.
The generator intentionally denies one directory: one issue and `complete=false`
are expected. Fixture construction took 10.2 seconds. Runs are warm; no cache
flush or cold-cache claim. Nine fresh processes per version, alternating
before/after order each repetition. No other measurement jobs ran concurrently
within this task; the workstation was not isolated from other activity.

| Workload | Before median ms (range) | After median ms (range) | Interpretation |
| --- | ---: | ---: | --- |
| Decode 2 million in-memory file records | 187.289 (174.536–204.243) | 154.316 (138.222–160.389) | 17.6% less parser time |
| Directory scan | 40.0 (36.8–44.7) | 39.5 (37.9–48.7) | Overlapping ranges; no demonstrated scan speedup |
| Legacy scan | 82.4 (78.7–94.8) | 86.6 (79.9–93.2) | Median 5.1% slower, overlapping ranges; no speedup claim |

The CPU microbenchmark uses a repeated ASCII filename without disk I/O. It is
an isolated measure, not a prediction for an entire drive. At these medians the
saved CPU work is about 16.5 ns/record; even 10 million records would save only
about 0.165 CPU seconds under that model. This cannot explain minutes of delay.
The change is retained for its demonstrated reduction in allocations and parser
CPU cost, not an end-to-end performance claim.

Raw samples, scanner phases, peak memory and allocation gates are in
[the JSON evidence](performance/2026-11-02-parser-reuse.json). The paired directory
runs spend median 0.5/0.6 ms before/after on rollup and 0.6/0.7 ms on the child
index. Most elapsed time is outside final aggregation: enumeration, scheduling,
collection and startup. This experiment does not separate their individual costs.

## Algorithm costs and the six-minute report

Let N be entries, D directories, S total filename code units, k(d) each folder's
child count, M the number of allocated MFT record slots and R bytes per record.

- The directory walk reads names/metadata in batches: O(N + S) decoding, plus
  O(D + listing-buffer count) system calls. Relative opens avoid repeatedly
  resolving full ancestor paths. Actual I/O/filter latency remains critical.
- Collection and identity deduplication are expected O(N), using a hash table
  with a dense record-number representation for sufficiently large scans.
  Live totals update at most two folder groups per file. Periodic live-view
  publication additionally sorts first/second-level groups.
- Rollup is O(N), one reverse pass. Child indexing is O(N + sum k(d) log k(d)),
  at worst O(N log N) for a single huge directory; directory lists sort in
  parallel. Names/entries/indexes require O(N + S) retained memory.
- MFT reads O(MR) bytes, even for a small subfolder, then parses records and
  indexes names. It replaces scattered directory opens with large sequential
  reads but pays for unused record slots and its record/parent/name indexes.
  It is not O(number of selected files), nor independent of volume history.

The existing historical cold/warm results in PERFORMANCE.md support cold
directory I/O as a candidate explanation for minutes versus seconds. They are
not measurements of this user's reported run. Required follow-up evidence is
the actual serving revision, `Summary.backend`, worker count, entry/directory
counts, elapsed time, issue counts, cancellation state, and warm versus first-run
timing. Summary-only evidence is sufficient; private paths are unnecessary.

## Auto and elevation audit

Source: `scan`, Windows `mft_source`, `MftSource::open`, and `emit_mft`.

- Auto checks the root's file reference; only NTFS root record 5 is eligible.
  Subfolders use the directory walk. Explicit MFT may read an entire volume to
  answer a subfolder request.
- Raw volume open needs existing rights. Auto falls back if opening fails or
  the MFT scan returns an error while not cancelled. It resets the collector
  and progress first; failed MFT totals cannot be added to directory totals.
  The fallback reason is not retained in the successful summary. Backend and
  elapsed time alone cannot distinguish a failed MFT attempt from a direct walk.
- MFT success with reported issues remains partial and does not force fallback.
  Cancellation is not retried. Explicit MFT failures return errors.
- Core never starts UAC or relaunches the process. The existing backup-privilege
  helper only enables a privilege on an already elevated token when a caller
  invokes it. Nothing in this patch changes that boundary.

Folder Auto fallback was exercised by benchmark and backend tests. Real elevated
whole-volume access and failure injection after an MFT read were not exercised.
The synthetic NTFS-image tests passed (parsing, accounting, budget, cancellation).

## Explorer 1.83 TB versus RillDisk 2.03 TB

The desktop formatter uses binary units (dividing by 1024), with Explorer-style
KB/MB/GB/TB labels. The CLI explicitly uses KiB/MiB/GiB/TiB. Therefore a decimal
versus binary conversion does **not** explain the user's desktop discrepancy.
At the displayed central values, 2.03 - 1.83 = 0.20 TiB, approximately
219,902,325,555 bytes. Exact bytes, metric and coverage must be compared.
The initial core-only investigation assumed decimal display without inspecting
the desktop formatter; integration corrected that assumption.

Core metrics have distinct meanings:

- Logical: unnamed-stream lengths per name, including every hard-link name.
- Unique logical: the same lengths counted once per observed file identity.
- Allocated: filesystem-reported allocation counted once per identity. NTFS
  resident data has no separate clusters; sparse/compressed data may allocate
  much less than logical length. Directory listings can lag current allocation,
  and WOF listings can report zero; MFT includes the WOF storage stream.
- Directory allocation sums scanned files, not all volume consumption. It does
  not account for all NTFS metadata, directory indexes, all alternate streams,
  snapshots, inaccessible or skipped trees. It also is not a universal physical
  block-sharing/reclaimability measurement. Unknown allocation remains `None`;
  partial totals retain their issues/completeness status.

Explorer file/folder "Size on disk" is itself an estimate using file attributes,
compression/sparsity and cluster rounding, rather than a volume reconciliation:
[Microsoft's explanation](https://devblogs.microsoft.com/oldnewthing/20160427-00/?p=93365).
Microsoft also distinguishes sparse allocation from nominal file size in
[its sparse-file documentation](https://learn.microsoft.com/en-us/windows/win32/fileio/obtaining-the-size-of-a-sparse-file).
Drive Properties used space is another metric: volume capacity minus free space.
Do not compare it directly with the scanner's sum of logical file lengths.

The fixture illustrates the distinction: logical 1,276,233,999 bytes, unique
logical 1,270,558,382 bytes, file-handle allocation 215,822,336 bytes over readable
folders. Initial baseline allocation gate without ground truth was UNVERIFIED;
it was not accepted as identical. With `--verify-allocation`, baseline and after
gates passed the existing 0.25% listing exception. Baseline listing deviation
was -0.082% to -0.032%; after was -0.027%. This tolerance was not introduced or
widened by this change and is not permission to ignore a real-drive discrepancy.

## Reproduction and validation

```
cargo build --release -p rilldisk-core --examples
make_fixture PATH --files 20000
python scripts/benchmark.py target/release/examples --fixture PATH --backends auto directory legacy --runs 5 --threads 8 --verify-allocation --require-identical
cargo test --release -p rilldisk-core --lib directory_record_benchmark -- --ignored --nocapture
```

Preserve baseline binaries before applying the parser change and alternate them
with rebuilt binaries against the same fixture. The harness default is now
20,000 requested files; million-file fixtures require explicit `--files` opt-in.

Passed: core tests (68 passed, one opt-in timing test ignored by default), release
timing test, core all-target Clippy with warnings denied, core formatting, Python
compilation, and before/after allocation verification. Tests include hard links,
partial-result contracts in the existing scanner suite, native buffer boundaries,
entry limits, cancellation, Unicode and invalid UTF-16 name preservation. Final
test-helper lint fix was followed by Clippy and the five Windows unit tests.
Workspace and real stdio MCP acceptance remain the integrating parent's gates.

AI-slop cleanup reviewed the task-owned diff against the base above: one scratch
buffer replaces temporary allocations; no new abstraction or success fallback.
Corrected the misleading Auto fallback wording in PERFORMANCE.md; the source comment still says "before entries" although the error branch resets unconditionally when not cancelled. No further
actionable cleanup findings. No desktop, workspace, release, commits or PR edits.

## Desktop integration (v0.1.1)

Live views now retain every directory, with O(1) own-counter updates per file.
Publication rolls up in O(D), then sorts the focused directory's immediate
folders and bounded child previews. The tree requires O(D + directory-name
bytes) additional storage; files are not duplicated. UI publication backs off
based on the previous publication cost (20 times that cost, minimum 250 ms),
targeting approximately 5% overhead under stable workload. This is a cadence
policy, not a measured CPU guarantee. The list renders only visible rows and
the map draws at most 48 top folders. The complete list remains navigable.

Root totals no longer omit folders beyond the old 48-folder publication cap.
Deep navigation, direct-file totals and hard-link allocation have regression
coverage. Live tiles process actual egui pointer press/release events in tests.
Completed-chart navigation now switches to Explore so breadcrumbs and Parent
operate on the same folder as the chart. Backspace is exercised by the native
capture tour, which asserts entering Projects and returning to the root.

The desktop retains binary Explorer-style size formatting. It now shows logical,
unique and allocated scan totals separately, exact bytes on hover, and volume
used/free/capacity for whole-drive scans. Volume values are explicitly labeled as
startup information. The scan progress number is explicitly labeled logical.
Auto backend status now exposes an MFT fallback reason while scanning.

Standalone desktop build verification found a pre-existing dependency feature
error: core imported Windows Threading APIs only enabled by dev-dependencies.
The production dependency now enables the required feature explicitly.

The scan_bench example accepts `--analyze` to separately time reclaim discovery,
file types and largest-file selection, reporting counts/times on stderr and
retaining the usual one-object scanner JSON on stdout. This helps distinguish
scanner time from the desktop analysis phase without exporting file names.

Integration cleanup review covered the task-owned changes from the base above.
Removed the capture helper's delayed synthetic release (PNG capture could turn
it into a long press), preserved existing pending navigation on scan completion,
and fixed stale documentation/units assumptions. Local Windows workspace tests,
Clippy and real stdio MCP acceptance passed; native fixture recording assertions
passed. Cross-platform CI and release-package acceptance remain separate gates.
