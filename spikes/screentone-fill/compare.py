"""Run fill candidates over the screentone corpus and compare them.

usage: compare.py <run name> <candidate> [<candidate> ...] [--pads 3,6] [--cases id,id]

A candidate is a module in this folder with `fill(gray, hole, avoid) -> uint8 gray`, or
`-> (uint8 gray, used)` when it replaces more than the mask (used: bool, what it replaced):
  gray  - the case's working crop, 2-D uint8
  hole  - bool, True where the text mask is (the pixels to replace)
  avoid - bool, True on every other detected region in the crop (never a source)
Writes out/<run name>/: metrics.json, metrics.tsv, one sheet per case and padding, and prints a table.

Metrics (see docs/patch-gradient-plan.md, "How results are compared"):
  Every reference below reads only screen-like pixels outside the hole (see screen_like), so glow
  or outline the mask left behind does not count as the background.
  tone_err  mean |tone(fill) - tone_ref| over the hole, tone = blur at sigma 2.5; tone_ref is the
            ring tone carried across the hole by row-wise and column-wise linear interpolation,
            averaged (a neutral reference that is none of the candidates' own tone methods)
  ramp_err  |slope(fill) - slope(ref)| of a plane fitted to each over the hole, tone units / 100 px
  seam      mean |tone(fill) - tone(original at the nearest pixel outside the hole)| on the hole's
            inner 3 px band
  phase     correlation of the fill's dot detail with the dots predicted from outside the hole
            through the screen's repeat offsets (1 = dots perfectly in phase; blank if no screen)
  remnant   pixels in or within 7 px of the mask whose fine tone (sigma 1) is past the screen ring's
            1st..99th percentile by max(12, 0.3 x that range): lettering left or copied, lights, ink.
            Real art in the band counts too, the same for all; candidate c_none is the start count
  grown     pixels a candidate replaced outside the detected mask (a remnant guard grows it)
  tone_err, ramp_err and phase are measured on the detected mask; seam on what was replaced
  time_ms   wall time of fill()
"""
import argparse
import importlib
import json
import os
import struct
import sys
import time

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
CORPUS = os.path.join(HERE, "corpus")
MARGIN = 110


def load_masks(page, pad, shape):
    """Every stored detection mask of the probe run, as (bbox, full-page bool mask)."""
    man_path = os.path.join(CORPUS, "detect", f"{page}_pad{pad}", "library", "p1", "c2.mtclean")
    man = json.load(open(man_path))
    out = []
    H, W = shape
    for r in man["detections"]:
        b = open(man_path + ".d/" + r["mask_ref"], "rb").read()
        assert b[:8] == b"MTCLEANM" and b[8] == 1
        x, y = struct.unpack_from("<qq", b, 9)
        w, h = struct.unpack_from("<II", b, 25)
        bits = np.frombuffer(b, np.uint8, w * h, 33).reshape(h, w) > 0
        m = np.zeros((H, W), bool)
        x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + w, W), min(y + h, H)
        m[y0:y1, x0:x1] = bits[y0 - y:y1 - y, x0 - x:x1 - x]
        out.append((r["bbox"], m))
    return out


def case_inputs(case, pad):
    gray = cv2.imread(os.path.join(CORPUS, "pages", case["page"] + ".png"), cv2.IMREAD_GRAYSCALE)
    H, W = gray.shape
    bx, by, bw, bh = case["box"]
    hole = np.zeros((H, W), bool)
    avoid = np.zeros((H, W), bool)
    for bbox, m in load_masks(case["page"], pad, (H, W)):
        ix = max(0, min(bx + bw, bbox["x"] + bbox["w"]) - max(bx, bbox["x"]))
        iy = max(0, min(by + bh, bbox["y"] + bbox["h"]) - max(by, bbox["y"]))
        if ix * iy >= 0.3 * bbox["w"] * bbox["h"] or ix * iy >= 0.3 * bw * bh:
            hole |= m
        else:
            avoid |= m
    ys, xs = np.nonzero(hole)
    if len(ys) == 0:
        raise SystemExit(f"{case['id']} pad {pad}: no detected mask overlaps the box")
    x0, y0 = max(0, xs.min() - MARGIN), max(0, ys.min() - MARGIN)
    x1, y1 = min(W, xs.max() + MARGIN + 1), min(H, ys.max() + MARGIN + 1)
    sl = (slice(y0, y1), slice(x0, x1))
    return gray[sl].copy(), hole[sl].copy(), (avoid & ~hole)[sl].copy(), (x0, y0)


