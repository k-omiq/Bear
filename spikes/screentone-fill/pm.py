"""Prototype: multiscale PatchMatch completion (Wexler 2007 EM + Barnes 2009 PatchMatch).

usage: pm.py page.png mask.png out.png [--patch 7] [--vote mean|best] [--src-radius 0] [--levels auto]
mask: white = hole. Grayscale or RGB page.
"""
import argparse
import time

import cv2
import numpy as np
from numba import njit


@njit(cache=True)
def patch_dist(img, ty, tx, sy, sx, r, best):
    h, w, c = img.shape
    d = 0.0
    for dy in range(-r, r + 1):
        y1 = ty + dy
        y2 = sy + dy
        if y1 < 0 or y1 >= h:
            continue
        for dx in range(-r, r + 1):
            x1 = tx + dx
            x2 = sx + dx
            if x1 < 0 or x1 >= w:
                continue
            for k in range(c):
                e = img[y1, x1, k] - img[y2, x2, k]
                d += e * e
        if d > best:
            return d
    return d


@njit(cache=True)
def patchmatch(img, valid, tys, txs, nnf, cost, r, iters, seed):
    np.random.seed(seed)
    h, w = valid.shape
    n = tys.shape[0]
    # index of target by pixel for propagation lookups
    tidx = -np.ones((h, w), np.int64)
    for i in range(n):
        tidx[tys[i], txs[i]] = i
    for i in range(n):
        cost[i] = patch_dist(img, tys[i], txs[i], nnf[i, 0], nnf[i, 1], r, 1e30)
    for it in range(iters):
        if it % 2 == 0:
            start, stop, step, dd = 0, n, 1, -1
        else:
            start, stop, step, dd = n - 1, -1, -1, 1
        for ii in range(start, stop, step):
            ty = tys[ii]
            tx = txs[ii]
            by = nnf[ii, 0]
            bx = nnf[ii, 1]
            bc = cost[ii]
            # propagation from the already-visited neighbours
            for k in range(2):
                ny = ty + (dd if k == 0 else 0)
                nx = tx + (dd if k == 1 else 0)
                if ny < 0 or ny >= h or nx < 0 or nx >= w:
                    continue
                j = tidx[ny, nx]
                if j >= 0:
                    cy = nnf[j, 0] - (ny - ty)
                    cx = nnf[j, 1] - (nx - tx)
                else:
                    continue
                if cy < 0 or cy >= h or cx < 0 or cx >= w or not valid[cy, cx]:
                    continue
                d = patch_dist(img, ty, tx, cy, cx, r, bc)
                if d < bc:
                    bc = d
                    by = cy
                    bx = cx
            # random search
            rad = max(h, w)
            while rad >= 1:
                cy = by + np.random.randint(-rad, rad + 1)
                cx = bx + np.random.randint(-rad, rad + 1)
                if 0 <= cy < h and 0 <= cx < w and valid[cy, cx]:
                    d = patch_dist(img, ty, tx, cy, cx, r, bc)
                    if d < bc:
                        bc = d
                        by = cy
                        bx = cx
                rad //= 2
            nnf[ii, 0] = by
            nnf[ii, 1] = bx
            cost[ii] = bc


@njit(cache=True)
def vote(img, hole, tys, txs, nnf, cost, r, best_mode):
    h, w, c = img.shape
    acc = np.zeros((h, w, c))
    wsum = np.zeros((h, w))
    bestc = np.full((h, w), 1e30)
    n = tys.shape[0]
    # robust sigma: 75th percentile-ish of costs
    sig = np.median(cost) + 1e-6
    for i in range(n):
        ty = tys[i]
        tx = txs[i]
        sy = nnf[i, 0]
        sx = nnf[i, 1]
        wt = np.exp(-cost[i] / (2.0 * sig))
        for dy in range(-r, r + 1):
            y = ty + dy
            if y < 0 or y >= h:
                continue
            for dx in range(-r, r + 1):
                x = tx + dx
                if x < 0 or x >= w or not hole[y, x]:
                    continue
                if best_mode:
                    if cost[i] < bestc[y, x]:
                        bestc[y, x] = cost[i]
                        for k in range(c):
                            acc[y, x, k] = img[sy + dy, sx + dx, k]
                        wsum[y, x] = 1.0
                else:
                    for k in range(c):
                        acc[y, x, k] += wt * img[sy + dy, sx + dx, k]
                    wsum[y, x] += wt
    out = img.copy()
    for y in range(h):
        for x in range(w):
            if hole[y, x] and wsum[y, x] > 0:
                for k in range(c):
                    out[y, x, k] = acc[y, x, k] / wsum[y, x]
    return out


def valid_sources(hole, r, src_region):
    h, w = hole.shape
    k = np.ones((2 * r + 1, 2 * r + 1), np.uint8)
    touches_hole = cv2.dilate(hole.astype(np.uint8), k) > 0
    v = ~touches_hole
    v[:r, :] = False
    v[-r:, :] = False
    v[:, :r] = False
    v[:, -r:] = False
    if src_region is not None:
        v &= src_region
    return v


def random_init(valid, n, rng):
    ys, xs = np.nonzero(valid)
    pick = rng.integers(0, len(ys), n)
    return np.stack([ys[pick], xs[pick]], 1).astype(np.int64)


