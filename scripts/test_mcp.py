"""Black-box stdio MCP acceptance test. Python is a test tool, not an app dependency."""
import json
import os
import pathlib
import queue
import subprocess
import sys
import tempfile
import threading
import time


def main():
    executable = str(pathlib.Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix="rilldisk-mcp-") as directory, \
            tempfile.TemporaryDirectory(prefix="rilldisk-state-") as state:
        root = pathlib.Path(directory)
        # A private RillDisk state folder, so the user's protected-paths.txt is not read.
        env = dict(os.environ, LOCALAPPDATA=state, XDG_STATE_HOME=state)
        if sys.platform == "darwin":
            env["HOME"] = state
            protected_file = pathlib.Path(state, "Library", "Application Support", "RillDisk", "protected-paths.txt")
        elif sys.platform == "win32":
            protected_file = pathlib.Path(state, "RillDisk", "protected-paths.txt")
        else:
            protected_file = pathlib.Path(state, "rilldisk", "protected-paths.txt")
        (root / "nested").mkdir()
        (root / "nested" / "مرحبا.txt").write_bytes(b"x" * 8192)
        (root / "small.txt").write_bytes(b"hello")
        # A Rust project with build output and a linked worktree: reclaim candidates.
        (root / "app" / "target" / "debug").mkdir(parents=True)
        (root / "app" / "Cargo.toml").write_bytes(b"[package]")
        (root / "app" / "target" / "debug" / "app.exe").write_bytes(b"x" * 4096)
        (root / "wt").mkdir()
        (root / "wt" / ".git").write_bytes(b"gitdir: elsewhere")
        process = subprocess.Popen(
            [executable, "mcp", "--root", str(root)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", bufsize=1, env=env,
        )
        messages = queue.Queue()
        threading.Thread(target=lambda: [messages.put(line) for line in process.stdout], daemon=True).start()
        sequence = 0

        def send(method, params=None, notify=False):
            nonlocal sequence
            sequence += 1
            message = {"jsonrpc": "2.0", "method": method}
            if params is not None:
                message["params"] = params
            if not notify:
                message["id"] = sequence
            process.stdin.write(json.dumps(message) + "\n")
            process.stdin.flush()
            if notify:
                return
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                result = json.loads(messages.get(timeout=20))
                if result.get("id") == sequence:
                    return result
            raise AssertionError("MCP response timed out")

        def tool(name, arguments):
            result = send("tools/call", {"name": name, "arguments": arguments})
            assert "error" not in result, result
            assert not result["result"].get("isError"), result
            return json.loads(result["result"]["content"][0]["text"])

        try:
            initialized = send("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "rilldisk-acceptance", "version": "1"}})
            assert "result" in initialized, initialized
            send("notifications/initialized", notify=True)
            listed = send("tools/list")
            names = {tool["name"] for tool in listed["result"]["tools"]}
            assert names == {"allowed_roots", "start_scan", "scan_status", "query_entries",
                             "find_reclaimable", "file_types", "cancel_scan"}, names
            assert len(tool("allowed_roots", {})["roots"]) == 1
            denied = send("tools/call", {"name": "start_scan", "arguments": {"path": str(root.parent)}})
            assert "error" in denied or denied["result"].get("isError"), denied
            job = tool("start_scan", {"path": str(root)})["scan_id"]
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                status = tool("scan_status", {"scan_id": job})
                if status["status"] != "running":
                    break
                time.sleep(0.01)
            assert status["status"] == "complete", status
            assert status["summary"]["logical_bytes"] == 8197 + 9 + 4096 + 17, status
            assert status["summary"]["files"] == 5
            page = tool("query_entries", {"scan_id": job, "files_only": True, "limit": 1})
            assert page["total"] == 5 and page["next_offset"] == 1, page
            assert page["entries"][0]["logical_bytes"] == 8192
            page = tool("query_entries", {"scan_id": job, "query": "مرحبا"})
            assert page["total"] == 1, page
            reclaim = tool("find_reclaimable", {"scan_id": job, "min_bytes": 0})
            kinds = {c["category"]: c for c in reclaim["candidates"]}
            assert set(kinds) == {"build_output", "git_worktree"}, reclaim
            assert kinds["build_output"]["suggested_action"].startswith("cargo clean --manifest-path ")
            assert kinds["build_output"]["safety"] == "regenerable"
            assert kinds["git_worktree"]["safety"] == "review"
            assert reclaim["scan_complete"] and "never deletes" in reclaim["notice"]
            only = tool("find_reclaimable", {"scan_id": job, "min_bytes": 0, "category": "git_worktree"})
            assert only["total"] == 1, only
            assert reclaim["protected_excluded"] == 0, reclaim
            # The user's protected paths hide candidates, read on every call.
            protected_file.parent.mkdir(parents=True)
            protected_file.write_text("# never touch\n" + str(root / "wt") + "\n", encoding="utf-8")
            hidden = tool("find_reclaimable", {"scan_id": job, "min_bytes": 0})
            assert [c["category"] for c in hidden["candidates"]] == ["build_output"], hidden
            assert hidden["protected_excluded"] == 1 and hidden["protected_paths_error"] is None, hidden
            bad = send("tools/call", {"name": "find_reclaimable", "arguments": {"scan_id": job, "category": "photos"}})
            assert "error" in bad or bad["result"].get("isError"), bad
            types = tool("file_types", {"scan_id": job, "limit": 2})
            assert types["types"][0]["extension"] == "txt" and len(types["types"]) == 2, types
            assert not tool("cancel_scan", {"scan_id": job})["cancellation_requested"]
            invalid = send("tools/call", {"name": "query_entries", "arguments": {"scan_id": job, "limit": 501}})
            assert "error" in invalid or invalid["result"].get("isError"), invalid
            assert "next_offset" in json.dumps(invalid), invalid
            for index in range(3000):
                (root / f"cancel-{index:04}").mkdir()
            for _ in range(5):
                next_job = tool("start_scan", {"path": str(root)})["scan_id"]
                cancelled = tool("cancel_scan", {"scan_id": next_job})
                if cancelled["cancellation_requested"]:
                    break
            assert cancelled["cancellation_requested"], cancelled
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                status = tool("scan_status", {"scan_id": next_job})
                if status["status"] != "running":
                    break
                time.sleep(0.01)
            assert status["status"] == "cancelled", status
            assert not status["summary"]["complete"] and status["summary"]["cancelled"]
            assert not tool("query_entries", {"scan_id": next_job})["complete"]
            stale = send("tools/call", {"name": "scan_status", "arguments": {"scan_id": job}})
            assert "error" in stale or stale["result"].get("isError"), stale
            print("PASS: initialize, discovery, root confinement, background scan, status, pagination, Unicode, reclaim candidates, protected paths, file types, active cancellation, partial queries, expired IDs, validation")
        finally:
            process.stdin.close()
            try:
                code = process.wait(timeout=10)
                assert code == 0, process.stderr.read()
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise AssertionError("MCP server did not shut down after stdin closed")


if __name__ == "__main__":
    main()