def blur_known(img, known, sigma):
    w = cv2.GaussianBlur(known.astype(np.float64), (0, 0), sigma)
    t = cv2.GaussianBlur(np.where(known, img, 0.0), (0, 0), sigma)
    return t / np.maximum(w, 1e-6), w


def screen_like(gray, hole):
    """Pixels outside the hole whose fine detail looks like the surrounding screen.

    A white glow or outline left outside the mask is flat, so its local spread is far below the
    screen's; it is left out of every reference, or the metrics would reward copying the glow.
    """
    g = gray.astype(np.float64)
    mean = cv2.GaussianBlur(g, (0, 0), 1.5)
    std = np.sqrt(np.maximum(cv2.GaussianBlur(g * g, (0, 0), 1.5) - mean * mean, 0))
    known = ~cv2.dilate(hole.astype(np.uint8), np.ones((5, 5), np.uint8)).astype(bool)
    ring = known & cv2.dilate(hole.astype(np.uint8), np.ones((61, 61), np.uint8)).astype(bool)
    level = np.median(std[ring]) if ring.any() else 0
    if level < 1.5:  # flat paper or flat tone: no screen to tell apart, keep every known pixel
        return known
    return known & (std >= 0.4 * level)


def reference_tone(gray, hole):
    """Ring tone carried across the hole by row and column linear interpolation, averaged."""
    known = screen_like(gray, hole)
    tone, _ = blur_known(gray.astype(np.float64), known, 2.5)
    H, W = hole.shape
    ref = np.zeros((H, W))
    cnt = np.zeros((H, W))
    for axis in (0, 1):
        t = tone if axis == 1 else tone.T
        k = known if axis == 1 else known.T
        hm = hole if axis == 1 else hole.T
        r = np.zeros(t.shape)
        c = np.zeros(t.shape)
        for i in range(t.shape[0]):
            if not hm[i].any():
                continue
            idx = np.nonzero(k[i])[0]
            if len(idx) < 2:
                continue
            row_hole = np.nonzero(hm[i])[0]
            # only interpolate between known pixels on both sides
            inside = (row_hole > idx.min()) & (row_hole < idx.max())
            xs = row_hole[inside]
            r[i, xs] = np.interp(xs, idx, t[i, idx])
            c[i, xs] = 1
        if axis == 0:
            r, c = r.T, c.T
        ref += r
        cnt += c
    ok = hole & (cnt > 0)
    ref[ok] /= cnt[ok]
    return ref, ok


def plane_slope(v, m):
    ys, xs = np.nonzero(m)
    if len(ys) < 50:
        return np.zeros(2)
    A = np.stack([np.ones(len(xs)), xs / 100.0, ys / 100.0], 1)
    coef, *_ = np.linalg.lstsq(A, v[ys, xs], rcond=None)
    return coef[1:]


def repeat_offsets(gray, known):
    import lattice
    g = gray.astype(np.float64)
    tone, _ = blur_known(g, known, 2.0)
    offs, _ = lattice.repeat_offsets(g - tone, known, radius=24, thresh=0.5)
    return offs


