"""Scanner benchmark harness. Synthetic by default; never inventories personal data.

Builds nothing: point it at the release examples built with

    cargo build --release -p rilldisk-core --examples

Then:

    python scripts/benchmark.py target/release/examples            # 1M-file fixture
    python scripts/benchmark.py target/release/examples --files 200000 --runs 5
    python scripts/benchmark.py target/release/examples --path D:\\some\\tree   # a real tree
    python scripts/benchmark.py target/release/examples --ntfs-image fx.img     # MFT parser, no admin

Every backend runs in a fresh process per run (peak memory is that scan's own);
the order rotates between runs so cache warm-up is not credited to one backend.
Each run reports wall time around scan(), files per second and peak memory.
Only counts and timings are printed, never names.

Correctness gate (printed every time; `--require-identical` exits 1 unless it
passes). Every requested run must succeed. All totals are compared: files,
directories, logical, unique and allocated bytes (unknown allocation is its own
value), hard-link duplicates, links and mounts not followed, issue counts.
Each check ends in one of:

  IDENTICAL                    every run of every backend reports the same value
  VALIDATED DIFFERING COVERAGE backends read different folders and each matches
                               the generator's manifest for what it read. Shown,
                               but not a pass: when elevated, every backend runs
                               with backup privilege so all of them read the
                               locked folder and must be IDENTICAL
  VALIDATED EXCEPTION          NTFS directory listings report the allocation
                               cached in each folder entry, which lags some files'
                               MFT records (it can change from run to run) and is
                               0 for WOF-compressed files. Accepted only for the
                               Windows walks, only against the allocation that
                               --verify-allocation measured file by file, and only
                               within 0.25 % of it; the deviation is printed.
                               Backends that read records (the MFT) and the Unix
                               walks must match the truth exactly
  UNVERIFIED                   a difference that could be the exception above, but
                               no ground truth was measured
  MISMATCH / FAILED            anything else, or a requested run that failed

Cache state: runs after the first are warm. `--cold-first` keeps the first run of
each backend separate, which is only meaningful on a tree nothing has read since
boot (each backend then needs its own such tree); Windows offers no unprivileged
way to drop the file cache.
"""
import argparse
import json
import os
import pathlib
import platform
import shutil
import stat
import statistics
import subprocess
import sys
import tempfile
import time

COUNTS = ["files", "directories", "logical_bytes", "unique_logical_bytes",
          "hard_link_duplicates", "skipped_links", "skipped_mounts", "issue_count"]
TOTALS = COUNTS + ["allocated_bytes"]
# Walks whose allocation comes from NTFS folder entries.
LISTING_ALLOCATION = ("nt-directory", "win32-batched-directory")
# Largest accepted deviation of listing-derived allocation from the truth.
LISTING_TOLERANCE = 0.0025


def exe(directory, name):
    path = pathlib.Path(directory) / (name + (".exe" if os.name == "nt" else ""))
    if not path.exists():
        sys.exit(f"missing {path}; build with: cargo build --release -p rilldisk-core --examples")
    return str(path)


def elevated():
    if os.name == "nt":
        import ctypes
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    return os.geteuid() == 0


def run(bench, target, backend, threads, image=False, backup=False):
    args = [bench] + (["--ntfs-image", str(target)] if image else [str(target), "--backend", backend])
    if backup:
        args.append("--backup-privilege")
    if threads:
        args += ["--threads", str(threads)]
    started = time.perf_counter()
    result = subprocess.run(args, capture_output=True, text=True, encoding="utf-8")
    if os.environ.get("RILLDISK_TRACE_PHASES") and result.stderr:
        print(result.stderr.rstrip(), file=sys.stderr)
    if result.returncode != 0:
        return {"requested_backend": backend, "error": (result.stderr or result.stdout).strip()[-400:]}
    report = json.loads(result.stdout.strip().splitlines()[-1])
    report["requested_backend"] = backend
    report["process_ms"] = round((time.perf_counter() - started) * 1000, 1)
    return report


def flush(target):
    """Make the filesystem write what it holds in memory (the MFT backend reads
    the table from disk). On Windows this needs administrator rights."""
    if os.name != "nt":
        os.sync()
        return
    drive = os.path.splitdrive(os.path.abspath(target))[0]
    try:
        with open(r"\\.\{}".format(drive), "r+b", buffering=0) as volume:
            os.fsync(volume.fileno())
    except OSError as error:
        print(f"cannot flush {drive}: {error}", file=sys.stderr)


