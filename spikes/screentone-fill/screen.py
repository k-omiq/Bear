"""Screen model shared by the screentone candidates (plan step S1).

- `repeat_offsets`: the screen's short repeat offsets (a dot step and its neighbours), from the
  masked autocorrelation of the page's fine detail.
- `confidence`: how much each pixel looks like that screen, from the local correlation of the fine
  detail with itself one repeat away. Lettering, lights, glows, lines and flat paper score low.
- `sources`: the pixels a fill may copy from: known, not another detected region, and screen.
- `onion_copy`: fill the hole from its edge inward, each pixel copying through a short repeat offset
  from a screen pixel or from a pixel already filled, so the dots stay in phase however wide the
  hole is. Among the offsets it can use, it picks the source whose tone is closest to the target tone.
"""
import cv2
import numpy as np
from numba import njit


def membrane(tone, known, iters=200):
    """Laplace fill of the unknown pixels from the known ones (T1), coarse to fine.

    Each level starts from the level below, scaled up, then relaxes with 4-neighbour averaging while
    the known pixels stay fixed.
    """
    k4 = np.array([[0, 0.25, 0], [0.25, 0, 0.25], [0, 0.25, 0]], np.float64)
    if min(tone.shape) > 32 and (~known).any():
        h, w = tone.shape
        small_known = cv2.resize(known.astype(np.float32), (w // 2, h // 2), interpolation=cv2.INTER_AREA) > 0.99
        small = cv2.resize(np.where(known, tone, 0), (w // 2, h // 2), interpolation=cv2.INTER_AREA)
        wsum = cv2.resize(known.astype(np.float64), (w // 2, h // 2), interpolation=cv2.INTER_AREA)
        small = np.where(small_known, small / np.maximum(wsum, 1e-6), 0)
        coarse = membrane(small, small_known, iters)
        start = cv2.resize(coarse, (w, h), interpolation=cv2.INTER_LINEAR)
    else:
        start = np.full(tone.shape, tone[known].mean() if known.any() else 0.0)
    u = np.where(known, tone, start)
    for _ in range(iters):
        u = np.where(known, tone, cv2.filter2D(u, -1, k4, borderType=cv2.BORDER_REPLICATE))
    return u


def masked_blur(img, known, sigma):
    w = cv2.GaussianBlur(known.astype(np.float64), (0, 0), sigma)
    t = cv2.GaussianBlur(np.where(known, img, 0.0), (0, 0), sigma)
    return t / np.maximum(w, 1e-6), w


def detail(raw, known, sigma=1.5):
    tone, _ = masked_blur(raw, known, sigma)
    return np.where(known, raw - tone, 0.0)


def autocorr(hp, known, radius):
    """Masked, normalised autocorrelation of `hp` over `known`, for offsets within `radius`."""
    h, w = hp.shape
    a = np.where(known, hp, 0.0)
    m = known.astype(np.float64)
    H, W = cv2.getOptimalDFTSize(h + radius + 1), cv2.getOptimalDFTSize(w + radius + 1)
    fa = np.fft.rfft2(a, (H, W))
    fm = np.fft.rfft2(m, (H, W))
    fe = np.fft.rfft2(a * a, (H, W))
    num = np.fft.irfft2(fa * np.conj(fa), (H, W))
    en = np.fft.irfft2(fe * np.conj(fm), (H, W))
    out = np.zeros((2 * radius + 1, 2 * radius + 1))
    for dy in range(-radius, radius + 1):
        for dx in range(-radius, radius + 1):
            e = np.sqrt(max(en[dy % H, dx % W], 0) * max(en[-dy % H, -dx % W], 0))
            out[dy + radius, dx + radius] = num[dy % H, dx % W] / (e + 1e-9)
    return out


def repeat_offsets(hp, known, radius=16, rel=0.55, floor=0.2):
    """Local maxima of the autocorrelation at 2..radius px, at least `rel` of the best and `floor`."""
    c = autocorr(hp, known, radius)
    yy, xx = np.mgrid[-radius:radius + 1, -radius:radius + 1]
    r2 = yy * yy + xx * xx
    c[r2 < 4] = -1  # 0 and 1 px: smoothness, not the screen
    mx = cv2.dilate(c.astype(np.float32), np.ones((3, 3), np.uint8))
    peak = (c >= mx - 1e-9) & (r2 <= radius * radius)
    best = c[peak].max() if peak.any() else 0
    sel = peak & (c >= max(floor, rel * best))
    oy, ox = np.nonzero(sel)
    offs = np.stack([oy - radius, ox - radius], 1).astype(np.int64)
    score = c[sel]
    order = np.argsort(-score)
    return offs[order], score[order]


def confidence(hp, known, offs, k=4, sigma=3.0):
    """Mean local correlation of the detail with itself through the k strongest offsets."""
    H, W = hp.shape
    acc = np.zeros((H, W))
    n = 0
    e0 = cv2.GaussianBlur(hp * hp, (0, 0), sigma)
    for dy, dx in offs[:k]:
        sh = np.zeros_like(hp)
        km = np.zeros((H, W), bool)
        ys0, ys1 = max(0, -dy), min(H, H - dy)
        xs0, xs1 = max(0, -dx), min(W, W - dx)
        sh[ys0:ys1, xs0:xs1] = hp[ys0 + dy:ys1 + dy, xs0 + dx:xs1 + dx]
        km[ys0:ys1, xs0:xs1] = known[ys0 + dy:ys1 + dy, xs0 + dx:xs1 + dx]
        both = (known & km).astype(np.float64)
        num = cv2.GaussianBlur(hp * sh * both, (0, 0), sigma)
        e1 = cv2.GaussianBlur(sh * sh * both, (0, 0), sigma)
        acc += num / np.sqrt(e0 * e1 + 1e-6)
        n += 1
    conf = acc / max(n, 1)
    # flat areas (glow, paper) have no detail to correlate: low confidence
    level = np.median(e0[known]) if known.any() else 0
    conf[e0 < 0.15 * level] = 0
    return conf


def sources(gray, hole, avoid, offs, near_px=60):
    raw = gray.astype(np.float64)
    known = ~cv2.dilate(hole.astype(np.uint8), np.ones((5, 5), np.uint8)).astype(bool) & ~avoid
    hp = detail(raw, known)
    conf = confidence(hp, known, offs)
    ring = known & cv2.dilate(hole.astype(np.uint8), np.ones((2 * near_px + 1,) * 2, np.uint8)).astype(bool)
    ref = np.median(conf[ring]) if ring.any() else 0
    return known & (conf >= max(0.25, 0.6 * ref)), conf


@njit(cache=True)
def _onion(raw, tone, hole, src, order_y, order_x, offs, wdist, chain_cost, keep_bonus, max_dt):
    out = raw.copy()
    ok = src.copy()  # pixels a later pixel may copy from
    filled = np.zeros(hole.shape, np.bool_)
    used_k = -np.ones(hole.shape, np.int64)
    h, w = hole.shape
    for i in range(order_y.shape[0]):
        y = order_y[i]
        x = order_x[i]
        best = 1e30
        bv = -1.0
        bk = -1
        for k in range(offs.shape[0]):
            sy = y + offs[k, 0]
            sx = x + offs[k, 1]
            if sy < 0 or sy >= h or sx < 0 or sx >= w or not ok[sy, sx]:
                continue
            dt = abs(tone[sy, sx] - tone[y, x])
            if dt > max_dt:
                continue  # never copy a source far off the target tone
            d = dt + wdist * np.sqrt(offs[k, 0] ** 2 + offs[k, 1] ** 2)
            if filled[sy, sx]:
                d += chain_cost  # a page pixel beats one already filled, so errors do not chain
            # coherence: the offset a filled neighbour used keeps whole patches together
            for ny, nx in ((y - 1, x), (y + 1, x), (y, x - 1), (y, x + 1)):
                if 0 <= ny < h and 0 <= nx < w and used_k[ny, nx] == k:
                    d -= keep_bonus
                    break
            if d < best:
                best = d
                bv = out[sy, sx]
                bk = k
        if bk >= 0:
            out[y, x] = bv
            ok[y, x] = True
            filled[y, x] = True
            used_k[y, x] = bk
    return out, filled


def onion_copy(gray, hole, src, tone, offs, wdist=0.02, chain_cost=4.0, keep_bonus=0.75, max_dt=8.0):
    """Phase-locked copy from the hole's edge inward. Returns (out, done) where done marks filled pixels."""
    raw = gray.astype(np.float64)
    # order: distance to the nearest source pixel, outside in
    dist = cv2.distanceTransform((~src).astype(np.uint8), cv2.DIST_L2, 5)
    ys, xs = np.nonzero(hole)
    order = np.argsort(dist[ys, xs], kind="stable")
    out, done = _onion(raw, tone.astype(np.float64), hole, src, ys[order].astype(np.int64),
                       xs[order].astype(np.int64), offs, wdist, chain_cost, keep_bonus, max_dt)
    return out, done


def far_offsets(hp, known, radius=100, rel=0.6, floor=0.3, near_best=None):
    """Repeat peaks out to `radius`: direct, in-phase copies from far away when the screen holds."""
    c = autocorr(hp, known, radius)
    yy, xx = np.mgrid[-radius:radius + 1, -radius:radius + 1]
    r2 = yy * yy + xx * xx
    c[r2 < 4] = -1
    mx = cv2.dilate(c.astype(np.float32), np.ones((3, 3), np.uint8))
    peak = (c >= mx - 1e-9) & (r2 <= radius * radius)
    best = near_best if near_best is not None else (c[peak].max() if peak.any() else 0)
    sel = peak & (c >= max(floor, rel * best))
    oy, ox = np.nonzero(sel)
    return np.stack([oy - radius, ox - radius], 1).astype(np.int64)
