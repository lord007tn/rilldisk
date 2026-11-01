"""Stage RillDisk release archives with license notices, manifests and checksums.

Package one target after `cargo build --release --target TARGET`:

    python scripts/package_release.py package --target x86_64-pc-windows-msvc \
        --tag v0.3.0 --commit "$(git rev-parse HEAD)" --out dist

Then, with every target's output in one folder, write SHA256SUMS and the
release-wide BUILD-MANIFEST.json:

    python scripts/package_release.py finalize --tag v0.3.0 --commit SHA --out dist

Archives are named `rilldisk-<tag>-<target>.zip` (Windows, flat) or
`.tar.gz` (macOS/Linux, one top-level folder). `crates/rilldisk-update` looks
for exactly these names; keep them in sync.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import time
import zipfile

REPO = pathlib.Path(__file__).resolve().parent.parent
BINARIES = ("rilldisk", "rilldisk-desktop")
SOURCE = "https://github.com/lord007tn/rilldisk"
NOTICE = re.compile(r"^(licen[cs]e|copying|notice|unlicense|copyright)([-._].*)?$", re.I)
MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
"""


def sha(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def version_of(tag):
    if not re.fullmatch(r"v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?", tag):
        sys.exit(f"tag {tag!r} is not v<semver>")
    return tag[1:]


def archive_name(tag, target):
    return f"rilldisk-{tag}-{target}." + ("zip" if "windows" in target else "tar.gz")


def exe(name, target):
    return name + (".exe" if "windows" in target else "")


def dependency_closure(target):
    """Non-dev dependencies of the shipped binaries for `target`."""
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target],
        cwd=REPO))
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    workspace = set(meta["workspace_members"])
    stack = [i for i in workspace if packages[i]["name"] in BINARIES]
    seen = set()
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]["deps"]:
            if {k["kind"] for k in dep["dep_kinds"]} - {"dev"}:
                stack.append(dep["pkg"])
    third_party = sorted(seen - workspace, key=lambda i: (packages[i]["name"], packages[i]["version"]))
    return [packages[i] for i in third_party]


def rust_docs():
    sysroot = pathlib.Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    docs = sysroot / "share" / "doc" / "rust"
    if not (docs / "COPYRIGHT.html").is_file():
        sys.exit(f"Rust toolchain notices not found in {docs}")
    return docs


def copy_notices(stage, packages, docs):
    """Copy every dependency's license files; reproduce standard texts for
    crates that declare a license but ship no file."""
    root = stage / "THIRD-PARTY-LICENSES"
    inventory, missing = [], []
    for pkg in packages:
        source = pathlib.Path(pkg["manifest_path"]).parent
        folder = f"{pkg['name']}-{pkg['version']}"
        notices = []
        candidates = [f for f in sorted(source.iterdir()) if f.is_file() and NOTICE.match(f.name)]
        # Fonts and other assets bundled inside crates carry their own licenses.
        candidates += [f for f in sorted(source.glob("fonts/*.txt"))]
        for f in candidates:
            rel = f.relative_to(source).as_posix()
            dest = root / folder / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(f, dest)
            notices.append(f"THIRD-PARTY-LICENSES/{folder}/{rel}")
        if not any(NOTICE.match(pathlib.PurePosixPath(n).name) for n in notices):
            missing.append((folder, pkg))
        inventory.append({"name": pkg["name"], "version": pkg["version"],
                          "license": pkg.get("license") or pkg.get("license_file"),
                          "repository": pkg.get("repository"), "notices": notices})

    def find_text(marker):
        for pkg in packages:
            for f in pathlib.Path(pkg["manifest_path"]).parent.iterdir():
                if f.is_file() and NOTICE.match(f.name):
                    text = f.read_text(encoding="utf-8", errors="replace")
                    if marker in text:
                        return text
        return None

    texts = {"Apache-2.0": (docs / "licenses" / "Apache-2.0.txt").read_text(encoding="utf-8")}
    generated = []
    for folder, pkg in missing:
        expr = pkg.get("license") or ""
        authors = ", ".join(pkg.get("authors") or []) or "the authors"
        parts = [f"{pkg['name']} {pkg['version']}\nDeclared license: {expr}\n"
                 f"Source: {pkg.get('repository') or 'crates.io'}\n"
                 "The crates.io package ships no license file; the standard text of each\n"
                 "declared license is reproduced below.\n"]
        if "MIT" in expr:
            parts.append(f"--- MIT ---\nCopyright (c) {authors}\n\n{MIT}")
        if "Apache-2.0" in expr:
            parts.append(f"--- Apache-2.0 ---\n{texts['Apache-2.0']}")
        if "BSL-1.0" in expr:
            bsl = texts.setdefault("BSL-1.0", find_text("Boost Software License - Version 1.0"))
            if not bsl:
                sys.exit(f"{folder} is BSL-1.0 but no Boost license text was found to reproduce")
            parts.append(f"--- BSL-1.0 ---\n{bsl}")
        if len(parts) == 1:
            sys.exit(f"{folder} ships no license file and its license {expr!r} has no known text")
        dest = root / folder / "LICENSE-GENERATED.txt"
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text("\n".join(parts), encoding="utf-8")
        for item in inventory:
            if f"{item['name']}-{item['version']}" == folder:
                item["notices"].append(f"THIRD-PARTY-LICENSES/{folder}/LICENSE-GENERATED.txt")
        generated.append(folder)

    # The Rust standard library is linked into every binary.
    for f in sorted(docs.glob("COPYRIGHT*.html")):
        shutil.copyfile(f, stage / f"RUST-{f.name}")
    if (docs / "licenses").is_dir():
        shutil.copytree(docs / "licenses", root / "rust-toolchain")
    (stage / "THIRD-PARTY-LICENSES.json").write_text(json.dumps(inventory, indent=2) + "\n", encoding="utf-8")
    return inventory, generated


