"""Rebuild design/icon.png as a clean layered SVG (design/icon.svg).

Each part (balloon, rose film, curl, lifted text) is segmented by color from the
raster, its outline is resampled and smoothed, then written as a few dozen cubic
curves. Tile, halftone, gradients, shadows and blur are native SVG, so the result
is small, editable and sharp at any size.

Needs numpy, opencv-python-headless and pillow:
    python3 vectorize.py
"""
import math
from pathlib import Path

import cv2
import numpy as np
from PIL import Image

HERE = Path(__file__).parent
SRC = HERE.parent.parent / "icon.png"

im = np.array(Image.open(SRC).convert("RGBA")).astype(int)
r, g, b, a = (im[..., i] for i in range(4))
tile = a > 200


def fill_largest(mask, close):
    m = cv2.morphologyEx(mask.astype(np.uint8) * 255, cv2.MORPH_CLOSE, np.ones((close, close), np.uint8))
    n, lab, st, _ = cv2.connectedComponentsWithStats(m)
    k = 1 + int(np.argmax(st[1:, cv2.CC_STAT_AREA]))
    cnts, _ = cv2.findContours((lab == k).astype(np.uint8) * 255, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_NONE)
    out = np.zeros(m.shape, np.uint8)
    cv2.drawContours(out, cnts, -1, 255, -1)
    return out


mx = np.maximum(np.maximum(r, g), b)
BALLOON = fill_largest(tile & (mx > 110), 9)
FILM = fill_largest(tile & (r - g > 35) & (g < 150), 15)
LIGHT = fill_largest(tile & (r > 195) & (r - g > 12) & (g > 130) & (g < 228), 7)

# Curl = from the fold line (left edge of the light band) out to the film's outer edge.
CURL = np.zeros_like(LIGHT)
outer = (FILM > 0) | (LIGHT > 0)
for y in range(1024):
    xs = np.flatnonzero(LIGHT[y])
    if xs.size:
        xo = np.flatnonzero(outer[y])
        CURL[y, xs.min():xo.max() + 1] = 255
CURL = fill_largest(CURL > 0, 5)

# The roll: film pixels just below the curl's tip, right of where the fold line ends.
ROLL = np.zeros_like(FILM)
tip_y = int(np.flatnonzero(LIGHT.any(axis=1)).max())
tip_x = int(np.flatnonzero(LIGHT[tip_y - 6]).min())
ROLL[tip_y - 12:tip_y + 48, tip_x - 4:] = FILM[tip_y - 12:tip_y + 48, tip_x - 4:]
ROLL[CURL > 0] = 0
ROLL = fill_largest(ROLL > 0, 5)

# Lifted text: small dark marks on the film, found with a black-hat filter so the
# broad shading along the fold is ignored.
near_curl = cv2.dilate(CURL | ROLL, np.ones((15, 15), np.uint8)) > 0
red = r.astype(np.uint8)
blackhat = cv2.morphologyEx(red, cv2.MORPH_BLACKHAT, cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (45, 45)))
TEXT = (FILM > 0) & (blackhat > 16) & ~near_curl
TEXT = cv2.morphologyEx(TEXT.astype(np.uint8) * 255, cv2.MORPH_OPEN, np.ones((5, 5), np.uint8))


def smooth_path(contour, spacing, sigma):
    pts = contour.reshape(-1, 2).astype(float)
    seg = np.linalg.norm(np.diff(np.vstack([pts, pts[:1]]), axis=0), axis=1)
    s = np.concatenate([[0], np.cumsum(seg)])
    total = s[-1]
    n = max(8, int(total / spacing))
    t = np.linspace(0, total, n, endpoint=False)
    closed = np.vstack([pts, pts[:1]])
    rs = np.column_stack([np.interp(t, s, closed[:, 0]), np.interp(t, s, closed[:, 1])])
    if sigma:
        k = np.arange(-3 * math.ceil(sigma), 3 * math.ceil(sigma) + 1)
        w = np.exp(-(k ** 2) / (2 * sigma ** 2)); w /= w.sum()
        rs = sum(w[i] * np.roll(rs, k[i], axis=0) for i in range(len(k)))
    d = [f"M{rs[0][0]:.1f} {rs[0][1]:.1f}"]
    for i in range(n):
        p0, p1, p2, p3 = (rs[(i + j) % n] for j in (-1, 0, 1, 2))
        c1 = p1 + (p2 - p0) / 6
        c2 = p2 - (p3 - p1) / 6
        d.append(f"C{c1[0]:.1f} {c1[1]:.1f} {c2[0]:.1f} {c2[1]:.1f} {p2[0]:.1f} {p2[1]:.1f}")
    return "".join(d) + "Z"


def outline(mask, spacing, sigma, min_area=0):
    cnts, _ = cv2.findContours(mask, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_NONE)
    return " ".join(smooth_path(c, spacing, sigma) for c in cnts if cv2.contourArea(c) >= min_area)


def squircle(x, y, size, n=5.0, steps=96):
    r0 = size / 2
    cx, cy = x + r0, y + r0
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        pts.append(np.array([cx + r0 * math.copysign(abs(c) ** (2 / n), c),
                             cy + r0 * math.copysign(abs(s) ** (2 / n), s)]))
    d = [f"M{pts[0][0]:.1f} {pts[0][1]:.1f}"]
    for i in range(steps):
        p0, p1, p2, p3 = (pts[(i + k) % steps] for k in (-1, 0, 1, 2))
        c1, c2 = p1 + (p2 - p0) / 6, p2 - (p3 - p1) / 6
        d.append(f"C{c1[0]:.1f} {c1[1]:.1f} {c2[0]:.1f} {c2[1]:.1f} {p2[0]:.1f} {p2[1]:.1f}")
    return "".join(d) + "Z"


