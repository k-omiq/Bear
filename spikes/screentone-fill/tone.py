"""Tone fields for plan step S2: how the brightness is carried across the hole.

Each takes `tone` (the screen's blurred brightness, valid where `known`) and returns a full field.
Only the values inside the hole are used; the copy then picks dots whose tone matches.

  T1 membrane    Laplace's equation: the smoothest surface through the edge values. Follows the
                 edge, but a ramp whose extreme sits inside the hole comes out flattened.
  T2 biharmonic  The biharmonic equation: carries the slope across as well as the value, so a ramp
                 keeps going and a blob's darker centre can be darker than its edge.
  T3 fitted      A quadratic surface fitted to a ring around the hole (robust weights, so lights and
                 leftover art barely pull it), plus a membrane fill of what the fit misses at the
                 edge, so it still meets the edge exactly.
  T4 PatchMatch  The tone map completed by PatchMatch, as the 2026-10-22 baseline did.

T1 and T2 are solved exactly as sparse linear systems over the crop.
"""
import cv2
import numpy as np
import scipy.sparse as sp
import scipy.sparse.linalg as spla

import pm


def _laplacian(h, w):
    """Graph Laplacian of the 4-neighbour grid (Neumann edges), as a sparse matrix."""
    n = h * w
    idx = np.arange(n).reshape(h, w)
    rows, cols = [], []
    for a, b in ((idx[:, :-1], idx[:, 1:]), (idx[:-1, :], idx[1:, :])):
        rows += [a.ravel(), b.ravel()]
        cols += [b.ravel(), a.ravel()]
    rows = np.concatenate(rows)
    cols = np.concatenate(cols)
    adj = sp.csr_matrix((np.ones(len(rows)), (rows, cols)), shape=(n, n))
    deg = np.asarray(adj.sum(1)).ravel()
    return sp.diags(deg) - adj


def _solve(matrix, tone, known):
    h, w = tone.shape
    unk = ~known.ravel()
    if not unk.any():
        return tone.copy()
    x = tone.ravel().astype(np.float64).copy()
    a_uu = matrix[unk][:, unk].tocsc()
    a_uk = matrix[unk][:, ~unk]
    x[unk] = spla.spsolve(a_uu, -a_uk @ x[~unk])
    return x.reshape(h, w)


def membrane(tone, known):
    """T1."""
    return _solve(_laplacian(*tone.shape), tone, known)


def biharmonic(tone, known):
    """T2: minimise the summed squared Laplacian, so value and slope both carry across."""
    lap = _laplacian(*tone.shape)
    return np.clip(_solve((lap.T @ lap).tocsr(), tone, known), 0, 255)


def fitted(tone, known, hole, ring_px=40):
    """T3: robust quadratic over a ring around the hole, plus a membrane fill of the edge residual."""
    h, w = tone.shape
    near = cv2.dilate(hole.astype(np.uint8), np.ones((2 * ring_px + 1,) * 2, np.uint8)).astype(bool)
    ring = known & near
    ys, xs = np.nonzero(ring)
    if len(ys) < 30:
        return membrane(tone, known)
    cy, cx, s = ys.mean(), xs.mean(), max(h, w) / 2.0

    def design(yy, xx):
        u, v = (xx - cx) / s, (yy - cy) / s
        return np.stack([np.ones_like(u), u, v, u * u, u * v, v * v], 1)

    a = design(ys.astype(float), xs.astype(float))
    b = tone[ys, xs]
    wts = np.ones(len(b))
    for _ in range(8):  # iteratively reweighted least squares, Huber at 4 tone levels
        coef, *_ = np.linalg.lstsq(a * wts[:, None], b * wts, rcond=None)
        r = np.abs(b - a @ coef)
        wts = np.sqrt(np.where(r <= 4.0, 1.0, 4.0 / np.maximum(r, 1e-9)))
    gy, gx = np.mgrid[0:h, 0:w]
    surf = (design(gy.ravel().astype(float), gx.ravel().astype(float)) @ coef).reshape(h, w)
    resid = np.where(known, tone - surf, 0.0)
    return np.clip(surf + membrane(resid, known), 0, 255)


def patchmatch(tone, known, hole):
    """T4: PatchMatch completion of the tone map; `hole` is unused, kept for one call shape."""
    gap = ~known  # every unknown pixel is in the hole, so none of them is a source patch
    t = pm.complete(np.where(known, tone, 0)[:, :, None], gap, patch=9, vote_mode="mean")[:, :, 0]
    return np.where(gap, t, tone)
