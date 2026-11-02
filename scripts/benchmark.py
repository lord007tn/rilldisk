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
Each run reports wall time around scan(), files per second and peak memory, and
the harness checks that every backend produced identical totals: files,
directories, logical, unique and allocated bytes, hard-link duplicates, links not
followed and issue counts. A synthetic fixture is also checked against the
generator's manifest. Only counts and timings are printed, never names.

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
import statistics
import subprocess
import sys
import tempfile
import time

TOTALS = ["files", "directories", "logical_bytes", "unique_logical_bytes",
          "hard_link_duplicates", "skipped_links", "skipped_mounts", "issue_count"]


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


def run(bench, target, backend, threads, image=False):
    args = [bench] + (["--ntfs-image", str(target)] if image else [str(target), "--backend", backend])
    if threads:
        args += ["--threads", str(threads)]
    started = time.perf_counter()
    result = subprocess.run(args, capture_output=True, text=True, encoding="utf-8")
    if result.returncode != 0:
        return {"backend": backend, "error": (result.stderr or result.stdout).strip()[-400:]}
    report = json.loads(result.stdout.strip().splitlines()[-1])
    report["process_ms"] = round((time.perf_counter() - started) * 1000, 1)
    return report


def flush(target):
    """Make the filesystem write what it holds in memory. A read-only flush of the
    volume's caches; on Windows it needs administrator rights (as does the MFT)."""
    if os.name != "nt":
        os.sync()
        return
    drive = os.path.splitdrive(os.path.abspath(target))[0]
    try:
        with open(r"\\.\{}".format(drive), "r+b", buffering=0) as volume:
            os.fsync(volume.fileno())
    except OSError as error:
        print(f"cannot flush {drive}: {error}", file=sys.stderr)


def median(values):
    return statistics.median(values) if values else None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("examples", help="directory holding scan_bench and make_fixture")
    parser.add_argument("--files", type=int, default=1_000_000, help="synthetic fixture size")
    parser.add_argument("--fixture", help="reuse an existing make_fixture directory")
    parser.add_argument("--keep", action="store_true", help="keep the generated fixture")
    parser.add_argument("--path", help="benchmark a real tree instead (counts only are printed)")
    parser.add_argument("--ntfs-image", help="also run the MFT parser on this NTFS image file")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--threads", type=int, nargs="*", default=[0], help="0 = default")
    parser.add_argument("--backends", nargs="*", default=None,
                        help="default: legacy directory, plus mft when elevated on Windows")
    parser.add_argument("--cold-first", action="store_true")
    parser.add_argument("--json", help="write the full report here")
    parser.add_argument("--flush", action="store_true",
                        help="flush the volume after creating the fixture (the MFT is read from disk; "
                             "Windows needs administrator rights for this)")
    parser.add_argument("--require-identical", action="store_true",
                        help="exit 1 unless the totals agree (see the summary for the rules)")
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
    report = {"platform": platform.platform(), "processor": platform.processor(), "cpus": os.cpu_count(),
              "elevated": elevated(), "target": "real tree" if args.path else "synthetic fixture",
              "manifest": manifest, "runs": []}
    try:
        for threads in args.threads:
            for run_index in range(args.runs):
                order = backends[run_index % len(backends):] + backends[:run_index % len(backends)]
                for backend in order:
                    result = run(bench, target, backend, threads)
                    result.update(run=run_index, threads_requested=threads)
                    report["runs"].append(result)
                    print(json.dumps(result), file=sys.stderr)
                if args.ntfs_image:
                    result = run(bench, args.ntfs_image, "mft-image", threads, image=True)
                    result.update(run=run_index, threads_requested=threads, requested="NtfsImage")
                    report["runs"].append(result)
                    print(json.dumps(result), file=sys.stderr)
    finally:
        if created and not args.keep:
            subprocess.run([exe(args.examples, "make_fixture"), "--remove", str(target)])
            shutil.rmtree(created.parent, ignore_errors=True)
    summarize(report, args)
    if args.json:
        pathlib.Path(args.json).write_text(json.dumps(report, indent=2))


