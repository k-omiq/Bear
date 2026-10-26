"""Find regions in the app library whose surroundings are a halftone screen.

Read-only: opens manifests and page images, writes nothing into the library.

usage: scan_library.py <library dir> <out dir> [--top 40]

For every region a manifest knows (detections, held regions, patches) on a single-page chapter, it
measures a ring around the region's box on the source page:
  - screen: the strongest normalised autocorrelation of the ring's dot detail at a small non-zero
    offset (a printed screen repeats every few pixels; paper, flat colour and art do not),
  - tone: the ring's blurred mean (0 black, 255 white), so blank paper is skipped.
Writes candidates.tsv (best first) and sheet_NN.png contact sheets of the top regions.
"""
import argparse
import glob
import json
import os

import cv2
import numpy as np


def screen_score(gray, ring):
    """Max normalised autocorrelation of the ring's high-pass at offsets 2..9 px, and its period.

    Masked autocorrelation by FFT: sum(a(p) a(p+d)) over pixels where both are in the ring, divided
    by the ring energy, so holes in the ring (the region itself) do not count.
    """
    g = gray.astype(np.float64)
    hp = g - cv2.GaussianBlur(g, (0, 0), 1.5)
    m = ring.astype(np.float64)
    a = hp * m
    if m.sum() < 400 or (a * a).sum() / m.sum() < 25:  # too few pixels, or no dot detail at all
        return 0.0, 0
    h, w = a.shape
    H, W = cv2.getOptimalDFTSize(h + 10), cv2.getOptimalDFTSize(w + 10)
    fa = np.fft.rfft2(a, (H, W))
    fm = np.fft.rfft2(m, (H, W))
    fe = np.fft.rfft2(a * a, (H, W))
    num = np.fft.irfft2(fa * np.conj(fa), (H, W))
    cnt = np.fft.irfft2(fm * np.conj(fm), (H, W))
    en = np.fft.irfft2(fe * np.conj(fm), (H, W))
    best, period, worst = 0.0, 0, 0.0
    for dy in range(-9, 10):
        for dx in range(-9, 10):
            r2 = dy * dy + dx * dx
            if r2 < 1 or r2 > 81 or cnt[dy % H, dx % W] < 300:
                continue
            # energy of each side of the overlap: en(d) for one, en(-d) for the other
            c = num[dy % H, dx % W] / (np.sqrt(en[dy % H, dx % W] * en[-dy % H, -dx % W]) + 1e-6)
            worst = min(worst, c)
            if r2 >= 4 and c > best:
                best, period = c, int(round(np.sqrt(r2)))
    # A dot screen swings: strongly alike one period away and strongly opposite half a period
    # away. Smooth paper, upscaling and noise are alike nearby and never strongly opposite.
    best = min(best, -worst)
    return float(best), period