def true_allocation(root):
    """Allocated bytes of every readable file, measured from each file's own
    handle (Windows) or lstat (Unix), each file identity counted once, links and
    reparse points not followed, data smaller than a cluster (inside the MFT
    record) counted as zero. Returns (bytes, unreadable folders)."""
    seen, total, unreadable = set(), 0, 0
    query = cluster = None
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.CreateFileW.restype = wintypes.HANDLE
        kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                                       wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]

        class Standard(ctypes.Structure):
            _fields_ = [("allocation", ctypes.c_longlong), ("end", ctypes.c_longlong),
                        ("links", wintypes.DWORD), ("delete", ctypes.c_bool), ("directory", ctypes.c_bool)]

        drive = os.path.splitdrive(plain(root))[0] + "\\"
        sectors, size, free, count = (wintypes.DWORD() for _ in range(4))
        kernel.GetDiskFreeSpaceW(drive, ctypes.byref(sectors), ctypes.byref(size), ctypes.byref(free),
                                 ctypes.byref(count))
        cluster = sectors.value * size.value
        # Below one cluster means "inside the MFT record" only when a cluster is at
        # least as large as a record (4 KiB covers both 1 KiB and 4 KiB records).
        if cluster < 4096:
            raise SystemExit(f"--verify-allocation needs clusters of at least 4 KiB (this volume: {cluster} bytes)")

        def query(path):
            handle = kernel.CreateFileW(verbatim(path), 0x80, 7, None, 3,
                                        0x02000000 | 0x00200000, None)
            if handle in (None, wintypes.HANDLE(-1).value):
                raise OSError(ctypes.get_last_error(), path)
            info = Standard()
            ok = kernel.GetFileInformationByHandleEx(wintypes.HANDLE(handle), 1, ctypes.byref(info),
                                                     ctypes.sizeof(info))
            kernel.CloseHandle(wintypes.HANDLE(handle))
            if not ok:
                raise OSError(ctypes.get_last_error(), path)
            return info.allocation if info.allocation >= cluster else 0
    stack = [plain(root)]
    while stack:
        folder = stack.pop()
        try:
            entries = list(os.scandir(verbatim(folder) if os.name == "nt" else folder))
        except OSError:
            unreadable += 1
            continue
        for entry in entries:
            path = os.path.join(folder, entry.name)
            info = os.stat(entry.path, follow_symlinks=False)
            if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
                continue
            if stat.S_ISDIR(info.st_mode):
                stack.append(path)
            elif stat.S_ISREG(info.st_mode):
                key = (info.st_dev, info.st_ino)
                if key in seen:
                    continue
                seen.add(key)
                total += query(entry.path) if query else info.st_blocks * 512
    return total, unreadable


def plain(path):
    r"""An absolute path without a `\\?\` prefix (the form os.path works with)."""
    path = os.path.abspath(str(path))
    if path.startswith("\\\\?\\UNC\\"):
        return "\\\\" + path[8:]
    if path.startswith("\\\\?\\"):
        return path[4:]
    return path


def verbatim(path):
    r"""The `\\?\` (or `\\?\UNC\`) form, so paths beyond MAX_PATH open."""
    path = str(path)
    if path.startswith("\\\\?\\"):
        return path
    path = plain(path)
    if path.startswith("\\\\"):
        return "\\\\?\\UNC\\" + path[2:]
    return "\\\\?\\" + path


