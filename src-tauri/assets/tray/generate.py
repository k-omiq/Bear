#!/usr/bin/env python3
"""Render the macOS menu-bar template icon.

The glyph is the approved "peel reveal" app icon reduced to one ink: the
speech balloon is an outline, the film that still carries text is solid, and
the back of the curl lifting off it is an outline, so the part that is peeled
away reads as clean. The balloon outline is read straight from the approved
vector (design/icon-explore/svg/icon.svg, the `balloonClip` path); the fold
and the curl are simplified from that file's film and curl paths, with the
curl widened so it survives at menu-bar size. All coordinates below are in
that file's 1024 x 1024 space.

Output is pure black on transparent, which is what an NSImage template
wants: macOS tints it for a light or a dark menu bar. The tray code sets any
image to 18 pt tall, so the 36 px file is the one the app embeds (sharp on
Retina, downsampled by AppKit on a 1x display); the 18 px file is its 1x twin.

Needs Pillow. Run from anywhere:

    python3 src-tauri/assets/tray/generate.py
"""

import re
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
SOURCE_SVG = ROOT / "design" / "icon-explore" / "svg" / "icon.svg"

# Canvas is 18 pt square; sizes below are in points.
CANVAS_PT = 18.0
MARGIN_PT = 1.5
STROKE_PT = 1.25
SUPERSAMPLE = 16

# Fold: the film's lifted edge, from the top of the balloon down to where the
# curl rolls under. Then the film's lower edge out to the balloon's left side.
FOLD_TOP = (612, 214)
FOLD_CTRL = ((545, 300), (430, 470))
FOLD_BOTTOM = (352, 612)
LOWER_CTRL = ((300, 628), (250, 634))
LOWER_END = (214, 628)
# Outer edge of the curl, bulging over the clean balloon.
CURL_CTRL = ((720, 300), (640, 560))

OUTPUTS = {"tray-template.png": 18, "tray-template@2x.png": 36}


def balloon_outline(svg_path):
    """The balloon clip path from the approved SVG, flattened to points."""
    text = svg_path.read_text(encoding="utf-8")
    match = re.search(r'id="balloonClip"><path d="([^"]+)"', text)
    if not match:
        sys.exit(f"no balloonClip path in {svg_path}")
    tokens = re.findall(r"[MCZ]|-?\d+(?:\.\d+)?", match.group(1))
    points, index, current = [], 0, None
    while index < len(tokens):
        token = tokens[index]
        if token == "M":
            current = (float(tokens[index + 1]), float(tokens[index + 2]))
            points.append(current)
            index += 3
        elif token == "C":
            index += 1
            # One C may carry several segments.
            while index < len(tokens) and tokens[index] not in "MCZ":
                c = [float(v) for v in tokens[index : index + 6]]
                points += cubic(current, (c[0], c[1]), (c[2], c[3]), (c[4], c[5]), 12)[1:]
                current = (c[4], c[5])
                index += 6
        elif token == "Z":
            break
        else:
            sys.exit(f"unsupported path token {token!r} in {svg_path}")
    return points


def cubic(p0, p1, p2, p3, steps=64):
    out = []
    for k in range(steps + 1):
        u = k / steps
        a, b, c, d = (1 - u) ** 3, 3 * (1 - u) ** 2 * u, 3 * (1 - u) * u * u, u**3
        out.append(
            (
                a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0],
                a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1],
            )
        )
    return out


def render(size, balloon):
    """One template image, `size` pixels square, as black plus alpha."""
    ss = size * SUPERSAMPLE
    px_per_pt = size / CANVAS_PT
    x0 = min(x for x, _ in balloon)
    x1 = max(x for x, _ in balloon)
    y0 = min(y for _, y in balloon)
    y1 = max(y for _, y in balloon)
    box = size - 2 * MARGIN_PT * px_per_pt
    scale = box / max(x1 - x0, y1 - y0)
    left = (size - (x1 - x0) * scale) / 2
    # Snap the top of the balloon to a whole pixel so its crown stays crisp.
    top = round((size - (y1 - y0) * scale) / 2)

    def place(points):
        return [
            ((x - x0) * scale * SUPERSAMPLE + left * SUPERSAMPLE, (y - y0) * scale * SUPERSAMPLE + top * SUPERSAMPLE)
            for x, y in points
        ]

    def blank():
        return Image.new("L", (ss, ss), 0)

    stroke = round(STROKE_PT * px_per_pt * SUPERSAMPLE)

    fill = blank()
    ImageDraw.Draw(fill).polygon(place(balloon), fill=255)
    # Inner stroke: a line twice as wide, clipped to the fill, keeps the
    # outer silhouette identical to the solid balloon.
    ring = blank()
    ImageDraw.Draw(ring).line(place(balloon + balloon[:2]), fill=255, width=2 * stroke, joint="curve")
    glyph = ImageChops.multiply(ring, fill)

    fold = cubic(FOLD_TOP, *FOLD_CTRL, FOLD_BOTTOM)
    lower = cubic(FOLD_BOTTOM, *LOWER_CTRL, LOWER_END)
    # Close the film far outside the balloon, up and to the left, then clip.
    reach = [
        (LOWER_END[0] - 200, LOWER_END[1] + 40),
        (x0 - 200, y0 - 200),
        (FOLD_TOP[0] + 40, y0 - 200),
    ]
    film = blank()
    ImageDraw.Draw(film).polygon(place(fold + lower + reach), fill=255)
    glyph = ImageChops.lighter(glyph, ImageChops.multiply(film, fill))

    curl = blank()
    ImageDraw.Draw(curl).line(place(cubic(FOLD_TOP, *CURL_CTRL, FOLD_BOTTOM)), fill=255, width=stroke, joint="curve")
    glyph = ImageChops.lighter(glyph, curl)

    alpha = glyph.resize((size, size), Image.Resampling.BOX)
    black = Image.new("L", (size, size), 0)
    return Image.merge("RGBA", (black, black, black, alpha))


def main():
    source = Path(sys.argv[1]) if len(sys.argv) > 1 else SOURCE_SVG
    if not source.exists():
        sys.exit(f"approved icon vector not found: {source}")
    balloon = balloon_outline(source)
    for name, size in OUTPUTS.items():
        render(size, balloon).save(HERE / name, optimize=True)
        print(f"wrote {HERE / name} ({size} px)")


if __name__ == "__main__":
    main()
