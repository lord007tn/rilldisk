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
    for f in sorted(OUT.iterdir()):
        print(f"{f.stat().st_size:>8}  {f.relative_to(REPO).as_posix()}")


if __name__ == "__main__":
    main()
