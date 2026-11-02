"""Build RillDisk installers and system packages from a release archive.

Run after `package_release.py package` produced the archive for the target:

    python scripts/package_installers.py msi   --target x86_64-pc-windows-msvc --tag v0.1.1 --commit SHA --out dist
    python scripts/package_installers.py linux --target x86_64-unknown-linux-gnu --tag v0.1.1 --commit SHA --out dist
    python scripts/package_installers.py macos --target aarch64-apple-darwin --tag v0.1.1 --commit SHA --out dist

Every package is built from the files inside the release archive, so the
programs, license notices and manifest are identical across formats.

| command | output | needs |
| --- | --- | --- |
| msi   | RillDisk-<version>-<x64/arm64>.msi | WiX Toolset 5 (`wix`), Windows |
| linux | rilldisk_<version>_<amd64/arm64>.deb | dpkg-deb |
|       | rilldisk-<version>-1.<x86_64/aarch64>.rpm | rpmbuild |
|       | RillDisk-<version>-<x86_64/aarch64>.AppImage (+ .zsync) | appimagetool, an AppImage runtime |
| macos | RillDisk-<version>-<arm64/x64>.dmg with RillDisk.app | codesign, hdiutil (macOS) |

Each package also gets `<name>.package.json`, which `package_release.py
finalize` folds into SHA256SUMS, BUILD-MANIFEST.json and the latest.yml feed.
Packages install an `installed-by` marker (`msi`, `deb`, `rpm`, `appimage`)
that `rilldisk update` reads to point at the matching new package instead of
replacing files a package manager owns.
"""
import argparse
import datetime
import gzip
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile

REPO = pathlib.Path(__file__).resolve().parent.parent
PACKAGING = REPO / "packaging"
SOURCE = "https://github.com/lord007tn/rilldisk"
APP_ID = "io.github.lord007tn.RillDisk"
MAINTAINER = "Raed Bahri <lord007tn@users.noreply.github.com>"
SUMMARY = "Fast, local disk usage analyzer with a desktop app, CLI and MCP server"
DESCRIPTION = """RillDisk scans a drive or folder, shows where the space went as a treemap
or rings chart, and points out build output, caches, old worktrees and
other things you can safely remove, each with a reason. It never deletes
anything on its own: only the folders you select and confirm, moved to the
Trash by default. It never uploads anything. The rilldisk
command line and its local MCP server share the same scanner."""
# Debian names; libraries the desktop app loads at run time for its window
# and GPU. The CLI needs only libc.
DEB_RECOMMENDS = "libegl1, libxkbcommon-x11-0, libwayland-client0, libvulkan1"
RPM_RECOMMENDS = ["mesa-libEGL", "libxkbcommon-x11", "libwayland-client", "vulkan-loader"]
ICON_SIZES = (16, 24, 32, 48, 64, 128, 256, 512)


def build_date():
    """Commit time when SOURCE_DATE_EPOCH is set (release builds), else now."""
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH") or time.time())
    return datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc)


