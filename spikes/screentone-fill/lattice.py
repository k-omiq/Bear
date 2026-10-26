"""Prototype: screentone-aware fill.

1. Split the page into tone (low pass) and dots (high pass).
2. Fill the tone map with PatchMatch (low frequency, where PatchMatch works well).
3. Find the screen's repeat offsets from the autocorrelation of the dots outside the hole.
4. For every hole pixel, copy a real pixel from an offset that is a repeat of the screen
   (so the dots stay in phase), choosing the offset whose tone patch matches the filled tone best.
"""
import sys

import cv2
import numpy as np
from numba import njit

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from pm import complete  # noqa: E402


def repeat_offsets(hp, known, radius, thresh):
    h, w = hp.shape
    a = np.where(known, hp, 0.0)
    m = known.astype(np.float64)
    H, W = cv2.getOptimalDFTSize(2 * h), cv2.getOptimalDFTSize(2 * w)
    fa = np.fft.rfft2(a, (H, W))
    fm = np.fft.rfft2(m, (H, W))
    fa2 = np.fft.rfft2(a * a, (H, W))
    num = np.fft.irfft2(fa * np.conj(fa), (H, W))
    cnt = np.fft.irfft2(fm * np.conj(fm), (H, W))
    en = np.fft.irfft2(fa2 * np.conj(fm), (H, W))  # energy of the overlap, one side
    num = np.fft.fftshift(num)
    cnt = np.fft.fftshift(np.maximum(cnt, 1))
    en = np.fft.fftshift(np.maximum(en, 1e-9))
    cy, cx = H // 2, W // 2
    win = num[cy - radius:cy + radius + 1, cx - radius:cx + radius + 1]
    e = en[cy - radius:cy + radius + 1, cx - radius:cx + radius + 1]
    corr = win / e
    corr[radius, radius] = 0  # zero offset copies the hole onto itself
    mx = cv2.dilate(corr.astype(np.float32), np.ones((3, 3), np.uint8))
    peaks = (corr >= mx - 1e-9) & (corr > thresh)
    oy, ox = np.nonzero(peaks)
    return np.stack([oy - radius, ox - radius], 1).astype(np.int64), corr


@njit(cache=True)
def locked_copy(raw, tone, hole, offs, r):
    h, w = raw.shape
    out = raw.copy()
    for y in range(h):
        for x in range(w):
            if not hole[y, x]:
                continue
            best = 1e30
            by, bx = -1, -1
            for k in range(offs.shape[0]):
                sy = y + offs[k, 0]
                sx = x + offs[k, 1]
                if sy < r or sy >= h - r or sx < r or sx >= w - r or hole[sy, sx]:
                    continue
                d = 0.0
                for dy in range(-r, r + 1, 2):
                    yy = y + dy
                    if yy < 0 or yy >= h:
                        continue
                    for dx in range(-r, r + 1, 2):
                        xx = x + dx
                        if xx < 0 or xx >= w:
                            continue
                        e = tone[yy, xx] - tone[sy + dy, sx + dx]
                        d += e * e
                    if d > best:
                        break
                # mild preference for near sources
                d += 0.02 * (offs[k, 0] * offs[k, 0] + offs[k, 1] * offs[k, 1])
                if d < best:
                    best = d
                    by, bx = sy, sx
            if by >= 0:
                out[y, x] = raw[by, bx]
    return out


def fill(gray, hole, avoid=None):
    """Harness entry: the 2026-10-22 baseline. Tone filled by PatchMatch (T4), then a phase-locked
    copy picking the in-phase offset whose tone patch matches best. `avoid` is ignored (baseline)."""
    raw = gray.astype(np.float64)
    sigma = 2.0
    known = ~cv2.dilate(hole.astype(np.uint8), np.ones((7, 7), np.uint8)).astype(bool)
    wsum = cv2.GaussianBlur(known.astype(np.float64), (0, 0), sigma)
    tone = cv2.GaussianBlur(np.where(known, raw, 0), (0, 0), sigma) / np.maximum(wsum, 1e-6)
    tone[~known] = 0
    tone_f = complete(tone[:, :, None], ~known, patch=9, vote_mode="mean")[:, :, 0]
    offs, _ = repeat_offsets(raw - tone_f, known, radius=60, thresh=0.5)
    if len(offs) == 0:
        return np.clip(tone_f + 0.5, 0, 255).astype(np.uint8)
    out = locked_copy(raw, tone_f, ~known, offs, 6)
    return np.clip(out + 0.5, 0, 255).astype(np.uint8)