def metrics(gray, hole, out, offs, used):
    """`hole` is the detected mask, the same pixels for every candidate; `used` is what the candidate
    replaced (the mask, or more if it grew it)."""
    g = gray.astype(np.float64)
    o = out.astype(np.float64)
    core = cv2.erode(hole.astype(np.uint8), np.ones((5, 5), np.uint8)).astype(bool)
    if core.sum() < 50:
        core = hole
    ref, ok = reference_tone(gray, hole)
    tone_o = cv2.GaussianBlur(o, (0, 0), 2.5)
    m = core & ok
    tone_err = float(np.abs(tone_o[m] - ref[m]).mean()) if m.any() else float("nan")
    ramp_err = float(np.abs(plane_slope(tone_o, m) - plane_slope(ref, m)).sum()) if m.any() else float("nan")
    # seam: inner band of the replaced region against the original's tone at the nearest outside pixel
    band = used & ~cv2.erode(used.astype(np.uint8), np.ones((7, 7), np.uint8)).astype(bool)
    known = screen_like(gray, used)
    tone_g, _ = blur_known(g, known, 2.5)
    _, labels = cv2.distanceTransformWithLabels((~known).astype(np.uint8), cv2.DIST_L2, 5, labelType=cv2.DIST_LABEL_PIXEL)
    ky, kx = np.nonzero(known)
    lut = np.zeros(labels.max() + 1, np.int64)
    lab_k = labels[ky, kx]
    lut[lab_k] = np.arange(len(ky))
    near = lut[labels[band]]
    seam = float(np.abs(tone_o[band] - tone_g[ky[near], kx[near]]).mean()) if band.any() else float("nan")
    # remnant: fine-tone outliers against the screen ring, in and around the mask (7 px), so lettering
    # left outside a tight mask counts too. Real art in that band (bars, lines) counts as well, the
    # same for every candidate; the unchanged page (candidate c_none) gives the starting count.
    screen = screen_like(gray, hole)
    ring = cv2.dilate(hole.astype(np.uint8), np.ones((41, 41), np.uint8)).astype(bool) & screen
    fine_g = cv2.GaussianBlur(g, (0, 0), 1.0)
    fine_o = cv2.GaussianBlur(o, (0, 0), 1.0)
    lo, hi = np.percentile(fine_g[ring], [1, 99])
    margin = max(12.0, 0.3 * (hi - lo))
    near = cv2.dilate(hole.astype(np.uint8), np.ones((15, 15), np.uint8)).astype(bool)
    remnant = int(((fine_o > hi + margin) | (fine_o < lo - margin))[near].sum())
    # phase: predicted dots from outside through the repeat offsets
    phase = float("nan")
    if len(offs):
        hp_g = g - cv2.GaussianBlur(g, (0, 0), 2.0)
        hp_o = o - cv2.GaussianBlur(o, (0, 0), 2.0)
        H, W = g.shape
        hy, hx = np.nonzero(core)
        acc = np.zeros(len(hy))
        n = np.zeros(len(hy))
        near_offs = offs[np.argsort((offs ** 2).sum(1))][:16]
        for dy, dx in near_offs:
            sy, sx = hy + dy, hx + dx
            v = (sy >= 0) & (sy < H) & (sx >= 0) & (sx < W)
            v[v] &= known[sy[v], sx[v]]
            acc[v] += hp_g[sy[v], sx[v]]
            n[v] += 1
        sel = n > 0
        if sel.sum() > 50:
            pred = acc[sel] / n[sel]
            got = hp_o[hy[sel], hx[sel]]
            phase = float(np.corrcoef(pred, got)[0, 1])
    return dict(grown=int((used & ~hole).sum()), tone_err=round(tone_err, 2), ramp_err=round(ramp_err, 2), seam=round(seam, 2),
                phase=round(phase, 3) if phase == phase else None, remnant=remnant)