def start_here(version, target, commit):
    pre = "-" in version
    title = f"# RillDisk {version}" + (" (pre-release)" if pre else "")
    common_tail = f"""
RillDisk reads file metadata only. It never deletes, moves or uploads anything;
reclaim results are suggestions with a command for you to run. Reports never
overwrite existing files. Exit 0 means complete, 2 partial, 130 cancelled, 1 failure.

Updates: `rilldisk update --check` shows whether a newer release exists and
`rilldisk update` installs it after asking. Nothing is checked unless you run
those commands or the desktop app's once-a-day check is on; set
`RILLDISK_NO_UPDATE_CHECK=1` to turn every update check off. See
docs/proposals/updates.md in the source for exactly what is sent (nothing
about you or your files).

AGPL-3.0-only. Complete source: {SOURCE}/tree/{commit}
Dependency notices are included under THIRD-PARTY-LICENSES.
"""
    if "windows" in target:
        return f"""{title}

Windows x64. Two programs, no installer, no admin rights:

* `rilldisk-desktop.exe` - the desktop app. Double-click it, then pick a drive or
  press Browse. Press F5 to rescan and Esc to stop.
* `rilldisk.exe` - the CLI and local MCP server.

From PowerShell in this directory:

```powershell
.\\rilldisk.exe drives
.\\rilldisk.exe reclaim $HOME          # worktrees, build output, caches you can free
.\\rilldisk.exe scan 'C:\\Data' --format json --output report.json
.\\rilldisk.exe mcp --root 'C:\\Users\\YourName'
```

Windows SmartScreen may warn because these builds are not code-signed yet.
{common_tail}"""
    if "apple" in target:
        arch = "Apple silicon (arm64)" if target.startswith("aarch64") else "Intel (x86_64)"
        platform_notes = f"""macOS, {arch}. Two programs, no installer:

* `rilldisk-desktop` - the desktop app.
* `rilldisk` - the CLI and local MCP server.

These builds are not notarized yet. If macOS blocks them, clear the download
quarantine once from this folder:

```sh
xattr -dr com.apple.quarantine .
```"""
    else:
        platform_notes = """Linux x86_64 (glibc). Two programs, no installer:

* `rilldisk-desktop` - the desktop app; needs a Wayland or X11 session with
  OpenGL and libxkbcommon (installed on any desktop distribution).
* `rilldisk` - the CLI and local MCP server; no runtime dependencies beyond glibc."""
    return f"""{title}

{platform_notes}

From a terminal in this directory:

```sh
./rilldisk reclaim "$HOME"            # worktrees, build output, caches you can free
./rilldisk scan /data --format json --output report.json
./rilldisk mcp --root "$HOME"
./rilldisk-desktop
```

`rilldisk drives` lists mounted disks with their capacity; scans stay on one
filesystem unless you pass `--cross-filesystems`.
{common_tail}"""


def write_zip(stage, archive):
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for f in sorted(stage.rglob("*")):
            if f.is_file():
                z.write(f, f.relative_to(stage).as_posix())