def run(*cmd, **kw):
    print("+", " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run([str(c) for c in cmd], check=True, **kw)


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


def tilde(version):
    """Debian and RPM order `1.0~rc.1` before `1.0`; semver writes `1.0-rc.1`."""
    return version.replace("-", "~", 1)


def archive_path(out, tag, target):
    return out / (f"rilldisk-{tag}-{target}." + ("zip" if "windows" in target else "tar.gz"))


def unpack(out, tag, target, into):
    """Extract the release archive for `target`; returns the folder holding
    the programs and notices."""
    archive = archive_path(out, tag, target)
    if not archive.is_file():
        sys.exit(f"missing {archive}; run package_release.py package first")
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as z:
            z.extractall(into)
        return into
    with tarfile.open(archive) as t:
        t.extractall(into, filter="data")
    (top,) = [p for p in into.iterdir() if p.is_dir()]
    return top


def record(out, kind, target, path, args):
    """Write the fragment `finalize` reads, and print a one-line summary."""
    fragment = {
        "kind": kind, "target": target, "name": path.name, "sha256": sha(path),
        "bytes": path.stat().st_size, "release_tag": args.tag, "release_commit": args.commit,
    }
    (out / f"{path.name}.package.json").write_text(json.dumps(fragment, indent=2) + "\n", encoding="utf-8")
    print(f"{kind:9} {path.name}  {round(path.stat().st_size / 1e6, 1)} MB")


def notices(stage):
    """License and notice files from the archive (everything but programs)."""
    skip = {"rilldisk", "rilldisk-desktop", "rilldisk.exe", "rilldisk-desktop.exe", "START-HERE.md"}
    return [p for p in sorted(stage.iterdir()) if p.name not in skip]


# ---------------------------------------------------------------- Windows MSI

def msi(args):
    version = version_of(args.tag)
    arch = {"x86_64-pc-windows-msvc": "x64", "aarch64-pc-windows-msvc": "arm64"}[args.target]
    out = pathlib.Path(args.out).resolve()
    numeric = version.split("-")[0]
    name = out / f"RillDisk-{version}-{arch}.msi"
    wix = args.wix or shutil.which("wix") or sys.exit("wix (WiX Toolset 5) not found; dotnet tool install --global wix --version 5.0.2")
    with tempfile.TemporaryDirectory() as tmp:
        stage = unpack(out, args.tag, args.target, pathlib.Path(tmp) / "stage")
        (stage / "installed-by").write_text("msi\n", encoding="utf-8")
        run(wix, "build", PACKAGING / "windows" / "rilldisk.wxs", "-arch", arch,
            "-ext", "WixToolset.UI.wixext", "-ext", "WixToolset.Util.wixext",
            "-d", f"Version={numeric}", "-d", f"Stage={stage}", "-d", f"PackagingDir={PACKAGING}",
            "-pdbtype", "none", "-o", name)
    # Windows Installer's ICE checks. ICE57 cannot see that HKMU means HKLM in a
    # per-machine package; ICE61 flags same-version upgrades, which are wanted.
    run(wix, "msi", "validate", "-sice", "ICE57", "-sice", "ICE61", name)
    record(out, "msi", args.target, name, args)


# ------------------------------------------------------------- Linux packages

LINUX_ARCH = {
    "x86_64-unknown-linux-gnu": ("amd64", "x86_64"),
    "aarch64-unknown-linux-gnu": ("arm64", "aarch64"),
}


def linked(binaries):
    """(newest GLIBC_x.y symbol version needed, shared libraries linked)."""
    versions, libraries = set(), set()
    for b in binaries:
        text = subprocess.run(["objdump", "-T", "-p", str(b)], check=True, capture_output=True, text=True).stdout
        versions.update(re.findall(r"GLIBC_(\d+)\.(\d+)", text))
        libraries.update(re.findall(r"NEEDED\s+(\S+)", text))
    major, minor = max((int(a), int(b)) for a, b in versions)
    return f"{major}.{minor}", libraries


def linux_tree(stage, root, version, kind):
    """Install layout shared by .deb, .rpm and the AppImage (under `root`)."""
    usr = root / "usr"
    (usr / "bin").mkdir(parents=True)
    for program in ("rilldisk", "rilldisk-desktop"):
        shutil.copyfile(stage / program, usr / "bin" / program)
        os.chmod(usr / "bin" / program, 0o755)
    share = usr / "share"
    (share / "applications").mkdir(parents=True)
    shutil.copyfile(PACKAGING / "linux" / f"{APP_ID}.desktop", share / "applications" / f"{APP_ID}.desktop")
    (share / "metainfo").mkdir()
    date = build_date()
    metainfo = (PACKAGING / "linux" / f"{APP_ID}.metainfo.xml").read_text(encoding="utf-8")
    metainfo = metainfo.replace("@VERSION@", version).replace("@DATE@", date.strftime("%Y-%m-%d"))
    (share / "metainfo" / f"{APP_ID}.metainfo.xml").write_text(metainfo, encoding="utf-8")
    for size in ICON_SIZES:
        icon_dir = share / "icons" / "hicolor" / f"{size}x{size}" / "apps"
        icon_dir.mkdir(parents=True)
        shutil.copyfile(PACKAGING / "icons" / f"rilldisk-{size}.png", icon_dir / f"{APP_ID}.png")
    scalable = share / "icons" / "hicolor" / "scalable" / "apps"
    scalable.mkdir(parents=True)
    shutil.copyfile(PACKAGING / "icons" / "rilldisk.svg", scalable / f"{APP_ID}.svg")
    (share / "rilldisk").mkdir()
    (share / "rilldisk" / "installed-by").write_text(f"{kind}\n", encoding="utf-8")
    docs = share / "doc" / "rilldisk"
    docs.mkdir(parents=True)
    for item in notices(stage):
        (shutil.copytree if item.is_dir() else shutil.copyfile)(item, docs / item.name)
    return usr


def normalize_modes(root):
    for path in root.rglob("*"):
        if path.is_dir():
            os.chmod(path, 0o755)
        elif path.parent.name != "bin" and path.name != "AppRun":
            os.chmod(path, 0o644)


def deb(stage, out, version, target, args, glibc, libraries):
    deb_arch, _ = LINUX_ARCH[target]
    deb_version = tilde(version)
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp) / "root"
        usr = linux_tree(stage, root, version, "deb")
        docs = usr / "share" / "doc" / "rilldisk"
        # Debian policy: machine-readable copyright; AGPL is not in common-licenses.
        license_text = (stage / "LICENSE").read_text(encoding="utf-8")
        (docs / "copyright").write_text(
            "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n"
            f"Upstream-Name: RillDisk\nSource: {SOURCE}\n\n"
            "Files: *\nCopyright: RillDisk contributors\nLicense: AGPL-3.0-only\n"
            " Third-party components and their licenses are listed in\n"
            " /usr/share/doc/rilldisk/THIRD-PARTY-LICENSES.json.\n\n"
            "License: AGPL-3.0-only\n"
            + "".join(f" {line}\n" if line.strip() else " .\n" for line in license_text.splitlines()),
            encoding="utf-8")
        (docs / "LICENSE").unlink()
        changelog = (f"rilldisk ({deb_version}) stable; urgency=medium\n\n"
                     f"  * Release {version}: {SOURCE}/releases/tag/{args.tag}\n\n"
                     f" -- {MAINTAINER}  {build_date().strftime('%a, %d %b %Y %H:%M:%S +0000')}\n")
        with gzip.GzipFile(docs / "changelog.gz", "wb", compresslevel=9, mtime=0) as gz:
            gz.write(changelog.encode())
        normalize_modes(root)
        size_kb = sum(p.stat().st_size for p in root.rglob("*") if p.is_file()) // 1024
        debian = root / "DEBIAN"
        debian.mkdir()
        description = "\n".join(f" {line}" for line in DESCRIPTION.splitlines())
        # zig-linked builds need only glibc; add libgcc if a build links it.
        depends = f"libc6 (>= {glibc})"
        if "libgcc_s.so.1" in libraries:
            depends += ", libgcc-s1 | libgcc1"
        (debian / "control").write_text(
            f"Package: rilldisk\nVersion: {deb_version}\nArchitecture: {deb_arch}\n"
            f"Maintainer: {MAINTAINER}\nInstalled-Size: {size_kb}\n"
            f"Depends: {depends}\n"
            f"Recommends: {DEB_RECOMMENDS}\nSuggests: xdg-desktop-portal\n"
            f"Section: utils\nPriority: optional\nHomepage: {SOURCE}\n"
            f"Description: {SUMMARY}\n{description}\n",
            encoding="utf-8")
        sums = []
        for path in sorted(p for p in root.rglob("*") if p.is_file() and debian not in p.parents):
            sums.append(f"{hashlib.md5(path.read_bytes()).hexdigest()}  {path.relative_to(root).as_posix()}")
        (debian / "md5sums").write_text("\n".join(sums) + "\n", encoding="utf-8")
        os.chmod(debian / "control", 0o644)
        os.chmod(debian / "md5sums", 0o644)
        name = out / f"rilldisk_{deb_version}_{deb_arch}.deb"
        run("dpkg-deb", "--root-owner-group", "-Zxz", "--build", root, name)
    run("dpkg-deb", "--info", name)
    record(out, "deb", target, name, args)


