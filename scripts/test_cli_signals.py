"""Verify that SIGINT can interrupt a blocked JSON export on Unix."""
import os
import pathlib
import queue
import signal
import subprocess
import sys
import tempfile
import threading


def main():
    if os.name == "nt":
        raise SystemExit("This test requires Unix SIGINT semantics")
    executable = str(pathlib.Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix="rilldisk-signal-") as directory:
        root = pathlib.Path(directory)
        for index in range(5000):
            (root / f"file-{index:04}").touch()
        process = subprocess.Popen([executable, "scan", str(root), "--format", "json"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        started = queue.Queue()
        threading.Thread(target=lambda: started.put(process.stdout.read(1)), daemon=True).start()
        try:
            assert started.get(timeout=20) == b"{", "JSON export did not begin"
            # The unconsumed report exceeds pipe capacity, keeping export in progress.
            os.kill(process.pid, signal.SIGINT)
            assert process.wait(timeout=10) == 130, "export did not honor SIGINT"
            print("PASS: SIGINT interrupts a blocked JSON export with exit code 130")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            process.stdout.close()


if __name__ == "__main__":
    main()