def write_tar_gz(stage, archive, executables, mtime):
    top = stage.name

    def normalize(info):
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        info.mtime = mtime
        if info.isdir():
            info.mode = 0o755
        else:
            info.mode = 0o755 if pathlib.PurePosixPath(info.name).name in executables else 0o644
        return info

    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as tar:
        tar.add(stage, arcname=top, recursive=False, filter=normalize)
        for f in sorted(stage.rglob("*")):
            tar.add(f, arcname=f"{top}/{f.relative_to(stage).as_posix()}", recursive=False, filter=normalize)
    with open(archive, "wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=mtime) as gz:
            gz.write(buffer.getvalue())


def package(args):
    version = version_of(args.tag)
    target = args.target
    out = pathlib.Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    bin_dir = pathlib.Path(args.bin_dir or REPO / "target" / target / "release").resolve()
    stage = out / "stage" / f"rilldisk-{args.tag}-{target}"
    if stage.exists():
        shutil.rmtree(stage)
    (stage / "THIRD-PARTY-LICENSES").mkdir(parents=True)

    docs = rust_docs()
    inventory, generated = copy_notices(stage, dependency_closure(target), docs)
    executables = [exe(b, target) for b in BINARIES]
    for name in executables:
        src = bin_dir / name
        if not src.is_file():
            sys.exit(f"missing {src}; build with `cargo build --release --locked --target {target}`")
        shutil.copyfile(src, stage / name)
        os.chmod(stage / name, 0o755)
    for name in ("LICENSE", "README.md"):
        shutil.copyfile(REPO / name, stage / name)
    (stage / "START-HERE.md").write_text(start_here(version, target, args.commit), encoding="utf-8")

    rustc = subprocess.check_output(["rustc", "--version"], text=True).split(" (")[0]
    manifest = {
        "name": "RillDisk", "version": version, "prerelease": "-" in version,
        "target": target, "license": "AGPL-3.0-only",
        "release_commit": args.commit, "release_tag": args.tag,
        "source_url": f"{SOURCE}/tree/{args.commit}",
        "toolchain": rustc, "profile": "release; thin LTO; stripped debug info",
        "executables_sha256": {n: sha(stage / n) for n in executables},
        "third_party_packages": len(inventory),
        "generated_license_texts": generated,
        "validation_ci": args.ci_url,
    }
    (stage / "BUILD-MANIFEST.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    archive = out / archive_name(args.tag, target)
    mtime = int(os.environ.get("SOURCE_DATE_EPOCH") or time.time())
    if archive.suffix == ".zip":
        write_zip(stage, archive)
    else:
        write_tar_gz(stage, archive, executables, mtime)
    manifest["archive"] = archive.name
    manifest["archive_sha256"] = sha(archive)
    manifest["archive_bytes"] = archive.stat().st_size
    (out / f"{archive.name}.build.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    shutil.rmtree(out / "stage")
    print(json.dumps({"archive": archive.name, "packages": len(inventory),
                      "generated_license_texts": generated,
                      "mb": round(archive.stat().st_size / 1e6, 1)}, indent=1))


def finalize(args):
    version = version_of(args.tag)
    out = pathlib.Path(args.out).resolve()
    fragments = sorted(out.glob("*.build.json"))
    if not fragments:
        sys.exit(f"no *.build.json in {out}; run `package` for each target first")
    targets, sums = [], []
    for fragment in fragments:
        item = json.loads(fragment.read_text(encoding="utf-8"))
        if item["release_tag"] != args.tag or item["release_commit"] != args.commit:
            sys.exit(f"{fragment.name} was built for {item['release_tag']} @ {item['release_commit']}")
        archive = out / item["archive"]
        digest = sha(archive)
        if digest != item["archive_sha256"]:
            sys.exit(f"{archive.name} changed after packaging")
        sums.append(f"{digest}  {archive.name}")
        targets.append({k: item[k] for k in ("target", "archive", "archive_sha256", "archive_bytes",
                                             "executables_sha256", "toolchain", "third_party_packages")})
        fragment.unlink()
    (out / "SHA256SUMS").write_text("\n".join(sorted(sums, key=lambda l: l.split("  ", 1)[1])) + "\n",
                                    encoding="utf-8", newline="\n")
    manifest = {
        "name": "RillDisk", "version": version, "prerelease": "-" in version,
        "license": "AGPL-3.0-only", "release_tag": args.tag, "release_commit": args.commit,
        "source_url": f"{SOURCE}/tree/{args.commit}", "validation_ci": args.ci_url,
        "profile": "release; thin LTO; stripped debug info",
        "targets": sorted(targets, key=lambda t: t["target"]),
    }
    (out / "BUILD-MANIFEST.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print((out / "SHA256SUMS").read_text(encoding="utf-8"), end="")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("package", help="stage and archive one target")
    p.add_argument("--target", required=True)
    p.add_argument("--bin-dir", help="default: target/<target>/release")
    f = sub.add_parser("finalize", help="write SHA256SUMS and BUILD-MANIFEST.json")
    for s in (p, f):
        s.add_argument("--tag", required=True)
        s.add_argument("--commit", required=True)
        s.add_argument("--out", default="dist")
        s.add_argument("--ci-url", default=None)
    args = parser.parse_args()
    package(args) if args.command == "package" else finalize(args)


if __name__ == "__main__":
    main()
