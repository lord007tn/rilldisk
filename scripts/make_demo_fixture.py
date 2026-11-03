"""Create a synthetic, deterministic disk for screenshots, recordings and demos.

Large files are sparse, so logical data uses very little real disk. Nothing here
comes from a real machine. Usage:

    python scripts/make_demo_fixture.py <empty-or-new-directory> [--scale 1.0]

The tree holds ~150 GB of logical data while using only a few hundred MB of disk.

With `git` on PATH, the projects become real repositories and the AI-agent and
linked worktrees real Git worktrees in fixed states (two with no changes, one with
an uncommitted edit, one with a commit only on its detached HEAD), so RillDisk's
Git verdicts in screenshots are Git's own answers. Without Git they stay pointer
files and their verdicts read "unknown".

Use the "Size" (logical) view when capturing; sparse files have ~0 bytes on disk.
"""
import ctypes
import os
import pathlib
import random
import shutil
import subprocess
import sys
import time

DAY = 86400
NOW = time.time()
rng = random.Random(20261101)
GiB = 1 << 30
MiB = 1 << 20
KiB = 1 << 10
SCALE = float(sys.argv[sys.argv.index("--scale") + 1]) if "--scale" in sys.argv else 1.0


def make_sparse(handle_file):
    if os.name != "nt":
        return  # truncate() already creates holes on Linux/macOS filesystems.
    import msvcrt

    FSCTL_SET_SPARSE = 0x000900C4
    handle = msvcrt.get_osfhandle(handle_file.fileno())
    returned = ctypes.c_ulong()
    ok = ctypes.windll.kernel32.DeviceIoControl(
        ctypes.c_void_p(handle), FSCTL_SET_SPARSE, None, 0, None, 0, ctypes.byref(returned), None
    )
    if not ok:
        raise OSError("FSCTL_SET_SPARSE failed")


def write(path, size, age_days=None):
    path.parent.mkdir(parents=True, exist_ok=True)
    if size > 64 * KiB:
        size = int(size * SCALE)
    with open(path, "wb") as f:
        if size > 256 * KiB:
            make_sparse(f)
            # truncate() extends with written zeros on Windows; seeking leaves a hole.
            f.seek(size - 1)
            f.write(bytes(1))
        else:
            f.write(b"\0" * size)
    if age_days is not None:
        stamp = NOW - age_days * DAY - rng.random() * DAY
        os.utime(path, (stamp, stamp))


def spread(base, names, total, age, jitter=0.6):
    weights = [rng.random() ** 2 + 0.05 for _ in names]
    scale = total / sum(weights)
    for name, weight in zip(names, weights):
        write(base / name, int(weight * scale), age + rng.random() * age * jitter)