def complete(img, hole, patch=7, vote_mode="mean", src_radius=0, min_size=24, em_coarse=8, em_fine=3, seed=1):
    r = patch // 2
    rng = np.random.default_rng(seed)
    # pyramid
    imgs, holes = [img], [hole]
    while True:
        ys, xs = np.nonzero(holes[-1])
        if len(ys) == 0:
            break
        ext = max(ys.max() - ys.min(), xs.max() - xs.min())
        if ext < min_size or min(imgs[-1].shape[:2]) < 4 * patch:
            break
        nh, nw = imgs[-1].shape[0] // 2, imgs[-1].shape[1] // 2
        hm = holes[-1].astype(np.float32)
        nhole = cv2.resize(hm, (nw, nh), interpolation=cv2.INTER_AREA) > 0
        if valid_sources(nhole, r, None).sum() < 64:  # too coarse to leave any whole source patch
            break
        imgs.append(cv2.resize(imgs[-1], (nw, nh), interpolation=cv2.INTER_AREA).reshape(nh, nw, -1))
        holes.append(nhole)
    L = len(imgs)
    print("levels", L, [im.shape[:2] for im in imgs])
    cur = None
    nnf_prev = None
    tprev = None
    for lv in range(L - 1, -1, -1):
        im = imgs[lv].astype(np.float64).copy()
        ho = holes[lv]
        h, w = ho.shape
        src_region = None
        if src_radius > 0:
            rad = max(1, src_radius >> lv)
            src_region = cv2.dilate(ho.astype(np.uint8), np.ones((2 * rad + 1, 2 * rad + 1), np.uint8)) > 0
        valid = valid_sources(ho, r, src_region)
        tmask = cv2.dilate(ho.astype(np.uint8), np.ones((2 * r + 1, 2 * r + 1), np.uint8)) > 0
        tys, txs = np.nonzero(tmask)
        tys = tys.astype(np.int64)
        txs = txs.astype(np.int64)
        n = len(tys)
        if cur is None:
            # coarsest: diffuse fill from the border as the first guess
            u8 = np.clip(im, 0, 255).astype(np.uint8)
            if u8.shape[2] == 1:
                u8 = u8[:, :, 0]
            fill = cv2.inpaint(u8, ho.astype(np.uint8), 5, cv2.INPAINT_TELEA).reshape(h, w, -1)
            im[ho] = fill[ho]
            nnf = random_init(valid, n, rng)
        else:
            # upsample previous NNF
            prev_idx = -np.ones(imgs[lv + 1].shape[:2], np.int64)
            prev_idx[tprev[0], tprev[1]] = np.arange(len(tprev[0]))
            nnf = random_init(valid, n, rng)
            for i in range(n):
                py, px = tys[i] // 2, txs[i] // 2
                if py < prev_idx.shape[0] and px < prev_idx.shape[1]:
                    j = prev_idx[py, px]
                    if j >= 0:
                        cy = nnf_prev[j, 0] * 2 + (tys[i] - 2 * py)
                        cx = nnf_prev[j, 1] * 2 + (txs[i] - 2 * px)
                        if 0 <= cy < h and 0 <= cx < w and valid[cy, cx]:
                            nnf[i] = (cy, cx)
            up = cv2.resize(cur, (w, h), interpolation=cv2.INTER_LINEAR).reshape(h, w, -1)
            im[ho] = up[ho]
        cost = np.zeros(n)
        ems = em_coarse if lv == L - 1 else em_fine
        for e in range(ems):
            patchmatch(im, valid, tys, txs, nnf, cost, r, 5, seed + e + 17 * lv)
            last = lv == 0 and e == ems - 1
            im = vote(im, ho, tys, txs, nnf, cost, r, last and vote_mode == "best")
        cur = im
        nnf_prev = nnf
        tprev = (tys, txs)
    return cur


def fill(gray, hole, avoid=None):
    """Harness entry: plain multiscale PatchMatch on the raw page. `avoid` is ignored (baseline)."""
    out = complete(gray.astype(np.float64)[:, :, None], hole, 7, "mean", 0)
    return np.clip(out[:, :, 0] + 0.5, 0, 255).astype(np.uint8)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("page")
    ap.add_argument("mask")
    ap.add_argument("out")
    ap.add_argument("--patch", type=int, default=7)
    ap.add_argument("--vote", default="mean")
    ap.add_argument("--src-radius", type=int, default=0)
    ap.add_argument("--crop", default="0,960,1008,1400")
    a = ap.parse_args()
    x0, y0, x1, y1 = map(int, a.crop.split(","))
    page = cv2.imread(a.page, cv2.IMREAD_GRAYSCALE)[y0:y1, x0:x1]
    hole = cv2.imread(a.mask, cv2.IMREAD_GRAYSCALE)[y0:y1, x0:x1] > 127
    img = page.astype(np.float64)[:, :, None]
    t = time.time()
    out = complete(img, hole, a.patch, a.vote, a.src_radius)
    print(f"{time.time() - t:.1f}s")
    cv2.imwrite(a.out, np.clip(out[:, :, 0] + 0.5, 0, 255).astype(np.uint8))


if __name__ == "__main__":
    main()
