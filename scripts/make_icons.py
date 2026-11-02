"""Render the RillDisk app icon for every platform from one definition.

    python scripts/make_icons.py

Writes packaging/icons/: the SVG source, PNGs for Linux and the window icon,
rilldisk.ico (Windows executables and the MSI) and rilldisk.icns (macOS app).
The geometry mirrors docs/media/logo.svg on a 128-unit grid; edit both
together. Needs Pillow. The outputs are committed so builds need neither.
"""
import pathlib
import shutil

from PIL import Image, ImageDraw

REPO = pathlib.Path(__file__).resolve().parent.parent
OUT = REPO / "packaging" / "icons"

# (x, y, width, height, radius, color) on a 128 x 128 grid, as in logo.svg.
BACKGROUND = (0, 0, 128, 128, 33, "#131B26")
TILES = [
    (23, 23, 47, 82, 9, "#30DEC4"),
    (77, 23, 28, 36, 8, "#3B8DF7"),
    (77, 65, 28, 19, 7, "#6366F1"),
    (77, 91, 28, 14, 6, "#C084FC"),
]
SUPERSAMPLE = 8


def render(size, margin=0.0):
    """The icon at `size` px. `margin` is the transparent border as a fraction
    of `size` (macOS icons sit inside a 824/1024 safe area)."""
    big = size * SUPERSAMPLE
    image = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    inset = big * margin
    unit = (big - 2 * inset) / 128
    for x, y, w, h, r, color in [BACKGROUND, *TILES]:
        box = [inset + x * unit, inset + y * unit, inset + (x + w) * unit - 1, inset + (y + h) * unit - 1]
        draw.rounded_rectangle(box, radius=r * unit, fill=color)
    return image.resize((size, size), Image.LANCZOS)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(REPO / "docs" / "media" / "logo.svg", OUT / "rilldisk.svg")
    for size in (16, 24, 32, 48, 64, 128, 256, 512):
        render(size).save(OUT / f"rilldisk-{size}.png", optimize=True)
    ico_sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    render(256).save(OUT / "rilldisk.ico", sizes=[(s, s) for s in ico_sizes],
                     append_images=[render(s) for s in ico_sizes])
    # 100 / 1024 margin on each side: Apple's grid for Big Sur style app icons.
    render(1024, margin=100 / 1024).save(OUT / "rilldisk.icns")
    installer_art()
    for f in sorted([*OUT.iterdir(), *WINDOWS.glob("*.bmp")]):
        print(f"{f.stat().st_size:>8}  {f.relative_to(REPO).as_posix()}")


WINDOWS = REPO / "packaging" / "windows"
INK = (11, 16, 24)


def installer_art():
    """Setup wizard art for the MSI (WixUI sizes): the dialog's left panel and
    the top banner. Windows Installer draws text on the white areas."""
    # Welcome / finish pages: 493 x 312, art in the left 164 px.
    dialog = Image.new("RGB", (493, 312), "white")
    panel = Image.new("RGB", (164, 312), INK)
    draw = ImageDraw.Draw(panel)
    # A soft band of the brand's tile colors along the bottom.
    for i, (x0, x1, color) in enumerate([(0, 70, "#30DEC4"), (74, 110, "#3B8DF7"),
                                         (114, 140, "#6366F1"), (144, 164, "#C084FC")]):
        draw.rounded_rectangle([x0 - (8 if i == 0 else 0), 284, x1 + (8 if i == 3 else 0), 330], radius=6, fill=color)
    logo = render(96)
    panel.paste(logo, ((164 - 96) // 2, 92), logo)
    dialog.paste(panel, (0, 0))
    dialog.save(WINDOWS / "dialog.bmp")
    # Inner pages: 493 x 58 banner, title text on the left, logo on the right.
    banner = Image.new("RGB", (493, 58), "white")
    logo = render(40)
    banner.paste(logo, (493 - 40 - 12, 9), logo)
    banner.save(WINDOWS / "banner.bmp")


if __name__ == "__main__":
    main()