def rpm(stage, out, version, target, args):
    _, rpm_arch = LINUX_ARCH[target]
    rpm_version = tilde(version)
    with tempfile.TemporaryDirectory() as tmp:
        tmp = pathlib.Path(tmp)
        root = tmp / "root"
        linux_tree(stage, root, version, "rpm")
        normalize_modes(root)
        # RPM keeps licenses under /usr/share/licenses.
        licenses = root / "usr" / "share" / "licenses" / "rilldisk"
        licenses.mkdir(parents=True)
        shutil.move(root / "usr" / "share" / "doc" / "rilldisk" / "LICENSE", licenses / "LICENSE")
        recommends = "\n".join(f"Recommends: {r}" for r in RPM_RECOMMENDS)
        spec = tmp / "rilldisk.spec"
        spec.write_text(f"""Name: rilldisk
Version: {rpm_version}
Release: 1
Summary: {SUMMARY}
License: AGPL-3.0-only
URL: {SOURCE}
{recommends}
# Prebuilt, already stripped release binaries.
%global debug_package %{{nil}}
%global __strip /bin/true
%global _build_id_links none

%description
{DESCRIPTION}

%install
cp -a {root}/. %{{buildroot}}/

%files
%license /usr/share/licenses/rilldisk/LICENSE
/usr/bin/rilldisk
/usr/bin/rilldisk-desktop
/usr/share/applications/{APP_ID}.desktop
/usr/share/metainfo/{APP_ID}.metainfo.xml
/usr/share/icons/hicolor/*/apps/{APP_ID}.*
/usr/share/rilldisk
%doc /usr/share/doc/rilldisk
""", encoding="utf-8")
        rpmbuild = tmp / "rpmbuild"
        run("rpmbuild", "-bb", "--target", rpm_arch, "--define", f"_topdir {rpmbuild}",
            "--define", "_binary_payload w9.xzdio", spec)
        (built,) = (rpmbuild / "RPMS" / rpm_arch).glob("*.rpm")
        name = out / f"rilldisk-{rpm_version}-1.{rpm_arch}.rpm"
        shutil.move(built, name)
    run("rpm", "-qpi", name)
    run("rpm", "-qpR", name)
    record(out, "rpm", target, name, args)