def regions_of(man):
    for r in man.get("detections", []):
        yield r["source_idx"], r["bbox"], "detected"
    for r in man.get("regions_untouched", []):
        yield r["source_idx"], r["bbox"], r.get("reason", "held").split(".")[-1]
    for r in man.get("patches", []):
        if r.get("engine") not in ("paint", "clone"):
            yield r["source_idx"], r["bbox"], "patch:" + str(r.get("engine"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("library")
    ap.add_argument("out")
    ap.add_argument("--top", type=int, default=40)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    index = json.load(open(os.path.join(a.library, "index.json")))
    names = {}
    for p in index.get("projects", []):
        for c in p.get("chapters", []):
            names[(p["id"], c["id"])] = f'{p.get("name")} / {c.get("name")}'
    rows = []
    for mpath in sorted(glob.glob(os.path.join(a.library, "p*", "c*.mtclean"))):
        man = json.load(open(mpath))
        strip = man.get("strip")
        mode = strip.get("mode") if isinstance(strip, dict) else strip
        if mode == "longstrip":
            continue
        pid = os.path.basename(os.path.dirname(mpath))
        cid = os.path.basename(mpath).split(".")[0]
        by_page = {}
        for idx, bbox, kind in regions_of(man):
            by_page.setdefault(idx, []).append((bbox, kind))
        for idx, regs in by_page.items():
            src = man["sources"][idx]
            path = os.path.normpath(os.path.join(os.path.dirname(mpath), src["rel_path"]))
            if not os.path.exists(path):
                continue
            img = cv2.imread(path, cv2.IMREAD_UNCHANGED)
            if img is None:
                continue
            if img.ndim == 3:
                color = img[:, :, :3]
                gray = cv2.cvtColor(color, cv2.COLOR_BGR2GRAY)
                chroma = float(np.abs(color.astype(int) - gray[:, :, None].astype(int)).mean())
            else:
                gray, chroma = img, 0.0
            H, W = gray.shape
            seen = set()
            for bbox, kind in regs:
                key = (bbox["x"], bbox["y"], bbox["w"], bbox["h"])
                if key in seen or bbox["w"] < 12 or bbox["h"] < 12:
                    continue
                seen.add(key)
                pad = max(12, min(40, int(0.25 * min(bbox["w"], bbox["h"]))))
                x0, y0 = max(0, bbox["x"] - pad), max(0, bbox["y"] - pad)
                x1, y1 = min(W, bbox["x"] + bbox["w"] + pad), min(H, bbox["y"] + bbox["h"] + pad)
                crop = gray[y0:y1, x0:x1]
                ring = np.ones(crop.shape, bool)
                ring[bbox["y"] - y0:bbox["y"] - y0 + bbox["h"], bbox["x"] - x0:bbox["x"] - x0 + bbox["w"]] = False
                tone = float(crop[ring].mean())
                if tone > 225:
                    continue
                score, period = screen_score(crop, ring)
                rows.append(dict(score=round(score, 3), period=period, tone=round(tone, 1), chroma=round(chroma, 1),
                                 kind=kind, project=pid, chapter=cid, title=names.get((pid, cid), ""),
                                 page=idx, path=path, x=bbox["x"], y=bbox["y"], w=bbox["w"], h=bbox["h"]))
        print(f"{mpath}: {len(rows)} rows so far", flush=True)
    rows.sort(key=lambda r: -r["score"])
    cols = list(rows[0].keys()) if rows else []
    with open(os.path.join(a.out, "candidates.tsv"), "w") as f:
        f.write("\t".join(cols) + "\n")
        for r in rows:
            f.write("\t".join(str(r[c]) for c in cols) + "\n")
    # contact sheets: 4 x 3 tiles of 300 x 300, each the region plus its ring
    top = rows[: a.top]
    for s in range(0, len(top), 12):
        tiles = []
        for i, r in enumerate(top[s:s + 12]):
            img = cv2.imread(r["path"], cv2.IMREAD_COLOR)
            pad = 40
            x0, y0 = max(0, r["x"] - pad), max(0, r["y"] - pad)
            crop = img[y0:r["y"] + r["h"] + pad, x0:r["x"] + r["w"] + pad].copy()
            cv2.rectangle(crop, (r["x"] - x0, r["y"] - y0), (r["x"] - x0 + r["w"], r["y"] - y0 + r["h"]), (255, 160, 0), 1)
            sc = 280 / max(crop.shape[:2])
            crop = cv2.resize(crop, None, fx=sc, fy=sc, interpolation=cv2.INTER_AREA if sc < 1 else cv2.INTER_NEAREST)
            tile = np.full((300, 300, 3), 255, np.uint8)
            tile[20:20 + crop.shape[0], 10:10 + crop.shape[1]] = crop
            cv2.putText(tile, f"#{s + i + 1} s{r['score']} t{r['tone']:.0f}", (4, 14), cv2.FONT_HERSHEY_SIMPLEX, 0.42, (0, 0, 0), 1)
            tiles.append(tile)
        while len(tiles) % 4:
            tiles.append(np.full((300, 300, 3), 255, np.uint8))
        sheet = np.vstack([np.hstack(tiles[k:k + 4]) for k in range(0, len(tiles), 4)])
        cv2.imwrite(os.path.join(a.out, f"sheet_{s // 12 + 1:02d}.png"), sheet)
    print(f"{len(rows)} regions scored; top {len(top)} drawn")


if __name__ == "__main__":
    main()