TILE_D = squircle(100, 100, 824)
BALLOON_D = outline(BALLOON, 14, 1.2)
FILM_D = outline(FILM, 12, 1.0)
CURL_D = outline(CURL, 8, 1.0)
TEXT_D = outline(TEXT, 6, 1.0, min_area=60)
ROLL_D = outline(ROLL, 6, 1.0)
rys, rxs = np.nonzero(ROLL)
# Curl shading runs across the strip: fold side -> outer edge, perpendicular to its axis.
ys, xs = np.nonzero(CURL)
top = np.array([xs[ys == ys.min()].mean(), ys.min()])
bottom = np.array([xs[ys == ys.max()].mean(), ys.max()])
axis = (bottom - top) / np.linalg.norm(bottom - top)
perp = np.array([-axis[1], axis[0]])
if perp[0] < 0:
    perp = -perp
mid = np.array([xs.mean(), ys.mean()])
gx0, gy0 = mid - perp * 42
gx1, gy1 = mid + perp * 42

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">
  <title>Manga Cleaner</title>
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#1A1B1A"/>
      <stop offset="1" stop-color="#121312"/>
    </linearGradient>
    <pattern id="tone" width="13" height="13" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
      <circle cx="6.5" cy="6.5" r="2.4" fill="#343534"/>
    </pattern>
    <radialGradient id="toneFade" cx="0.5" cy="0.45" r="0.62">
      <stop offset="0.55" stop-color="#fff" stop-opacity="0.35"/>
      <stop offset="1" stop-color="#fff" stop-opacity="1"/>
    </radialGradient>
    <mask id="toneMask"><rect x="100" y="100" width="824" height="824" fill="url(#toneFade)"/></mask>
    <linearGradient id="paper" x1="0" y1="0" x2="0.3" y2="1">
      <stop offset="0" stop-color="#FAF9F6"/>
      <stop offset="1" stop-color="#EDEBE6"/>
    </linearGradient>
    <radialGradient id="film" gradientUnits="userSpaceOnUse" cx="330" cy="330" r="330">
      <stop offset="0" stop-color="#BA6A80"/>
      <stop offset="0.7" stop-color="#A85C70"/>
      <stop offset="1" stop-color="#93455B"/>
    </radialGradient>
    <linearGradient id="curl" gradientUnits="userSpaceOnUse" x1="{gx0:.0f}" y1="{gy0:.0f}" x2="{gx1:.0f}" y2="{gy1:.0f}">
      <stop offset="0" stop-color="#EDB6C8"/>
      <stop offset="0.25" stop-color="#FFF1F5"/>
      <stop offset="0.6" stop-color="#F5CCD9"/>
      <stop offset="1" stop-color="#E29AB2"/>
    </linearGradient>
    <linearGradient id="roll" gradientUnits="userSpaceOnUse" x1="{rxs.min()}" y1="{rys.min()}" x2="{rxs.max()}" y2="{rys.max()}">
      <stop offset="0" stop-color="#7E2A47"/>
      <stop offset="0.7" stop-color="#B8577A"/>
      <stop offset="1" stop-color="#E8A9BE"/>
    </linearGradient>
    <clipPath id="tileClip"><path d="{TILE_D}"/></clipPath>
    <clipPath id="filmClip"><path d="{FILM_D}"/></clipPath>
    <clipPath id="balloonClip"><path d="{BALLOON_D}"/></clipPath>
    <filter id="tileShadow" x="-10%" y="-10%" width="120%" height="125%">
      <feDropShadow dx="0" dy="10" stdDeviation="10" flood-color="#000" flood-opacity="0.3"/>
    </filter>
    <filter id="balloonShadow" x="-15%" y="-15%" width="130%" height="135%">
      <feDropShadow dx="0" dy="8" stdDeviation="10" flood-color="#000" flood-opacity="0.5"/>
    </filter>
    <filter id="curlShadow" x="-30%" y="-30%" width="160%" height="160%">
      <feDropShadow dx="7" dy="9" stdDeviation="8" flood-color="#2B0A16" flood-opacity="0.35"/>
    </filter>
    <filter id="textBlur" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="5"/></filter>
  </defs>

  <path d="{TILE_D}" fill="url(#tile)" filter="url(#tileShadow)"/>
  <g clip-path="url(#tileClip)">
    <rect x="100" y="100" width="824" height="824" fill="url(#tone)" mask="url(#toneMask)"/>
  </g>
  <path d="{TILE_D}" fill="none" stroke="#fff" stroke-opacity="0.07" stroke-width="3"/>

  <path d="{BALLOON_D}" fill="url(#paper)" filter="url(#balloonShadow)"/>
  <g clip-path="url(#balloonClip)">
    <path d="{FILM_D}" fill="url(#film)"/>
    <g clip-path="url(#filmClip)">
      <path d="{TEXT_D}" fill="#6A2336" fill-opacity="0.7" filter="url(#textBlur)"/>
      <path d="{CURL_D}" fill="none" stroke="#4A1027" stroke-opacity="0.4" stroke-width="26" filter="url(#textBlur)"/>
    </g>
  </g>
  <g filter="url(#curlShadow)">
    <path d="{CURL_D}" fill="url(#curl)"/>
    <path d="{ROLL_D}" fill="url(#roll)"/>
  </g>
</svg>
'''
out = HERE.parent.parent / "icon.svg"
out.write_text(svg)
print(out, f"{len(svg) / 1024:.1f} KB")
