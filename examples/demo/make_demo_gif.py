#!/usr/bin/env python3
"""Render ``demo.gif`` from the **real** output of the replay demo.

This runs the two documented ``sensorwatch watch`` commands, captures their
actual stdout and exit codes, and paints an animated terminal recording from
them -- so the GIF can never drift from what the CLI really does. It is a docs
tool, not part of the shipped package; its only dependency is Pillow::

    uv pip install pillow      # or: pip install pillow
    python make_demo_gif.py    # optional: pass the sensorwatch binary path

Run it from this directory (``examples/demo``) with the CLI built.
"""
from __future__ import annotations

import glob
import os
import shutil
import subprocess
import sys
import textwrap
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]

# --- theme -----------------------------------------------------------------
BG = (30, 30, 46)          # window body
BAR = (44, 44, 62)         # title bar
FG = (205, 214, 244)       # default text
GREEN = (166, 227, 161)    # prompt
TEAL = (137, 220, 235)     # comments
BLUE = (137, 180, 250)     # command text
SLATE = (147, 153, 178)    # json output / dim
AMBER = (249, 226, 175)    # exit note
DOTS = [(237, 135, 150), (249, 226, 175), (166, 227, 161)]

FONT_SIZE = 22
PAD = 28
BAR_H = 44
LINE_H = FONT_SIZE + 10
MAX_COLS = 82  # soft-wrap width in characters


def load_font(size: int) -> ImageFont.FreeTypeFont:
    for name in ("CascadiaMono.ttf", "CascadiaCode.ttf", "consola.ttf", "lucon.ttf"):
        p = Path("C:/Windows/Fonts") / name
        if p.exists():
            return ImageFont.truetype(str(p), size)
    # load_default(size) returns a sized FreeTypeFont (Pillow >= 10.1), so the
    # fallback matches the return type and still honors the requested size.
    return ImageFont.load_default(size)


FONT = load_font(FONT_SIZE)
BAR_FONT = load_font(FONT_SIZE - 4)


def find_binary(argv: list[str]) -> str:
    if len(argv) > 1:
        return argv[1]
    exe = "sensorwatch.exe" if os.name == "nt" else "sensorwatch"
    cand = REPO / "rust" / "target" / "release" / exe
    if cand.exists():
        return str(cand)
    found = shutil.which("sensorwatch")
    if found:
        return found
    sys.exit("sensorwatch binary not found; build it or pass its path as an arg.")


def run(binary: str, args: list[str]) -> tuple[str, int]:
    proc = subprocess.run(
        [binary, *args], cwd=HERE, capture_output=True, text=True
    )
    return proc.stdout.strip(), proc.returncode


def wrap(text: str, color) -> list[tuple[str, tuple]]:
    """Soft-wrap one logical line into rendered (text, color) rows."""
    rows = textwrap.wrap(text, MAX_COLS) or [""]
    return [(r, color) for r in rows]


def build_transcript(binary: str) -> list[tuple[str, tuple]]:
    # Real run #1: one-shot -> stdout event + exit code.
    shutil.rmtree(HERE / "logs", ignore_errors=True)
    event, rc1 = run(binary, ["watch", "--config", "demo.toml",
                              "--replay", "sensors_demo.jsonl"])
    # Real run #2: follow -> events file with fired + cleared.
    shutil.rmtree(HERE / "logs", ignore_errors=True)
    run(binary, ["watch", "--config", "demo.toml",
                 "--replay", "sensors_demo.jsonl", "--follow"])
    events = []
    for f in sorted(glob.glob(str(HERE / "logs" / "events_*.jsonl"))):
        events += [ln for ln in Path(f).read_text().splitlines() if ln.strip()]
    shutil.rmtree(HERE / "logs", ignore_errors=True)

    rows: list[tuple[str, tuple]] = []
    rows += [("# One command, no hardware, any OS -- watch a rule fire:", TEAL)]
    rows += [("$ sensorwatch watch --config demo.toml --replay sensors_demo.jsonl", GREEN)]
    rows += wrap(event, SLATE)
    # Exit 10 is the "a rule fired" contract; don't assert it if the command
    # exited any other way (a broken demo must not render as a passing one).
    note = "a rule fired" if rc1 == 10 else "no rule fired"
    rows += [(f"exit {rc1}  -- {note}", AMBER)]
    rows += [("", FG)]
    rows += [("# Add --follow for the full fire -> clear lifecycle:", TEAL)]
    rows += [("$ sensorwatch watch ... --follow  &&  cat logs/events_*.jsonl", GREEN)]
    for ev in events:
        rows += wrap(ev, SLATE)
    return rows


def paint(rows: list[tuple[str, tuple]], reveal: int, cursor: bool) -> Image.Image:
    width = PAD * 2 + int(FONT.getlength("M") * MAX_COLS)
    height = BAR_H + PAD * 2 + LINE_H * len(rows)
    img = Image.new("RGB", (width, height), BG)
    d = ImageDraw.Draw(img)
    # title bar
    d.rectangle([0, 0, width, BAR_H], fill=BAR)
    for i, c in enumerate(DOTS):
        cx = PAD + i * 26
        d.ellipse([cx, BAR_H // 2 - 7, cx + 14, BAR_H // 2 + 7], fill=c)
    d.text((width // 2, BAR_H // 2), "sensorwatch replay demo",
           font=BAR_FONT, fill=SLATE, anchor="mm")
    # body
    y = BAR_H + PAD
    for text, color in rows[:reveal]:
        d.text((PAD, y), text, font=FONT, fill=color)
        y += LINE_H
    if cursor and reveal <= len(rows):
        yb = BAR_H + PAD + LINE_H * (reveal - 1)
        # cursor sits after the last revealed row
        last = rows[reveal - 1][0] if reveal else ""
        x = PAD + FONT.getlength(last) + 4
        d.rectangle([x, yb + 2, x + FONT_SIZE * 0.55, yb + FONT_SIZE + 2], fill=FG)
    return img


def main() -> None:
    binary = find_binary(sys.argv)
    rows = build_transcript(binary)

    frames: list[Image.Image] = []
    durations: list[int] = []

    def add(img: Image.Image, ms: int) -> None:
        frames.append(img)
        durations.append(ms)

    # progressive reveal, line by line, with a blinking cursor
    for r in range(1, len(rows) + 1):
        text = rows[r - 1][0]
        is_cmd = text.startswith("$")
        add(paint(rows, r, cursor=True), 550 if is_cmd else 260)
        if is_cmd:  # brief blink to feel like a prompt before output appears
            add(paint(rows, r, cursor=False), 180)
    # hold the finished frame, blinking, so viewers can read it
    for _ in range(4):
        add(paint(rows, len(rows), cursor=True), 600)
        add(paint(rows, len(rows), cursor=False), 600)

    out = HERE / "demo.gif"
    frames[0].save(
        out, save_all=True, append_images=frames[1:], duration=durations,
        loop=0, optimize=True, disposal=2,
    )
    kb = out.stat().st_size / 1024
    print(f"wrote {out} ({kb:.0f} KB, {len(frames)} frames)")


if __name__ == "__main__":
    main()
