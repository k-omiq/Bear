"""Fit the chosen render to the macOS app icon grid.

Crops the generated tile, backs it with solid ink so it is fully opaque, re-masks it
with an exact squircle (superellipse n=5) at 824/1024 of the canvas, and adds the
standard soft drop shadow. Writes a 1024 master plus common PNG sizes.
"""
import sys
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter

here = Path(__file__).parent
src = Image.open(here / "out" / "F-peel-final-f2.png").convert("RGBA")
x0, y0, x1, y1 = src.getchannel("A").point(lambda v: 255 if v > 128 else 0).getbbox()
inset = 24  # trim the generated rim so only our mask defines the edge
art = src.crop((x0 + inset, y0 + inset, x1 - inset, y1 - inset))

S = 4096                      # work at 4x of 1024
tile = round(S * 824 / 1024)  # Apple Big Sur tile size
off = (S - tile) // 2
art = art.resize((tile, tile), Image.LANCZOS)
base = Image.new("RGBA", (tile, tile), (20, 21, 20, 255))
base.alpha_composite(art)

# Superellipse mask |x|^n + |y|^n = 1, n = 5 (close to Apple's continuous corner).
n, r = 5.0, tile / 2
pts = []
import math
for i in range(2048):
    t = 2 * math.pi * i / 2048
    c, s = math.cos(t), math.sin(t)
    pts.append((r + r * math.copysign(abs(c) ** (2 / n), c), r + r * math.copysign(abs(s) ** (2 / n), s)))
mask = Image.new("L", (tile, tile), 0)
ImageDraw.Draw(mask).polygon(pts, fill=255)
base.putalpha(mask)

canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))
shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
sh = Image.new("RGBA", (tile, tile), (0, 0, 0, int(255 * 0.30)))
sh.putalpha(mask.point(lambda v: v * 0.30))
shadow.paste(sh, (off, off + 40), sh)
shadow = shadow.filter(ImageFilter.GaussianBlur(40))
canvas.alpha_composite(shadow)
canvas.alpha_composite(base, (off, off))

master = canvas.resize((1024, 1024), Image.LANCZOS)
out = here / "final"
master.save(out / "icon-1024.png")
for size in (512, 256, 128, 64, 32, 16):
    master.resize((size, size), Image.LANCZOS).save(out / f"icon-{size}.png")
print("tile alpha at center:", master.getpixel((512, 512))[3], "corner alpha:", master.getpixel((2, 2))[3])