def git(cwd, *args, age_days=None):
    env = dict(os.environ, GIT_AUTHOR_NAME="RillDisk demo", GIT_AUTHOR_EMAIL="demo@example.invalid",
               GIT_COMMITTER_NAME="RillDisk demo", GIT_COMMITTER_EMAIL="demo@example.invalid")
    if age_days is not None:
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(NOW - age_days * DAY))
        env["GIT_AUTHOR_DATE"] = env["GIT_COMMITTER_DATE"] = stamp
    subprocess.run(["git", "-C", str(cwd), *args], check=True, env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def age_tree(path, age_days):
    stamp = NOW - age_days * DAY
    for directory, _, files in os.walk(path):
        for name in files:
            os.utime(os.path.join(directory, name), (stamp, stamp))


def repository(path, ignored, age_days):
    """Commit everything in `path` except `ignored` folders (shared by its worktrees)."""
    git(path, "init", "-q", "-b", "main")
    (path / ".git" / "info" / "exclude").write_text("".join(f"{name}/\n" for name in ignored))
    git(path, "add", "-A")
    git(path, "commit", "-q", "-m", "Import", age_days=age_days)


def worktree(repo, path, age_days, branch=None):
    """Make the folder at `path` a linked worktree of `repo` holding its current files,
    committed `age_days` ago on `branch` (detached when None)."""
    (path / ".git").unlink()
    aside = path.with_name(path.name + ".aside")
    path.rename(aside)
    args = ["worktree", "add", "-q"] + (["-b", branch] if branch else ["--detach"]) + [str(path)]
    git(repo, *args)
    for item in path.iterdir():
        if item.name != ".git":
            shutil.rmtree(item) if item.is_dir() else item.unlink()
    for item in aside.iterdir():
        shutil.move(str(item), str(path / item.name))
    aside.rmdir()
    git(path, "add", "-A")
    git(path, "commit", "-q", "--allow-empty", "-m", f"Work in {path.name}", age_days=age_days)
    age_tree(path, age_days)


def gitify(root):
    if shutil.which("git") is None:
        print("git not found: worktrees stay pointer files (Git verdicts will read unknown)")
        return
    p = root / "Projects"
    web, rust = p / "atlas-web", p / "ferrite-engine"
    repository(web, [".claude", "node_modules", ".next"], 2)
    repository(rust, ["target"], 3)
    # No changes Git can see; idle for weeks.
    worktree(web, web / ".claude" / "worktrees" / "agent-3f9c2a", 41, "claude/agent-3f9c2a")
    worktree(web, root / "Users" / "demo" / ".codex" / "worktrees" / "a71c" / "atlas-web", 63, "codex/search")
    # A commit made on a detached HEAD: nothing else holds it.
    worktree(web, root / "Users" / "demo" / ".t3" / "worktrees" / "atlas-web-spike", 25)
    # Idle for months, yet one tracked file holds an uncommitted edit.
    perf = p / "ferrite-engine-perf"
    worktree(rust, perf, 118, "perf-experiment")
    edited = perf / "src" / "module_0.rs"
    with open(edited, "ab") as f:
        f.write(b"// unfinished benchmark tweak\n")
    age_tree(perf, 118)


def main():
    root = pathlib.Path(sys.argv[1]).resolve()
    if root.exists() and any(root.iterdir()):
        sys.exit(f"{root} must be empty or new")
    root.mkdir(parents=True, exist_ok=True)

    # Media
    v = root / "Videos"
    write(v / "Iceland 2025" / "ring-road-drone-4k.mp4", int(9.4 * GiB), 380)
    write(v / "Iceland 2025" / "glacier-hike.mov", int(5.1 * GiB), 381)
    spread(v / "Iceland 2025", [f"clip-{i:03}.mp4" for i in range(18)], 6.5 * GiB, 380)
    write(v / "Screen recordings" / "conference-talk-final.mkv", int(3.8 * GiB), 140)
    spread(v / "Screen recordings", [f"capture-{i:02}.mp4" for i in range(12)], 2.6 * GiB, 60)
    write(v / "Renders" / "product-launch-master.mov", int(7.2 * GiB), 220)

    spread(root / "Pictures" / "2025", [f"IMG_{4000 + i}.jpg" for i in range(260)], 1.6 * GiB, 300)
    spread(root / "Pictures" / "2026", [f"IMG_{7000 + i}.heic" for i in range(180)], 0.9 * GiB, 40)
    spread(root / "Pictures" / "RAW", [f"DSC_{1000 + i}.dng" for i in range(90)], 2.4 * GiB, 410)
    spread(root / "Music" / "Lossless", [f"track-{i:03}.flac" for i in range(120)], 3.4 * GiB, 700)

    # Games
    game = root / "Games" / "SteamLibrary" / "steamapps" / "common" / "Starfall Odyssey"
    write(game / "Content" / "world.pak", int(18.5 * GiB), 25)
    write(game / "Content" / "audio.pak", int(4.2 * GiB), 25)
    write(game / "Content" / "cinematics.pak", int(6.1 * GiB), 25)
    spread(game / "Binaries", ["starfall.exe", "engine.dll", "physics.dll", "steam_api64.dll"], 0.4 * GiB, 25)

    # Downloads
    d = root / "Downloads"
    write(d / "ubuntu-26.04-desktop-amd64.iso", int(5.9 * GiB), 90)
    write(d / "Win11_24H2_English_x64.iso", int(5.4 * GiB), 260)
    spread(d, ["blender-5.0-windows-x64.msi", "VSCodeUserSetup-x64.exe", "dataset-archive.zip",
               "photos-backup-2024.7z", "driver-pack.zip", "fonts.zip"], 4.8 * GiB, 120)

    spread(root / "Documents" / "Finance", [f"statement-{m:02}.pdf" for m in range(1, 13)], 60 * MiB, 200)
    spread(root / "Documents" / "Work", ["roadmap.pptx", "budget-2026.xlsx", "spec.docx", "research.pdf",
                                          "notes.txt", "contracts.pdf"], 380 * MiB, 30)

    # Virtual machines and models
    write(root / "VMs" / "ubuntu-dev.vhdx", int(24 * GiB), 160)
    write(root / "VMs" / "windows-test.vhdx", int(11 * GiB), 400)

    # Projects
    p = root / "Projects"
    web = p / "atlas-web"
    write(web / "package.json", 900, 2)
    spread(web / "src", [f"component-{i}.tsx" for i in range(40)], 2 * MiB, 2)
    packages = ["react", "react-dom", "next", "typescript", "@swc", "esbuild", "lodash", "date-fns",
                "@aws-sdk", "playwright-core", "prisma", "@img", "sharp", "rxjs", "zod", "tailwindcss",
                "eslint", "@babel", "webpack", "terser"]
    for name in packages:
        files = rng.randint(30, 140)
        spread(web / "node_modules" / name / "dist", [f"chunk-{i}.js" for i in range(files)],
               rng.uniform(8, 140) * MiB, 9)
        write(web / "node_modules" / name / "package.json", 600, 9)
    write(web / "node_modules" / "@next" / "swc-win32-x64-msvc" / "next-swc.win32-x64-msvc.node", int(140 * MiB), 9)
    spread(web / ".next" / "cache" / "webpack", [f"pack-{i}.pack" for i in range(40)], 820 * MiB, 1)

    # An AI-agent worktree of the same app, idle for weeks.
    wt = web / ".claude" / "worktrees" / "agent-3f9c2a"
    write(wt / ".git", 60, 41)
    write(wt / "package.json", 900, 41)
    spread(wt / "src", [f"component-{i}.tsx" for i in range(40)], 2 * MiB, 41)
    for name in packages[:12]:
        spread(wt / "node_modules" / name / "dist", [f"chunk-{i}.js" for i in range(40)],
               rng.uniform(10, 90) * MiB, 41)

    rust = p / "ferrite-engine"
    write(rust / "Cargo.toml", 1200, 3)
    spread(rust / "src", [f"module_{i}.rs" for i in range(60)], 3 * MiB, 3)
    spread(rust / "target" / "debug" / "deps", [f"libcrate_{i}.rlib" for i in range(160)], 3.1 * GiB, 3)
    spread(rust / "target" / "debug" / "incremental", [f"session-{i}.bin" for i in range(60)], 1.4 * GiB, 3)
    write(rust / "target" / "debug" / "ferrite.pdb", int(640 * MiB), 3)
    spread(rust / "target" / "release" / "deps", [f"libcrate_{i}.rlib" for i in range(80)], 1.2 * GiB, 12)

    # More agent worktrees of atlas-web, kept where Codex and T3 Code put them.
    for wt, age, count in [(root / "Users" / "demo" / ".codex" / "worktrees" / "a71c" / "atlas-web", 63, 8),
                           (root / "Users" / "demo" / ".t3" / "worktrees" / "atlas-web-spike", 25, 6)]:
        write(wt / ".git", 60, age)
        write(wt / "package.json", 900, age)
        spread(wt / "src", [f"component-{i}.tsx" for i in range(12)], 1 * MiB, age)
        for name in packages[:count]:
            spread(wt / "node_modules" / name / "dist", [f"chunk-{i}.js" for i in range(30)],
                   rng.uniform(10, 60) * MiB, age)

    # A stale linked worktree for a perf experiment.
    perf = p / "ferrite-engine-perf"
    write(perf / ".git", 60, 118)
    write(perf / "Cargo.toml", 1200, 118)
    spread(perf / "src", [f"module_{i}.rs" for i in range(60)], 3 * MiB, 118)
    spread(perf / "target" / "release" / "deps", [f"libcrate_{i}.rlib" for i in range(90)], 2.3 * GiB, 118)

    ml = p / "ml-notebooks"
    write(ml / "pyproject.toml", 700, 20)
    write(ml / ".venv" / "pyvenv.cfg", 200, 20)
    spread(ml / ".venv" / "Lib" / "site-packages" / "torch" / "lib", ["torch_cpu.dll", "torch_cuda.dll",
           "cudnn64_9.dll", "cublasLt64_12.dll"], 4.6 * GiB, 20)
    spread(ml / "models", ["llama-8b-q4.gguf", "embeddings.safetensors"], 6.2 * GiB, 70)
    spread(ml / "data", [f"shard-{i:02}.parquet" for i in range(16)], 2.1 * GiB, 70)
    spread(ml / "src" / "__pycache__", [f"mod{i}.cpython-313.pyc" for i in range(30)], 3 * MiB, 20)

    game_dev = p / "unity-prototype"
    write(game_dev / "Assets" / "Scenes" / "Main.unity", 3 * MiB, 50)
    write(game_dev / "ProjectSettings" / "ProjectSettings.asset", 40 * KiB, 50)
    spread(game_dev / "Library" / "Artifacts", [f"{i:02x}" for i in range(120)], 2.9 * GiB, 50)

    # User caches
    local = root / "Users" / "demo" / "AppData" / "Local"
    spread(local / "Temp", [f"tmp{i:04x}.tmp" for i in range(220)], 1.7 * GiB, 6)
    spread(local / "npm-cache" / "_cacache" / "content-v2", [f"sha512-{i:04x}" for i in range(300)], 1.3 * GiB, 15)
    spread(local / "pip" / "Cache" / "http", [f"wheel-{i:03}" for i in range(80)], 0.7 * GiB, 30)
    spread(local / "NVIDIA" / "DXCache", [f"shader-{i:03}.bin" for i in range(90)], 0.6 * GiB, 4)

    gitify(root)

    # Marks the tree as synthetic: `rilldisk-desktop --capture-cleanup` only
    # removes folders from a scan that has this file at its root (or lives in temp).
    (root / ".rilldisk-synthetic-fixture").write_text("Synthetic RillDisk demo data. Safe to delete.\n")

    # Directory times reflect their newest content (bottom-up), as on a lived-in disk.
    for directory, _, files in sorted(os.walk(root), key=lambda w: -len(w[0])):
        children = [os.path.join(directory, f) for f in files] + [
            os.path.join(directory, c) for c in os.listdir(directory) if os.path.isdir(os.path.join(directory, c))]
        newest = max((os.stat(c).st_mtime for c in children), default=NOW - 30 * DAY)
        os.utime(directory, (newest, newest))
    print(root)


if __name__ == "__main__":
    main()
