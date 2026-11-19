#!/usr/bin/env python3
"""Render a tmux capture-pane ANSI dump to a PNG.

The demo capture script uses tmux so the screenshot does not depend on an X
server or a specific terminal emulator. tmux provides the styled grid; this
script draws that grid with Pillow.

Font discovery is deliberately configurable. The demo was tuned with GeistMono
Nerd Font Mono, DejaVu Sans Mono, DejaVu Sans, and Noto Sans Math. Set the
DUX_DEMO_FONT* environment variables to override the choices.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import unicodedata
from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:
    raise SystemExit("Pillow is required: python3 -m pip install Pillow")

FONT_SIZE = 20
CELL_H = 27
APP_BG = (20, 20, 20)
DEFAULT_FG = (210, 210, 210)

# One Dark-ish 16-color palette. dux's default theme uses named ANSI colors for
# accents (cyan), status (yellow), and diffs (green/red).
ANSI16 = [
    (40, 44, 52),
    (224, 108, 117),
    (152, 195, 121),
    (229, 192, 123),
    (97, 175, 239),
    (198, 120, 221),
    (86, 182, 194),
    (171, 178, 191),
    (92, 99, 112),
    (224, 108, 117),
    (152, 195, 121),
    (229, 192, 123),
    (97, 175, 239),
    (198, 120, 221),
    (86, 182, 194),
    (255, 255, 255),
]


def color_256(index: int) -> Tuple[int, int, int]:
    if index < 16:
        return ANSI16[index]
    if index < 232:
        index -= 16
        r, g, b = index // 36, (index % 36) // 6, index % 6

        def ramp(value: int) -> int:
            return 0 if value == 0 else 55 + value * 40

        return ramp(r), ramp(g), ramp(b)
    value = 8 + (index - 232) * 10
    return value, value, value


@dataclass
class Attr:
    fg: Tuple[int, int, int] = DEFAULT_FG
    bg: Optional[Tuple[int, int, int]] = None
    bold: bool = False
    dim: bool = False
    italic: bool = False
    underline: bool = False

    def reset(self) -> None:
        self.fg = DEFAULT_FG
        self.bg = None
        self.bold = False
        self.dim = False
        self.italic = False
        self.underline = False

    def copy(self) -> "Attr":
        return Attr(
            fg=self.fg,
            bg=self.bg,
            bold=self.bold,
            dim=self.dim,
            italic=self.italic,
            underline=self.underline,
        )


def apply_sgr(attr: Attr, params: str) -> None:
    if not params:
        attr.reset()
        return
    values = [int(value) if value else 0 for value in params.split(";")]
    i = 0
    while i < len(values):
        value = values[i]
        if value == 0:
            attr.reset()
        elif value == 1:
            attr.bold = True
        elif value == 2:
            attr.dim = True
        elif value == 3:
            attr.italic = True
        elif value == 4:
            attr.underline = True
        elif value == 22:
            attr.bold = False
            attr.dim = False
        elif value == 23:
            attr.italic = False
        elif value == 24:
            attr.underline = False
        elif 30 <= value <= 37:
            attr.fg = color_256(value - 30)
        elif 90 <= value <= 97:
            attr.fg = color_256(value - 90 + 8)
        elif 40 <= value <= 47:
            attr.bg = color_256(value - 40)
        elif 100 <= value <= 107:
            attr.bg = color_256(value - 100 + 8)
        elif value == 39:
            attr.fg = DEFAULT_FG
        elif value == 49:
            attr.bg = None
        elif value in (38, 48):
            if i + 1 >= len(values):
                break
            mode = values[i + 1]
            if mode == 5 and i + 2 < len(values):
                color = color_256(values[i + 2])
                if value == 38:
                    attr.fg = color
                else:
                    attr.bg = color
                i += 2
            elif mode == 2 and i + 4 < len(values):
                color = (values[i + 2], values[i + 3], values[i + 4])
                if value == 38:
                    attr.fg = color
                else:
                    attr.bg = color
                i += 4
        i += 1


CSI = re.compile(r"\x1b\[([0-9;]*)m")


def parse_rows(text: str) -> List[List[Tuple[str, Attr]]]:
    rows: List[List[Tuple[str, Attr]]] = []
    for line in text.split("\n"):
        if line.endswith("\r"):
            line = line[:-1]
        attr = Attr()
        cells: List[Tuple[str, Attr]] = []
        cursor = 0
        for match in CSI.finditer(line):
            for char in line[cursor : match.start()]:
                cells.append((char, attr.copy()))
            apply_sgr(attr, match.group(1))
            cursor = match.end()
        for char in line[cursor:]:
            cells.append((char, attr.copy()))
        rows.append(cells)
    while rows and not rows[-1]:
        rows.pop()
    return rows


def cell_width(char: str) -> int:
    return 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1


def mix(
    fg: Tuple[int, int, int], bg: Tuple[int, int, int], amount: float
) -> Tuple[int, int, int]:
    return tuple(
        round(f * amount + b * (1 - amount)) for f, b in zip(fg, bg)
    )


def first_existing(paths: List[Optional[str]]) -> Optional[str]:
    for path in paths:
        if path and os.path.isfile(os.path.expanduser(path)):
            return os.path.expanduser(path)
    return None


def fc_match(name: str) -> Optional[str]:
    if not shutil.which("fc-match"):
        return None
    try:
        output = subprocess.check_output(
            ["fc-match", "-f", "%{file}", name],
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        return None
    path = output.decode("utf-8", "replace").strip()
    return path if os.path.isfile(path) else None


def resolve_font(
    env_name: str,
    fc_names: List[str],
    candidates: List[Optional[str]],
) -> Optional[str]:
    override = os.environ.get(env_name)
    if override:
        if not os.path.isfile(os.path.expanduser(override)):
            raise SystemExit(f"{env_name} points to a missing font: {override}")
        return os.path.expanduser(override)
    for name in fc_names:
        path = fc_match(name)
        if path:
            return path
    return first_existing(candidates)


def load_fonts() -> Dict[str, ImageFont.FreeTypeFont]:
    regular_path = resolve_font(
        "DUX_DEMO_FONT",
        ["GeistMono Nerd Font Mono", "DejaVu Sans Mono", "Menlo", "Noto Sans Mono"],
        [
            "~/.fonts/GeistMono/GeistMonoNerdFontMono-Regular.otf",
            "~/Library/Fonts/GeistMonoNerdFontMono-Regular.otf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/Supplemental/Menlo.ttc",
        ],
    )
    if regular_path is None:
        raise SystemExit(
            "No usable monospace font found. Set DUX_DEMO_FONT to a font path."
        )

    bold_path = resolve_font(
        "DUX_DEMO_FONT_BOLD",
        ["GeistMono Nerd Font Mono:bold", "DejaVu Sans Mono:bold", "Menlo:bold"],
        [
            "~/.fonts/GeistMono/GeistMonoNerdFontMono-Bold.otf",
            "~/Library/Fonts/GeistMonoNerdFontMono-Bold.otf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
            regular_path,
        ],
    ) or regular_path

    symbol_path = resolve_font(
        "DUX_DEMO_SYMBOL_FONT",
        ["DejaVu Sans Mono", "Apple Symbols", "Menlo"],
        [
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/System/Library/Fonts/Apple Symbols.ttf",
            regular_path,
        ],
    ) or regular_path

    symbol_sans_path = resolve_font(
        "DUX_DEMO_SYMBOL_SANS_FONT",
        ["DejaVu Sans", "Apple Symbols"],
        [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/System/Library/Fonts/Apple Symbols.ttf",
            symbol_path,
        ],
    ) or symbol_path

    math_path = resolve_font(
        "DUX_DEMO_MATH_FONT",
        ["Noto Sans Math", "Apple Symbols", "DejaVu Sans"],
        [
            "/usr/share/fonts/truetype/noto/NotoSansMath-Regular.ttf",
            "/System/Library/Fonts/Apple Symbols.ttf",
            symbol_sans_path,
        ],
    ) or symbol_sans_path

    return {
        "regular": ImageFont.truetype(regular_path, FONT_SIZE),
        "bold": ImageFont.truetype(bold_path, FONT_SIZE),
        "symbol": ImageFont.truetype(symbol_path, FONT_SIZE),
        "symbol_sans": ImageFont.truetype(symbol_sans_path, FONT_SIZE),
        "math": ImageFont.truetype(math_path, FONT_SIZE),
    }


# GeistMono is a good text face but its Unicode coverage stops short of a few
# symbols dux uses. PIL does not perform fontconfig fallback, so route those
# glyphs through a known-good face and center them inside the cell.
DEJAVU_MONO_SYMBOLS = set("▾⇄↳◐✻⠏●")


def font_for_char(char: str, attr: Attr, fonts: Dict[str, ImageFont.FreeTypeFont]):
    if char == "⧉":
        return fonts["math"], True
    if char == "⎇":
        return fonts["symbol_sans"], True
    if char in DEJAVU_MONO_SYMBOLS:
        return fonts["symbol"], True
    return (fonts["bold"] if attr.bold else fonts["regular"]), False


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: render_terminal.py <capture.ansi> <out.png>")
    source, destination = sys.argv[1], sys.argv[2]
    with open(source, encoding="utf-8") as handle:
        text = handle.read()
    rows = parse_rows(text)
    if not rows:
        raise SystemExit("capture is empty")

    fonts = load_fonts()
    regular = fonts["regular"]
    cell_w = round(regular.getlength("M"))
    baseline_offset = CELL_H - 7

    columns = max(sum(cell_width(char) for char, _ in row) for row in rows)
    panel_w = columns * cell_w
    panel_h = len(rows) * CELL_H

    margin = 28
    radius = 14
    canvas = Image.new(
        "RGB", (panel_w + margin * 2, panel_h + margin * 2), (10, 12, 16)
    )
    frame = ImageDraw.Draw(canvas)
    frame.rounded_rectangle(
        (margin - 2, margin - 2, margin + panel_w + 2, margin + panel_h + 2),
        radius=radius + 2,
        fill=(24, 27, 33),
    )

    panel = Image.new("RGB", (panel_w, panel_h), APP_BG)
    draw = ImageDraw.Draw(panel)

    for row_index, row in enumerate(rows):
        y = row_index * CELL_H
        x = 0
        for char, attr in row:
            width = cell_width(char)
            bg = attr.bg if attr.bg is not None else APP_BG
            if bg != APP_BG:
                draw.rectangle(
                    (x, y, x + width * cell_w - 1, y + CELL_H - 1), fill=bg
                )
            if char != " " and char.strip():
                color = attr.fg
                if attr.dim:
                    color = mix(color, bg, 0.55)
                font, centered = font_for_char(char, attr, fonts)
                if centered:
                    bbox = font.getbbox(char, anchor="ls")
                    glyph_w = bbox[2] - bbox[0]
                    draw.text(
                        (x + (cell_w - glyph_w) / 2 - bbox[0], y + baseline_offset),
                        char,
                        font=font,
                        fill=color,
                        anchor="ls",
                    )
                else:
                    draw.text(
                        (x, y + baseline_offset),
                        char,
                        font=font,
                        fill=color,
                        anchor="ls",
                    )
                if attr.underline:
                    draw.line(
                        (x, y + CELL_H - 3, x + cell_w - 1, y + CELL_H - 3),
                        fill=color,
                    )
            x += width * cell_w

    mask = Image.new("L", (panel_w, panel_h), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, panel_w - 1, panel_h - 1), radius=radius, fill=255
    )
    canvas.paste(panel, (margin, margin), mask)
    canvas.save(destination, optimize=True)
    print(f"wrote {destination} ({canvas.width}x{canvas.height})")


if __name__ == "__main__":
    main()
