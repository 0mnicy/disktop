#!/usr/bin/env python3
"""Records the animations in the README.

Each scene is played by the `record_demo` test in `src/ui.rs`, which draws
the demo with the app's own UI code on a simulated clock and writes out
every frame. This script paints those frames the way a terminal would and
encodes them as a GIF in `assets/`. The same commit always gives the same
frames.

Needs Python 3.9+, Pillow (`pip install pillow`), ffmpeg on the PATH, and a
monospace font with box-drawing characters, such as Cascadia Mono, which
the committed GIFs use. Run from anywhere in the repository:

    python scripts/record.py            # every scene
    python scripts/record.py arrows     # just one
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

from PIL import Image, ImageColor, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class Scene:
    name: str
    title: str
    cols: int
    rows: int
    # Second of the demo's minute-long script to start at, and how long.
    at: int
    secs: int
    style: str = "dots"


SCENES = [
    Scene("demo", "disktop --demo", cols=140, rows=36, at=8, secs=10),
    Scene(
        "arrows",
        "disktop --demo --style arrows",
        cols=140,
        rows=36,
        at=24,
        secs=8,
        style="arrows",
    ),
]

FPS = 25
FONT_SIZE = 15

# A dark theme in the spirit of GitHub's. Each color's bright variant is
# clearly lighter, since disktop draws a flow's dots in the bright one and
# its track in the plain one.
BACKGROUND = "#0d1117"
FOREGROUND = "#c9d1d9"
COLORS = {
    "Black": "#0d1117",
    "Red": "#c93c37",
    "Green": "#2ea043",
    "Yellow": "#d29922",
    "Blue": "#1f6feb",
    "Magenta": "#bc8cff",
    "Cyan": "#39c5cf",
    "Gray": "#b1bac4",
    "DarkGray": "#6e7681",
    "LightRed": "#ff7b72",
    "LightGreen": "#56d364",
    "LightYellow": "#f8e3a1",
    "LightBlue": "#79c0ff",
    "LightMagenta": "#d2a8ff",
    "LightCyan": "#56d4dd",
    "White": "#f0f6fc",
}
WINDOW_BAR = "#161b22"
WINDOW_EDGE = "#30363d"
BUTTONS = ["#ff5f57", "#febc2e", "#28c840"]

PADDING = 18
BAR_HEIGHT = 34

# Modifier bits, as ratatui numbers them.
BOLD = 1 << 0
REVERSED = 1 << 6

# Fonts to try, regular and bold. A variable font gets its bold from its
# weight axis.
FONTS = [
    ("C:/Windows/Fonts/CascadiaMono.ttf", None),
    ("/usr/share/fonts/truetype/cascadia-code/CascadiaMono.ttf", None),
    (
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
    ),
    ("/System/Library/Fonts/Menlo.ttc", None),
]


def color(name: str, default: str) -> str:
    if name == "Reset":
        return default
    if name.startswith("Rgb("):
        r, g, b = (int(v) for v in name[4:-1].split(","))
        return f"#{r:02x}{g:02x}{b:02x}"
    return COLORS.get(name, default)


class Glyphs:
    """Coverage masks for every character, one cell in size.

    Box-drawing lines and braille dots are drawn rather than taken from the
    font, as terminals do, so lines meet across cells and graphs stay crisp.
    """

    SCALE = 4  # Drawn this much bigger, then shrunk, to smooth the edges.

    def __init__(self, regular: str, bold: str | None):
        self.fonts = {False: self._font(regular, 400), True: self._font(bold or regular, 700)}
        ascent, descent = self.fonts[False].getmetrics()
        self.width = round(self.fonts[False].getlength("M"))
        self.height = ascent + descent + 2
        self.baseline = ascent + 1

    @staticmethod
    def _font(path: str, weight: int) -> ImageFont.FreeTypeFont:
        font = ImageFont.truetype(path, FONT_SIZE)
        try:
            font.set_variation_by_axes([weight])
        except OSError:
            pass  # Not a variable font.
        return font

    @lru_cache(maxsize=None)
    def mask(self, char: str, bold: bool) -> Image.Image | None:
        if char == " ":
            return None
        code = ord(char)
        if 0x2800 <= code <= 0x28FF:
            return self._braille(code - 0x2800)
        if char in LINES:
            return self._line(*LINES[char])
        image = Image.new("L", (self.width, self.height))
        ImageDraw.Draw(image).text(
            (0, self.baseline), char, font=self.fonts[bold], fill=255, anchor="ls"
        )
        return image

    def _canvas(self) -> tuple[Image.Image, ImageDraw.ImageDraw]:
        image = Image.new("L", (self.width * self.SCALE, self.height * self.SCALE))
        return image, ImageDraw.Draw(image)

    def _shrink(self, image: Image.Image) -> Image.Image:
        return image.resize((self.width, self.height), Image.Resampling.BOX)

    def _braille(self, bits: int) -> Image.Image:
        image, draw = self._canvas()
        w, h = image.size
        radius = w * 0.18
        # Dots 1-3 and 7 run down the left, 4-6 and 8 down the right.
        spots = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)]
        for bit, (column, row) in enumerate(spots):
            if bits & (1 << bit):
                x = w * (0.3 + 0.4 * column)
                y = h * (0.125 + 0.25 * row)
                draw.ellipse((x - radius, y - radius, x + radius, y + radius), fill=255)
        return self._shrink(image)

    def _line(self, up: bool, down: bool, left: bool, right: bool, heavy: bool, rounded: bool):
        image, draw = self._canvas()
        s = self.SCALE
        w, h = image.size
        t = s * (3 if heavy else 1)
        # The band a line runs along, centered on the cell's middle pixel.
        bx = (self.width // 2) * s - (t - s) // 2
        by = (self.height // 2) * s - (t - s) // 2

        def bar(x0: float, y0: float, x1: float, y1: float) -> None:
            if x1 > x0 and y1 > y0:
                draw.rectangle((x0, y0, x1 - 1, y1 - 1), fill=255)

        if rounded:
            # A quarter circle from one arm to the other, as wide as it can be.
            xc, yc = bx + t / 2, by + t / 2
            r = min(xc, w - xc, yc, h - yc)
            sx, sy = (1 if right else -1), (1 if down else -1)
            ox, oy = xc + sx * r, yc + sy * r
            reach = r + t / 2
            start = {(1, 1): 180, (-1, 1): 270, (-1, -1): 0, (1, -1): 90}[(sx, sy)]
            draw.arc(
                (ox - reach, oy - reach, ox + reach, oy + reach),
                start, start + 90, fill=255, width=t,
            )
            if down:
                bar(bx, oy, bx + t, h)
            else:
                bar(bx, 0, bx + t, oy)
            if right:
                bar(ox, by, w, by + t)
            else:
                bar(0, by, ox, by + t)
        else:
            if up:
                bar(bx, 0, bx + t, by + t)
            if down:
                bar(bx, by, bx + t, h)
            if left:
                bar(0, by, bx + t, by + t)
            if right:
                bar(bx, by, w, by + t)
        return self._shrink(image)


# Which arms each line character has: up, down, left, right, heavy, rounded.
LINES = {
    "─": (False, False, True, True, False, False),
    "━": (False, False, True, True, True, False),
    "│": (True, True, False, False, False, False),
    "╭": (False, True, False, True, False, True),
    "╮": (False, True, True, False, False, True),
    "╯": (True, False, True, False, False, True),
    "╰": (True, False, False, True, False, True),
    "├": (True, True, False, True, False, False),
    "┤": (True, True, True, False, False, False),
    "┬": (False, True, True, True, False, False),
    "┴": (True, False, True, True, False, False),
    "┼": (True, True, True, True, False, False),
    "╴": (False, False, True, False, False, False),
    "╵": (True, False, False, False, False, False),
    "╶": (False, False, False, True, False, False),
    "╷": (False, True, False, False, False, False),
}


class Painter:
    """Paints frames into a window, redrawing only the cells that changed."""

    def __init__(self, glyphs: Glyphs, cols: int, rows: int, title: str):
        self.glyphs = glyphs
        self.cols, self.rows = cols, rows
        width = cols * glyphs.width + 2 * PADDING
        height = rows * glyphs.height + 2 * PADDING + BAR_HEIGHT
        self.image = Image.new("RGB", (width, height), BACKGROUND)
        self.cells: list[list[tuple] | None] = [None] * rows
        self._window(title)

    def _window(self, title: str) -> None:
        draw = ImageDraw.Draw(self.image)
        width, height = self.image.size
        draw.rectangle((0, 0, width - 1, BAR_HEIGHT - 1), fill=WINDOW_BAR)
        draw.line((0, BAR_HEIGHT - 1, width, BAR_HEIGHT - 1), fill=WINDOW_EDGE)
        draw.rectangle((0, 0, width - 1, height - 1), outline=WINDOW_EDGE)
        for i, button in enumerate(BUTTONS):
            x, y, r = 20 + i * 20, BAR_HEIGHT // 2, 6
            draw.ellipse((x - r, y - r, x + r, y + r), fill=button)
        font = self.glyphs.fonts[False]
        draw.text((width / 2, BAR_HEIGHT / 2), title, font=font, fill=COLORS["DarkGray"], anchor="mm")

    def paint(self, frame: list) -> Image.Image:
        draw = ImageDraw.Draw(self.image)
        g = self.glyphs
        for y, runs in enumerate(frame):
            cells = [
                (char, fg, bg, mods)
                for text, fg, bg, mods in runs
                for char in text
            ]
            before = self.cells[y]
            for x, cell in enumerate(cells):
                if before is not None and before[x] == cell:
                    continue
                char, fg, bg, mods = cell
                fg_color = color(fg, FOREGROUND)
                bg_color = color(bg, BACKGROUND)
                if mods & REVERSED:
                    fg_color, bg_color = bg_color, fg_color
                left = PADDING + x * g.width
                top = BAR_HEIGHT + PADDING + y * g.height
                draw.rectangle((left, top, left + g.width - 1, top + g.height - 1), fill=bg_color)
                mask = g.mask(char, bool(mods & BOLD))
                if mask is not None:
                    self.image.paste(fg_color, (left, top), mask)
            self.cells[y] = cells
        return self.image


def record(scene: Scene, frames_path: Path) -> None:
    """Plays the scene with the `record_demo` test, writing its frames."""
    env = dict(
        os.environ,
        OUT=str(frames_path),
        COLS=str(scene.cols),
        ROWS=str(scene.rows),
        AT=str(scene.at),
        SECS=str(scene.secs),
        FPS=str(FPS),
        STYLE=scene.style,
    )
    subprocess.run(
        ["cargo", "test", "--release", "--quiet", "record_demo", "--", "--ignored"],
        cwd=ROOT,
        env=env,
        check=True,
        stdout=subprocess.DEVNULL,
    )


def palette(frames: list[Image.Image]) -> Image.Image:
    """One palette for the whole GIF, so colors don't shift between frames,
    as the 16 by 16 swatch ffmpeg's `paletteuse` takes. The theme's colors
    are kept exactly; the rest of the palette goes to the shades between
    them that smooth the edges of text."""
    exact = {
        ImageColor.getrgb(c)
        for c in [BACKGROUND, FOREGROUND, WINDOW_BAR, WINDOW_EDGE, *COLORS.values(), *BUTTONS]
    }
    width, height = frames[0].size
    sheet = Image.new("RGB", (width, height * len(frames)))
    for i, frame in enumerate(frames):
        sheet.paste(frame, (0, i * height))
    shades = sheet.quantize(colors=256 - len(exact), method=Image.Quantize.MEDIANCUT)
    colors = [c for rgb in sorted(exact) for c in rgb] + shades.getpalette()
    colors = colors[: 256 * 3]
    swatch = Image.new("P", (16, 16))
    swatch.putpalette(colors + [0] * (256 * 3 - len(colors)))
    swatch.putdata(range(256))
    return swatch.convert("RGB")


def render(scene: Scene, frames_path: Path, out: Path, glyphs: Glyphs) -> None:
    with frames_path.open(encoding="utf-8") as lines:
        header = json.loads(next(lines))
        frames = [json.loads(line) for line in lines]
    painter = Painter(glyphs, header["cols"], header["rows"], scene.title)

    # A palette from a frame each second covers every color that appears.
    samples = []
    for i, frame in enumerate(frames):
        image = painter.paint(frame)
        if i % FPS == 0:
            samples.append(image.copy())
    palette_path = frames_path.with_suffix(".palette.png")
    palette(samples).save(palette_path)

    painter = Painter(glyphs, header["cols"], header["rows"], scene.title)
    width, height = painter.image.size
    encoder = subprocess.Popen(
        [
            "ffmpeg", "-y", "-loglevel", "error",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{width}x{height}",
            "-r", str(header["fps"]), "-i", "-",
            "-i", str(palette_path),
            "-lavfi", "[0:v][1:v]paletteuse=dither=none:diff_mode=rectangle",
            "-loop", "0", str(out),
        ],
        stdin=subprocess.PIPE,
    )
    for frame in frames:
        encoder.stdin.write(painter.paint(frame).tobytes())
    encoder.stdin.close()
    if encoder.wait() != 0:
        sys.exit(f"ffmpeg failed on {out}")


def find_font(regular: str | None, bold: str | None) -> tuple[str, str | None]:
    if regular:
        return regular, bold
    for regular, bold in FONTS:
        if Path(regular).exists():
            return regular, bold
    sys.exit("No monospace font found; pass one with --font.")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("scenes", nargs="*", help="scenes to record (default: all)")
    parser.add_argument("--font", help="a monospace TrueType font")
    parser.add_argument("--bold-font", help="its bold, unless it's a variable font")
    args = parser.parse_args()

    scenes = [s for s in SCENES if not args.scenes or s.name in args.scenes]
    unknown = set(args.scenes) - {s.name for s in SCENES}
    if unknown:
        sys.exit(f"Unknown scene: {', '.join(sorted(unknown))}")

    glyphs = Glyphs(*find_font(args.font, args.bold_font))
    work = ROOT / "target" / "record"
    work.mkdir(parents=True, exist_ok=True)
    (ROOT / "assets").mkdir(exist_ok=True)
    for scene in scenes:
        frames_path = work / f"{scene.name}.jsonl"
        out = ROOT / "assets" / f"{scene.name}.gif"
        print(f"{scene.name}: recording", flush=True)
        record(scene, frames_path)
        print(f"{scene.name}: rendering", flush=True)
        render(scene, frames_path, out, glyphs)
        print(f"{scene.name}: {out.relative_to(ROOT)} ({out.stat().st_size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