def median(values):
    return statistics.median(values) if values else None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("examples", help="directory holding scan_bench and make_fixture")
    parser.add_argument("--files", type=int, default=1_000_000, help="synthetic fixture size")
    parser.add_argument("--fixture", help="reuse an existing make_fixture directory")
    parser.add_argument("--keep", action="store_true", help="keep the generated fixture")
    parser.add_argument("--path", help="benchmark a real tree instead (counts only are printed)")
    parser.add_argument("--ntfs-image", help="also time the MFT parser on this NTFS image (reported "
                        "separately; it is a different tree)")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--threads", type=int, nargs="*", default=[0], help="0 = default")
    parser.add_argument("--backends", nargs="*", default=None,
                        help="default: legacy directory, plus mft when elevated on Windows")
    parser.add_argument("--cold-first", action="store_true")
    parser.add_argument("--json", help="write the full report here")
    parser.add_argument("--flush", action="store_true",
                        help="flush the volume first (the MFT is read from disk; Windows needs administrator rights)")
    parser.add_argument("--verify-allocation", action="store_true",
                        help="measure the true allocation file by file (one handle per file) for the gate")
    parser.add_argument("--require-identical", action="store_true",
                        help="exit 1 unless the correctness gate passes (see above)")
    args = parser.parse_args()
    bench = exe(args.examples, "scan_bench")
    backends = args.backends or (["legacy", "directory"] + (["mft"] if os.name == "nt" and elevated() else []))
    manifest = None
    created = None
    if args.path:
        target = pathlib.Path(args.path)
    elif args.fixture:
        target = pathlib.Path(args.fixture)
    else:
        created = pathlib.Path(tempfile.mkdtemp(prefix="rilldisk-bench-")) / "fixture"
        target = created
        start = time.perf_counter()
        subprocess.run([exe(args.examples, "make_fixture"), str(target), "--files", str(args.files)],
                       check=True, stdout=subprocess.DEVNULL)
        print(f"fixture: {args.files} files in {time.perf_counter() - start:.0f} s", file=sys.stderr)
    if args.flush:
        flush(target)
    sidecar = pathlib.Path(str(target) + ".manifest.json")
    if not args.path and sidecar.exists():
        manifest = json.loads(sidecar.read_text())
    # Elevated Windows: every backend reads with backup privilege, so walks and
    # the MFT cover the same folders and must agree exactly.
    backup = os.name == "nt" and elevated()
    report = {"platform": platform.platform(), "processor": platform.processor(), "cpus": os.cpu_count(),
              "elevated": elevated(), "target": "real tree" if args.path else "synthetic fixture",
              "backends": backends, "backup_privilege": backup, "manifest": manifest, "runs": [], "image_runs": [], "truth": None}
    try:
        for threads in args.threads:
            for run_index in range(args.runs):
                order = backends[run_index % len(backends):] + backends[:run_index % len(backends)]
                for backend in order:
                    result = run(bench, target, backend, threads, backup=backup)
                    result.update(run=run_index, threads_requested=threads)
                    report["runs"].append(result)
                    print(json.dumps(result), file=sys.stderr)
                if args.ntfs_image:
                    result = run(bench, args.ntfs_image, "mft-image", threads, image=True)
                    result.update(run=run_index, threads_requested=threads)
                    report["image_runs"].append(result)
                    print(json.dumps(result), file=sys.stderr)
        if args.verify_allocation:
            start = time.perf_counter()
            allocated, unreadable = true_allocation(target)
            report["truth"] = {"allocated_bytes": allocated, "unreadable_folders": unreadable}
            print(f"true allocation measured in {time.perf_counter() - start:.0f} s", file=sys.stderr)
    finally:
        if created and not args.keep:
            subprocess.run([exe(args.examples, "make_fixture"), "--remove", str(target)])
            shutil.rmtree(created.parent, ignore_errors=True)
    passed = summarize(report, args)
    if args.json:
        pathlib.Path(args.json).write_text(json.dumps(report, indent=2))
    if args.require_identical and not passed:
        sys.exit(1)


def table(rows_by_group, args):
    print("| Backend (ran) | Threads | Runs | Wall ms (each) | Median wall | Files/s (median) | Peak RSS | Peak commit |")
    print("| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: |")
    for (_, threads), rows in rows_by_group.items():
        runs = rows
        if args.cold_first and len(rows) > 1:
            cold = rows[0]
            print(f"| {cold['backend']} (first run) | {threads} | 1 | {cold['wall_ms']:.0f} | {cold['wall_ms']:.0f} | "
                  f"{cold['files_per_s']:,.0f} | {cold['peak_rss_bytes'] / 2**20:,.0f} MiB | "
                  f"{cold['peak_commit_bytes'] / 2**20:,.0f} MiB |")
            runs = rows[1:]
        walls = [r["wall_ms"] for r in runs]
        print(f"| {runs[0]['backend']} | {threads} | {len(runs)} | {', '.join(f'{w:.0f}' for w in walls)} | "
              f"{median(walls):,.0f} | {median([r['files_per_s'] for r in runs]):,.0f} | "
              f"{max(r['peak_rss_bytes'] for r in runs) / 2**20:,.0f} MiB | "
              f"{max(r['peak_commit_bytes'] for r in runs) / 2**20:,.0f} MiB |")