def appimage(stage, out, version, target, args):
    _, arch = LINUX_ARCH[target]
    tool = args.appimagetool or sys.exit("--appimagetool is required for AppImages")
    runtime = args.appimage_runtime or sys.exit("--appimage-runtime is required for AppImages")
    with tempfile.TemporaryDirectory() as tmp:
        appdir = pathlib.Path(tmp) / "RillDisk.AppDir"
        usr = linux_tree(stage, appdir, version, "appimage")
        shutil.copyfile(PACKAGING / "linux" / "AppRun", appdir / "AppRun")
        os.chmod(appdir / "AppRun", 0o755)
        shutil.copyfile(PACKAGING / "linux" / f"{APP_ID}.desktop", appdir / f"{APP_ID}.desktop")
        shutil.copyfile(PACKAGING / "icons" / "rilldisk-256.png", appdir / f"{APP_ID}.png")
        shutil.copyfile(PACKAGING / "icons" / "rilldisk-256.png", appdir / ".DirIcon")
        assert usr.is_dir()
        normalize_modes(appdir)
        name = out / f"RillDisk-{version}-{arch}.AppImage"
        update = f"gh-releases-zsync|lord007tn|rilldisk|latest|RillDisk-*-{arch}.AppImage.zsync"
        env = dict(os.environ, ARCH=arch, APPIMAGE_EXTRACT_AND_RUN="1")
        run(tool, "--no-appstream", "--runtime-file", runtime, "-u", update, appdir, name,
            env=env, cwd=out)
    record(out, "appimage", target, name, args)
    zsync = out / f"{name.name}.zsync"
    if zsync.is_file():
        record(out, "zsync", target, zsync, args)


def linux(args):
    version = version_of(args.tag)
    out = pathlib.Path(args.out).resolve()
    formats = set(args.formats.split(","))
    with tempfile.TemporaryDirectory() as tmp:
        stage = unpack(out, args.tag, args.target, pathlib.Path(tmp))
        glibc, libraries = linked([stage / "rilldisk", stage / "rilldisk-desktop"])
        print(f"programs need glibc >= {glibc}; link {sorted(libraries)}")
        if "deb" in formats:
            deb(stage, out, version, args.target, args, glibc, libraries)
        if "rpm" in formats:
            rpm(stage, out, version, args.target, args)
        if "appimage" in formats:
            appimage(stage, out, version, args.target, args)