def summarize(report, args):
    groups = {}
    for r in report["runs"]:
        if "error" in r:
            print(f"error ({r['backend']}): {r['error']}")
            continue
        groups.setdefault((r["requested"], r["threads"]), []).append(r)
    print(f"\n{report['platform']}, {report['cpus']} logical CPUs, elevated={report['elevated']}, {report['target']}")
    print("\n| Backend (ran) | Threads | Runs | Wall ms (each) | Median wall | Files/s (median) | Peak RSS | Peak commit |")
    print("| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: |")
    for (requested, threads), rows in groups.items():
        runs = rows
        if args.cold_first and len(rows) > 1:
            cold = rows[0]
            print(f"| {cold['backend']} (first run) | {threads} | 1 | {cold['wall_ms']:.0f} | {cold['wall_ms']:.0f} | "
                  f"{cold['files_per_s']:,.0f} | {cold['peak_rss_bytes'] / 2**20:,.0f} MiB | {cold['peak_commit_bytes'] / 2**20:,.0f} MiB |")
            runs = rows[1:]
        walls = [r["wall_ms"] for r in runs]
        print(f"| {runs[0]['backend']} | {threads} | {len(runs)} | {', '.join(f'{w:.0f}' for w in walls)} | "
              f"{median(walls):,.0f} | {median([r['files_per_s'] for r in runs]):,.0f} | "
              f"{max(r['peak_rss_bytes'] for r in runs) / 2**20:,.0f} MiB | "
              f"{max(r['peak_commit_bytes'] for r in runs) / 2**20:,.0f} MiB |")
    # Correctness. Every run of one backend must agree, and backends that could
    # read the same folders (same issue count) must agree with each other.
    # Allocated bytes are compared separately: on NTFS a directory listing reports
    # the allocation cached in the folder entry, which can lag the file's MFT
    # record, so walks can disagree run to run; the MFT backend reads the record.
    ok = True
    distinct = {}
    allocated = {}
    for rows in groups.values():
        for r in rows:
            distinct.setdefault(tuple(r[k] for k in TOTALS), set()).add(r["backend"])
            allocated.setdefault(r["backend"], set()).add(r["allocated_bytes"])
    print("\nTotals (" + ", ".join(TOTALS) + "):")
    for totals, who in distinct.items():
        print(f"  {totals}  <- {', '.join(sorted(who))}")
    by_issues = {}
    for totals, who in distinct.items():
        for backend in who:
            by_issues.setdefault(totals[TOTALS.index("issue_count")], set()).add(totals)
            if sum(backend in w for w in distinct.values()) > 1:
                ok = False
                print(f"MISMATCH: {backend} reported different totals in different runs")
    for issues, sets in by_issues.items():
        if len(sets) > 1:
            ok = False
            print(f"MISMATCH: backends with {issues} issues disagree")
    if ok:
        print("Identical: every backend agrees (backends that read more folders are listed separately)")
    print("Allocated bytes:")
    for backend, values in sorted(allocated.items()):
        values = sorted(v for v in values if v is not None)
        spread = (values[-1] - values[0]) / values[-1] * 100 if values and values[-1] else 0
        stable = len(values) == 1
        note = "" if stable or backend.endswith("-directory") else "  UNSTABLE"
        if not stable and not backend.endswith("-directory"):
            ok = False
        print(f"  {backend}: {', '.join(f'{v:,}' for v in values)}"
              + ("" if stable else f" (spread {spread:.2f}%: NTFS folder entries lag the MFT){note}"))
    manifest = report["manifest"]
    if manifest:
        hidden = manifest["unreadable"]
        for totals, who in distinct.items():
            got = dict(zip(TOTALS, totals))
            full = (manifest["files"], manifest["directories"], manifest["logical_bytes"], manifest["unique_logical_bytes"])
            partial = (manifest["files"] - hidden["files"], manifest["directories"] - hidden["directories"],
                       manifest["logical_bytes"] - hidden["logical_bytes"],
                       manifest["unique_logical_bytes"] - hidden["logical_bytes"])
            seen = (got["files"], got["directories"], got["logical_bytes"], got["unique_logical_bytes"])
            if seen == full and got["issue_count"] == 0:
                verdict = "matches the manifest (everything readable)"
            elif seen == partial and got["issue_count"] == 1:
                verdict = "matches the manifest (the locked folder reported as 1 issue)"
            else:
                ok = False
                verdict = f"DIFFERS from the manifest: got {seen}, expected {full} or {partial}"
            links_ok = (got["hard_link_duplicates"] == manifest["hard_link_duplicates"]
                        and got["skipped_links"] == manifest["skipped_links"])
            ok &= links_ok
            print(f"  {', '.join(sorted(who))}: {verdict}; hard links and links {'match' if links_ok else 'DIFFER'}")
    if args.require_identical and not ok:
        sys.exit(1)


if __name__ == "__main__":
    main()