def summarize(report, args):
    verdicts = []

    def verdict(status, text):
        verdicts.append(status)
        print(f"  {status}: {text}")

    ok_runs = [r for r in report["runs"] if "error" not in r]
    groups = {}
    for r in ok_runs:
        groups.setdefault((r["requested_backend"], r["threads"]), []).append(r)
    print(f"\n{report['platform']}, {report['cpus']} logical CPUs, elevated={report['elevated']}, {report['target']}\n")
    if groups:
        table(groups, args)
    if report["image_runs"]:
        print("\nMFT parser on the NTFS image (a different tree; not part of the comparison):\n")
        image = {}
        for r in report["image_runs"]:
            if "error" in r:
                verdict("FAILED", f"NTFS image run {r['run']}: {r['error']}")
            else:
                image.setdefault(("image", r["threads"]), []).append(r)
        if image:
            table(image, args)

    print("\nCorrectness gate:")
    for r in report["runs"]:
        if "error" in r:
            verdict("FAILED", f"{r['requested_backend']} run {r['run']}: {r['error']}")
    for backend in report["backends"]:
        if not any(r["requested_backend"] == backend for r in ok_runs):
            verdict("FAILED", f"{backend}: no successful run")
    if not ok_runs:
        verdict("FAILED", "no successful runs")
        return False

    by_backend = {}
    for r in ok_runs:
        by_backend.setdefault(r["backend"], []).append(r)
    counts = {b: {tuple(r[k] for k in COUNTS) for r in rows} for b, rows in by_backend.items()}
    allocations = {b: sorted({r["allocated_bytes"] for r in rows}, key=lambda v: (v is None, v or 0))
                   for b, rows in by_backend.items()}
    for backend, values in counts.items():
        if len(values) > 1:
            verdict("MISMATCH", f"{backend} reported different counts in different runs: {sorted(values)}")

    # Counts across backends.
    distinct = {next(iter(v)) for v in counts.values() if len(v) == 1}
    manifest = report["manifest"]
    if len(distinct) == 1:
        verdict("IDENTICAL", "counts " + ", ".join(f"{k}={v:,}" for k, v in zip(COUNTS, next(iter(distinct)))))
    elif distinct:
        coverage = {}
        if manifest:
            hidden = manifest["unreadable"]
            full = (manifest["files"], manifest["directories"], manifest["logical_bytes"],
                    manifest["unique_logical_bytes"], manifest["hard_link_duplicates"], manifest["skipped_links"], 0, 0)
            partial = (full[0] - hidden["files"], full[1] - hidden["directories"], full[2] - hidden["logical_bytes"],
                       full[3] - hidden["logical_bytes"], full[4], full[5], 0, 1)
            for backend, values in counts.items():
                value = next(iter(values))
                coverage[backend] = "full" if value == full else "without the locked folder" if value == partial else None
        if coverage and all(coverage.values()):
            verdict("VALIDATED DIFFERING COVERAGE" if not args.require_identical else "MISMATCH",
                    "; ".join(f"{b}: matches the manifest {c}" for b, c in sorted(coverage.items())))
        else:
            verdict("MISMATCH", "backends disagree on counts: " +
                    "; ".join(f"{b}: {sorted(v)}" for b, v in sorted(counts.items())))

    # Allocation.
    truth = report["truth"]
    expected = {}
    if truth is not None:
        locked = (manifest or {}).get("unreadable", {}).get("allocated_bytes")
        for backend, values in counts.items():
            readable_all = next(iter(values))[COUNTS.index("issue_count")] == 0
            if truth["unreadable_folders"] == 0 or not readable_all:
                expected[backend] = truth["allocated_bytes"]
            elif locked is not None:
                expected[backend] = truth["allocated_bytes"] + locked
    stable_exact = True
    for backend, values in sorted(allocations.items()):
        listing = backend in LISTING_ALLOCATION
        want = expected.get(backend)
        shown = ", ".join("unknown" if v is None else f"{v:,}" for v in values)
        if want is not None and values == [want]:
            verdict("IDENTICAL", f"{backend} allocated {shown} equals the measured truth")
        elif (listing and want and None not in values
              and all(abs(v - want) <= want * LISTING_TOLERANCE for v in values)):
            spread = ", ".join(f"{(v - want) / want * 100:+.3f}%" for v in values)
            verdict("VALIDATED EXCEPTION",
                    f"{backend} allocated {shown}; truth {want:,} ({spread}, within "
                    f"{LISTING_TOLERANCE:.2%}): NTFS folder entries lag the MFT")
        elif want is not None:
            stable_exact = False
            verdict("MISMATCH", f"{backend} allocated {shown}, measured truth {want:,}")
        elif len(values) > 1:
            stable_exact = False
            verdict("UNVERIFIED" if listing else "MISMATCH",
                    f"{backend} allocated changed between runs: {shown}" +
                    (" (use --verify-allocation)" if listing else ""))
    if truth is None:
        settled = {tuple(v) for b, v in allocations.items() if len(v) == 1}
        if len(settled) > 1 and stable_exact:
            listing = any(b in LISTING_ALLOCATION for b in allocations)
            verdict("UNVERIFIED" if listing else "MISMATCH",
                    "backends disagree on allocated bytes: " +
                    "; ".join(f"{b}: {v}" for b, v in sorted(allocations.items())) +
                    (" (use --verify-allocation)" if listing else ""))
        elif len(settled) == 1 and len(allocations) > 0 and all(len(v) == 1 for v in allocations.values()):
            verdict("IDENTICAL", f"allocated {next(iter(settled))[0]} for every backend (not checked against "
                    "a measured truth)")
    passed = not any(v in ("FAILED", "MISMATCH", "UNVERIFIED") for v in verdicts)
    print(f"\nGate: {'PASSED' if passed else 'NOT PASSED'}")
    return passed


if __name__ == "__main__":
    main()
