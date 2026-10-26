"""Dot steps for plan step S3, run after the phase-locked copy (D1, `c04_s1`).

  D2 coverage   Correct the copy's tone toward the tone field: the difference between the target tone
                and the copy's blurred tone, smoothed so it moves whole dots and not single pixels, is
                added inside the hole. Removes banding where the copy picked a source a few levels off.
  D3 re-screen  Render new dots from the screen's lattice: two lattice vectors fitted (sub-pixel) to
                the autocorrelation peaks, every ring pixel binned by its phase in the lattice cell and
                by its tone, and each hole pixel takes the mean ring value at its phase and target tone.
                Exact tone and no copied texture, but the dots are averages.
"""
import cv2
import numpy as np

import screen


def coverage(gray, used, out, tone_f, src, offs, sigma=2.0, smooth=3.0, limit=6.0):
    """D2. The correction is capped at `limit` levels: the copy already took sources within 8 levels of
    the tone field, so a larger gap means the tone field is wrong there (s3-1: a light, low-contrast
    screen with few sources came out 72 levels off), not the dots."""
    tone_out = cv2.GaussianBlur(out, (0, 0), sigma)
    corr = cv2.GaussianBlur(np.where(used, tone_f - tone_out, 0.0), (0, 0), smooth)
    wts = cv2.GaussianBlur(used.astype(np.float64), (0, 0), smooth)
    corr = corr / np.maximum(wts, 1e-6)
    if np.median(np.abs(corr[used])) > limit / 2:
        return out  # the tone field disagrees with the copy across the hole: trust the copy
    corr = np.clip(corr, -limit, limit)
    return np.where(used, out + corr, out)


def lattice(offs):
    """Two lattice vectors (rows of B, in px) fitted to integer repeat offsets, or None."""
    offs = np.asarray(offs, np.float64)
    offs = offs[(offs[:, 0] > 0) | ((offs[:, 0] == 0) & (offs[:, 1] > 0))]  # one of each +-pair
    if len(offs) < 2:
        return None
    order = np.argsort((offs ** 2).sum(1))
    a = offs[order[0]]
    b = None
    for i in order[1:]:
        c = offs[i]
        if abs(a[0] * c[1] - a[1] * c[0]) > 0.5 * np.hypot(*a) * np.hypot(*c):  # > 30 degrees apart
            b = c
            break
    if b is None:
        return None
    basis = np.stack([a, b])
    inv = np.linalg.inv(basis)
    mn = np.round(offs @ inv)
    ok = np.abs(offs - mn @ basis).max(1) <= 1.0
    if ok.sum() >= 3 and np.linalg.matrix_rank(mn[ok]) == 2:
        basis, *_ = np.linalg.lstsq(mn[ok], offs[ok], rcond=None)
    return basis


def rescreen(gray, used, out, tone_f, src, offs, nphase=8, tone_step=4.0, near_px=60):
    """D3."""
    basis = lattice(offs)
    if basis is None:
        return out
    raw = gray.astype(np.float64)
    h, w = gray.shape
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float64)
    uv = np.stack([yy.ravel(), xx.ravel()], 1) @ np.linalg.inv(basis)
    ph = np.floor((uv - np.floor(uv)) * nphase).astype(np.int64).clip(0, nphase - 1)
    cell = (ph[:, 0] * nphase + ph[:, 1]).reshape(h, w)
    ring = src & cv2.dilate(used.astype(np.uint8), np.ones((2 * near_px + 1,) * 2, np.uint8)).astype(bool)
    tone_src, _ = screen.masked_blur(raw, src, 2.0)
    lo = min(tone_src[ring].min(), tone_f[used].min()) if ring.any() else 0.0
    nt = int((max(tone_src[ring].max(), tone_f[used].max()) - lo) / tone_step) + 2
    tb = ((tone_src - lo) / tone_step).clip(0, nt - 1).astype(np.int64)
    ncell = nphase * nphase
    s = np.zeros(ncell * nt)
    n = np.zeros(ncell * nt)
    np.add.at(s, cell[ring] * nt + tb[ring], raw[ring])
    np.add.at(n, cell[ring] * nt + tb[ring], 1)
    s, n = s.reshape(ncell, nt), n.reshape(ncell, nt)
    # fill empty tone bins of each cell by linear interpolation along tone
    table = np.zeros((ncell, nt))
    t = np.arange(nt)
    for c in range(ncell):
        have = n[c] > 0
        if not have.any():
            table[c] = np.nan
            continue
        table[c] = np.interp(t, t[have], s[c, have] / n[c, have])
    if np.isnan(table).any():
        return out
    ys, xs = np.nonzero(used)
    f = ((tone_f[ys, xs] - lo) / tone_step).clip(0, nt - 1)
    i0 = np.floor(f).astype(np.int64).clip(0, nt - 2)
    fr = f - i0
    c = cell[ys, xs]
    res = out.copy()
    res[ys, xs] = table[c, i0] * (1 - fr) + table[c, i0 + 1] * fr
    return res
