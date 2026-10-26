"""Remnant guard (plan step S4): grow the hole over lettering the mask left behind.

A tight mask (3 px padding) leaves the outer part of a white letter outline, a glow, or bits of ink
outside it. Any fill then works around them, and they stay on the page. The guard adds to the hole
every piece that

  - is brighter or darker than the screen around the mask ever is (fine tone past the ring's
    1st..99th percentile by a margin),
  - touches the hole (directly, or through pieces already added), and
  - lies mostly within `reach` px of the mask. A piece that runs far beyond it (a panel line, a bar,
    a light, a character's outline) is art, and is left alone whole.

Only the part within `reach` is added, then closed and widened by `rim` pixels for the soft edge of
a glow or an anti-aliased outline.
Other detected regions (`avoid`) are never added.
"""
import cv2
import numpy as np


def _screen_ring(g, hole, avoid):
    """Pixels 3..20 px around the hole that look like the screen (local spread near the ring's)."""
    h8 = hole.astype(np.uint8)
    ring = cv2.dilate(h8, np.ones((41, 41), np.uint8)).astype(bool) & ~cv2.dilate(h8, np.ones((7, 7), np.uint8)).astype(bool)
    ring &= ~avoid
    mean = cv2.GaussianBlur(g, (0, 0), 1.5)
    std = np.sqrt(np.maximum(cv2.GaussianBlur(g * g, (0, 0), 1.5) - mean * mean, 0))
    level = np.median(std[ring]) if ring.any() else 0
    if level >= 1.5:
        ring &= std >= 0.4 * level
    return ring


def grow(gray, hole, avoid=None, reach=12, rim=2):
    g = gray.astype(np.float64)
    avoid = np.zeros_like(hole) if avoid is None else avoid
    ring = _screen_ring(g, hole, avoid)
    if ring.sum() < 50:
        return hole.copy()
    fine = cv2.GaussianBlur(g, (0, 0), 1.0)
    lo, hi = np.percentile(fine[ring], [1, 99])
    margin = max(10.0, 0.25 * (hi - lo))
    odd = ((fine > hi + margin) | (fine < lo - margin)) & ~hole & ~avoid
    band = cv2.dilate(hole.astype(np.uint8), np.ones((2 * reach + 1, 2 * reach + 1), np.uint8)).astype(bool)
    n, lab, stats, _ = cv2.connectedComponentsWithStats(odd.astype(np.uint8), connectivity=8)
    inside_band = np.bincount(lab[band], minlength=n)
    grown = hole.copy()
    taken = np.zeros(n, bool)
    taken[0] = True
    for _ in range(4):  # pieces touching pieces already added
        touch = cv2.dilate(grown.astype(np.uint8), np.ones((3, 3), np.uint8)).astype(bool)
        ids = np.unique(lab[touch & odd])
        new = [i for i in ids if not taken[i] and inside_band[i] >= 0.6 * stats[i, cv2.CC_STAT_AREA]]
        if not new:
            break
        taken[new] = True
        grown |= np.isin(lab, new) & band
    added = grown & ~hole
    if added.any():
        added = cv2.morphologyEx(added.astype(np.uint8), cv2.MORPH_CLOSE, np.ones((3, 3), np.uint8))
        added = cv2.dilate(added, np.ones((2 * rim + 1, 2 * rim + 1), np.uint8)).astype(bool)
        grown = hole | (added & band & ~avoid)
    return grown
