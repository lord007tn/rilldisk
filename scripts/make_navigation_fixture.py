"""Extend the fictional demo fixture with the hard cases of folder navigation.

Adds long names, hundreds of siblings, a deep chain, an empty folder and an
unreadable folder. Nothing here comes from a real machine. Usage:

    python scripts/make_demo_fixture.py <dir> --scale 0.01
    python scripts/make_navigation_fixture.py <dir>
    python scripts/make_navigation_fixture.py <dir> --unlock    # before deleting <dir>

Scan a short path such as C:\\RillDemo\\home so captures show no account name.
"""
import getpass
import os
import pathlib
import subprocess
import sys

LONG_NAME = (
    "quarterly-brand-campaign-final-approved-exports-for-regional-print-partners-"
    "and-archive-review-2026-revision-14"
)


def small_file(path, size):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as f:
        f.write(b"\0" * size)


def lock(path, deny):
    """Windows only: refuse (or restore) listing `path` for the current user."""
    user = getpass.getuser()
    args = ["/deny", f"{user}:(RX)"] if deny else ["/remove:d", user]
    subprocess.run(["icacls", str(path), *args], check=True, stdout=subprocess.DEVNULL)


def main():
    root = pathlib.Path(sys.argv[1])
    archive = root / "Archive"
    vault = archive / "locked-vault"
    if "--unlock" in sys.argv:
        if os.name == "nt" and vault.exists():
            lock(vault, deny=False)
        return

    # Long names: one folder, one nested folder, one file.
    small_file(archive / LONG_NAME / f"{LONG_NAME}.psd", 900_000)
    small_file(archive / LONG_NAME / "print-partners" / "north-region.pdf", 400_000)

    # Many siblings, with sizes that give each a distinct place in the list.
    for i in range(1, 241):
        small_file(archive / "client-deliverables" / f"project-{i:04}" / "brief.pdf", 2_000 + i * 700)

    # Deep chain: more levels than a breadcrumb can show.
    deep = root / "Projects" / "atlas-web" / "packages" / "design-system" / "src"
    deep = deep / "components" / "forms" / "inputs" / "date-picker"
    small_file(deep / "calendar.tsx", 120_000)

    # Empty and unreadable folders.
    (archive / "empty-placeholder").mkdir(parents=True, exist_ok=True)
    small_file(vault / "sealed.bin", 64_000)
    if os.name == "nt":
        lock(vault, deny=True)


if __name__ == "__main__":
    main()
