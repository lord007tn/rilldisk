"""Synthetic local benchmark. Does not inventory or publish personal directories."""
import argparse
import json
import pathlib
import platform
import re
import statistics
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("executable", type=pathlib.Path)
    parser.add_argument("--files", type=int, default=50000)
    parser.add_argument("--runs", type=int, default=5)
    args = parser.parse_args()
    if args.files < 1 or args.runs < 1:
        parser.error("--files and --runs must be positive")
    executable = args.executable.resolve()
    report = {"platform": platform.platform(), "files": args.files, "directories": 257,
        "logical_bytes": args.files * 1024, "conditions": "synthetic; freshly written, warm metadata; no cache eviction",
        "results": []}
    with tempfile.TemporaryDirectory(prefix="rilldisk-benchmark-") as directory:
        root = pathlib.Path(directory)
        dirs = [root / f"dir-{i:03}" for i in range(256)]
        for item in dirs:
            item.mkdir()
        for index in range(args.files):
            (dirs[index % 256] / f"file-{index:07}.bin").write_bytes(b"x" * 1024)
        for threads in (1, 8):
            elapsed, scan = [], []
            for _ in range(args.runs):
                start = time.perf_counter()
                result = subprocess.run([str(executable), "scan", str(root), "--threads", str(threads), "--top", "1"],
                    capture_output=True, text=True, encoding="utf-8", check=True)
                elapsed.append(round((time.perf_counter() - start) * 1000, 2))
                match = re.search(r"(\d+) files \| (\d+) directories \| (\d+) ms", result.stdout)
                assert match and int(match[1]) == args.files and int(match[2]) == 257, result.stdout
                scan.append(int(match[3]))
            report["results"].append({"threads": threads, "end_to_end_ms": elapsed,
                "end_to_end_median_ms": statistics.median(elapsed), "scanner_ms": scan,
                "scanner_median_ms": statistics.median(scan)})
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