# --------------------------------------------------------------------- macOS

MAC_ARCH = {"aarch64-apple-darwin": ("arm64", "11.0"), "x86_64-apple-darwin": ("x64", "10.15")}


def macos(args):
    version = version_of(args.tag)
    arch, minimum = MAC_ARCH[args.target]
    out = pathlib.Path(args.out).resolve()
    with tempfile.TemporaryDirectory() as tmp:
        tmp = pathlib.Path(tmp)
        stage = unpack(out, args.tag, args.target, tmp / "stage")
        volume = tmp / "volume"
        app = volume / "RillDisk.app" / "Contents"
        (app / "MacOS").mkdir(parents=True)
        (app / "Resources").mkdir()
        for program in ("rilldisk-desktop", "rilldisk"):
            shutil.copyfile(stage / program, app / "MacOS" / program)
            os.chmod(app / "MacOS" / program, 0o755)
        plist = (PACKAGING / "macos" / "Info.plist").read_text(encoding="utf-8")
        plist = (plist.replace("@SHORT_VERSION@", version.split("-")[0])
                 .replace("@VERSION@", version).replace("@MIN_MACOS@", minimum))
        (app / "Info.plist").write_text(plist, encoding="utf-8")
        (app / "PkgInfo").write_text("APPL????", encoding="ascii")
        shutil.copyfile(PACKAGING / "icons" / "rilldisk.icns", app / "Resources" / "rilldisk.icns")
        for item in notices(stage):
            (shutil.copytree if item.is_dir() else shutil.copyfile)(item, app / "Resources" / item.name)
        run("plutil", "-lint", app / "Info.plist")
        # Ad-hoc signature: required to run on Apple silicon and keeps the
        # bundle sealed. Not notarized (needs a paid Apple Developer ID).
        run("codesign", "--force", "--sign", "-", "--timestamp=none", app / "MacOS" / "rilldisk")
        run("codesign", "--force", "--sign", "-", "--timestamp=none", volume / "RillDisk.app")
        run("codesign", "--verify", "--deep", "--strict", "--verbose=2", volume / "RillDisk.app")
        os.symlink("/Applications", volume / "Applications")
        (volume / "Command line.txt").write_text(
            "RillDisk's command line and MCP server live inside the app. To use them\n"
            "from a terminal, after copying RillDisk to Applications:\n\n"
            "  sudo mkdir -p /usr/local/bin\n"
            "  sudo ln -sf /Applications/RillDisk.app/Contents/MacOS/rilldisk /usr/local/bin/rilldisk\n\n"
            "This build is not notarized. If macOS says RillDisk cannot be opened,\n"
            "open System Settings > Privacy & Security and choose Open Anyway, or run:\n\n"
            "  xattr -dr com.apple.quarantine /Applications/RillDisk.app\n",
            encoding="utf-8")
        name = out / f"RillDisk-{version}-{arch}.dmg"
        if name.exists():
            name.unlink()
        run("hdiutil", "create", "-volname", f"RillDisk {version}", "-srcfolder", volume,
            "-fs", "HFS+", "-format", "UDZO", "-imagekey", "zlib-level=9", "-ov", name)
    run("hdiutil", "verify", name)
    record(out, "dmg", args.target, name, args)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    commands = {"msi": msi, "linux": linux, "macos": macos}
    for name in commands:
        p = sub.add_parser(name)
        p.add_argument("--target", required=True)
        p.add_argument("--tag", required=True)
        p.add_argument("--commit", required=True)
        p.add_argument("--out", default="dist")
        if name == "msi":
            p.add_argument("--wix", help="path to wix.exe (default: on PATH)")
        if name == "linux":
            p.add_argument("--formats", default="deb,rpm,appimage")
            p.add_argument("--appimagetool")
            p.add_argument("--appimage-runtime")
    args = parser.parse_args()
    commands[args.command](args)


if __name__ == "__main__":
    main()
