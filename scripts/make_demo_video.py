"""Turn a recorded capture tour into a captioned MP4 and a small GIF.

    rilldisk-desktop "C:\\RillDisk Demo" --capture shots --tour video
    python scripts/make_demo_video.py shots --ffmpeg PATH --mp4 docs/media/tour.mp4 --gif docs/media/tour.gif

Frames come from `shots/frames` (one PNG per three app frames), captions from
`shots/captions.json` (first frame of each caption, written by the tour). Both
are played at 20 frames a second, so timing is the tour's own, not real time.
Needs Pillow; the MP4 needs an ffmpeg with libx264.
"""
import argparse
import json
import pathlib
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageFont

FPS = 20


def font(size):
    for name in ["segoeuisb.ttf", "segoeui.ttf", "DejaVuSans.ttf", "Arial.ttf"]:
        for folder in [pathlib.Path("C:/Windows/Fonts"), pathlib.Path("/usr/share/fonts/truetype/dejavu"),
                       pathlib.Path("/Library/Fonts")]:
            if (folder / name).exists():
                return ImageFont.truetype(str(folder / name), size)
    return ImageFont.load_default()


def caption_at(captions, frame):
    text = ""
    for item in captions:
        if item["frame"] <= frame:
            text = item["text"]
    return text


def burn(image, text, face):
    """A dark band at the bottom with the caption centred in it."""
    if not text:
        return image
    image = image.convert("RGB")
    w, h = image.size
    band = int(h * 0.085)
    overlay = Image.new("RGBA", (w, band), (8, 10, 16, 215))
    image.paste(overlay, (0, h - band), overlay)
    draw = ImageDraw.Draw(image)
    left, top, right, bottom = draw.textbbox((0, 0), text, font=face)
    draw.text(((w - (right - left)) / 2, h - band + (band - (bottom - top)) / 2 - top), text,
              font=face, fill=(240, 244, 248))
    return image


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("shots", type=pathlib.Path)
    ap.add_argument("--ffmpeg")
    ap.add_argument("--mp4", type=pathlib.Path)
    ap.add_argument("--gif", type=pathlib.Path)
    ap.add_argument("--width", type=int, default=1600)
    args = ap.parse_args()
    frames = sorted((args.shots / "frames").glob("frame_*.png"))
    if not frames:
        sys.exit(f"no frames in {args.shots / 'frames'}")
    captions = json.loads((args.shots / "captions.json").read_text(encoding="utf-8"))
    print(f"{len(frames)} frames, {len(frames) / FPS:.1f} s at {FPS} fps")
    first = Image.open(frames[0])
    height = round(first.height * args.width / first.width / 2) * 2
    face = font(int(height * 0.032))
    with tempfile.TemporaryDirectory() as tmp:
        tmp = pathlib.Path(tmp)
        gif = []
        for i, path in enumerate(frames):
            image = Image.open(path).convert("RGB").resize((args.width, height), Image.LANCZOS)
            image = burn(image, caption_at(captions, i), face)
            image.save(tmp / f"f_{i:05}.png")
            if args.gif and i % 2 == 0:
                gif.append(image.resize((args.width // 2, height // 2), Image.LANCZOS)
                           .convert("P", palette=Image.ADAPTIVE, colors=128))
        if args.mp4:
            if not args.ffmpeg:
                sys.exit("--mp4 needs --ffmpeg")
            args.mp4.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run([args.ffmpeg, "-y", "-loglevel", "error", "-framerate", str(FPS),
                            "-i", str(tmp / "f_%05d.png"), "-c:v", "libx264", "-preset", "slow",
                            "-crf", "22", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
                            str(args.mp4)], check=True)
            print(f"wrote {args.mp4} ({args.mp4.stat().st_size / 1e6:.1f} MB)")
        if args.gif:
            args.gif.parent.mkdir(parents=True, exist_ok=True)
            gif[0].save(args.gif, save_all=True, append_images=gif[1:], duration=1000 * 2 // FPS,
                        loop=0, optimize=True)
            print(f"wrote {args.gif} ({args.gif.stat().st_size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