def sheet(gray, hole, outs, names, useds, path):
    ys, xs = np.nonzero(hole)
    pad = 30
    y0, y1 = max(0, ys.min() - pad), min(gray.shape[0], ys.max() + pad)
    x0, x1 = max(0, xs.min() - pad), min(gray.shape[1], xs.max() + pad)
    z = 2 if max(y1 - y0, x1 - x0) < 400 else 1

    def tile(img, label, overlay=False, grown=None):
        c = cv2.cvtColor(img[y0:y1, x0:x1], cv2.COLOR_GRAY2BGR)
        if overlay:
            c[hole[y0:y1, x0:x1]] = (0.5 * c[hole[y0:y1, x0:x1]] + [0, 0, 127]).astype(np.uint8)
        if grown is not None:
            gm = grown[y0:y1, x0:x1]
            c[gm] = (0.5 * c[gm] + [127, 60, 0]).astype(np.uint8)
        c = cv2.resize(c, None, fx=z, fy=z, interpolation=cv2.INTER_NEAREST)
        c = np.pad(c, ((26, 6), (0, 6), (0, 0)), constant_values=255)
        cv2.putText(c, label, (4, 18), cv2.FONT_HERSHEY_SIMPLEX, 0.5, (0, 0, 0), 1)
        return c

    tiles = [tile(gray, "original"), tile(gray, "mask", True)]
    for o, n, u in zip(outs, names, useds):
        if (u & ~hole).any():
            tiles.append(tile(gray, n + " mask", True, u & ~hole))
        tiles.append(tile(o, n))
    cv2.imwrite(path, np.hstack(tiles))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run")
    ap.add_argument("candidates", nargs="+")
    ap.add_argument("--pads", default="3,6")
    ap.add_argument("--cases", default="")
    a = ap.parse_args()
    cases = json.load(open(os.path.join(CORPUS, "cases.json")))["cases"]
    if a.cases:
        keep = set(a.cases.split(","))
        cases = [c for c in cases if c["id"] in keep]
    mods = [importlib.import_module(n) for n in a.candidates]
    out_dir = os.path.join(HERE, "out", a.run)
    os.makedirs(out_dir, exist_ok=True)
    rows = []
    for case in cases:
        for pad in map(int, a.pads.split(",")):
            gray, hole, avoid, origin = case_inputs(case, pad)
            known = ~cv2.dilate(hole.astype(np.uint8), np.ones((7, 7), np.uint8)).astype(bool) & ~avoid
            offs = repeat_offsets(gray, known)
            outs, useds = [], []
            for name, mod in zip(a.candidates, mods):
                t = time.time()
                res = mod.fill(gray.copy(), hole.copy(), avoid.copy())
                ms = int((time.time() - t) * 1000)
                # a candidate may grow the mask (remnant guard) and returns (out, used) then
                out, used = res if isinstance(res, tuple) else (res, hole)
                used = used | hole
                out = np.where(used, out, gray).astype(np.uint8)  # nothing outside what it claims may change
                outs.append(out)
                useds.append(used)
                row = dict(case=case["id"], pad=pad, candidate=name, **metrics(gray, hole, out, offs, used), time_ms=ms)
                rows.append(row)
                print(row, flush=True)
            sheet(gray, hole, outs, a.candidates, useds, os.path.join(out_dir, f"{case['id']}_pad{pad}.png"))
    json.dump(rows, open(os.path.join(out_dir, "metrics.json"), "w"), indent=1)
    cols = ["case", "pad", "candidate", "grown", "tone_err", "ramp_err", "seam", "phase", "remnant", "time_ms"]
    with open(os.path.join(out_dir, "metrics.tsv"), "w") as f:
        f.write("\t".join(cols) + "\n")
        for r in rows:
            f.write("\t".join(str(r[c]) for c in cols) + "\n")
    # summary per candidate and padding: means, remnant summed
    print("\ncandidate\tpad\tgrown\ttone_err\tramp_err\tseam\tphase\tremnant\ttime_ms")
    for name in a.candidates:
        for pad in map(int, a.pads.split(",")):
            rs = [r for r in rows if r["candidate"] == name and r["pad"] == pad]
            ph = [r["phase"] for r in rs if r["phase"] is not None]
            print(f"{name}\t{pad}\t{sum(r['grown'] for r in rs)}\t{np.mean([r['tone_err'] for r in rs]):.2f}\t{np.mean([r['ramp_err'] for r in rs]):.2f}\t"
                  f"{np.mean([r['seam'] for r in rs]):.2f}\t{(np.mean(ph) if ph else float('nan')):.3f}\t"
                  f"{sum(r['remnant'] for r in rs)}\t{int(np.mean([r['time_ms'] for r in rs]))}")


if __name__ == "__main__":
    main()
