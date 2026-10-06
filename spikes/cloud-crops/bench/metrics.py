"""Per-variant numbers for a benchmark results directory.

    python3 metrics.py CROPS RESULTS [--ref VARIANT] [--page 1080x1536]

Reads each variant's raw answers (`<id>.png`), composites (`<id>.composite.png`)
and `scores.jsonl` from `spike-cloud-crops composite`, and prints one row per
variant. Only numpy and Pillow.

- text_ink: mean text-detector level left on the lettering, after compositing.
  The original page's level is the first row. Lower is better.
- text_left: share of crops whose lettering still reads as text (level > 0.2).
- out_diff: mean absolute difference, 0-255, between the raw answer and the
  crop outside the write mask. How much the model changed what it was told to
  keep: redrawn screentone, tone shifts, drift.
- drift: median and max whole-crop shift in pixels between the raw answer and
  the crop, measured outside the write mask by phase correlation.
- bg_gap: mean absolute gap, 0-255, between the composite over the lettering
  and the page around the lettering inside the write mask (the page the text
  sits on). Catches an edit that repainted a white balloon grey, or erased
  the balloon itself. Art under the text makes it nonzero legitimately, so it
  compares variants rather than judging one.
- ink_left: of the lettering pixels that were dark in the page (below 80),
  the share still dark after the edit. Catches a sound effect the model left
  in place, which the text detector often does not see as text to begin with.
- vs_ref: mean absolute difference inside the write mask between this
  variant's composite and the reference variant's. Only meaningful against a
  second seed of the reference, which is the noise floor.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image


def gray(path: Path) -> np.ndarray:
    return np.asarray(Image.open(path).convert("L"), dtype=np.float64)


def rgb(path: Path) -> np.ndarray:
    return np.asarray(Image.open(path).convert("RGB"), dtype=np.float64)


def on_page(meta: dict, page_w: int, page_h: int) -> np.ndarray:
    x, y, w, h = meta["crop"]
    ys = np.arange(y, y + h)[:, None]
    xs = np.arange(x, x + w)[None, :]
    return (xs >= 0) & (xs < page_w) & (ys >= 0) & (ys < page_h)


def phase_shift(a: np.ndarray, b: np.ndarray, weight: np.ndarray) -> float:
    """Magnitude of the translation between a and b, sub-pixel, over `weight`."""
    window = np.outer(np.hanning(a.shape[0]), np.hanning(a.shape[1])) * weight
    fa = np.fft.fft2((a - a[weight > 0].mean()) * window)
    fb = np.fft.fft2((b - b[weight > 0].mean()) * window)
    cross = fa * np.conj(fb)
    cross /= np.abs(cross) + 1e-9
    corr = np.fft.ifft2(cross).real
    py, px = np.unravel_index(np.argmax(corr), corr.shape)

    def refine(c_minus: float, c0: float, c_plus: float) -> float:
        denom = c_minus - 2 * c0 + c_plus
        return 0.0 if denom == 0 else 0.5 * (c_minus - c_plus) / denom

    h, w = corr.shape
    dy = py + refine(corr[(py - 1) % h, px], corr[py, px], corr[(py + 1) % h, px])
    dx = px + refine(corr[py, (px - 1) % w], corr[py, px], corr[py, (px + 1) % w])
    dy = dy - h if dy > h / 2 else dy
    dx = dx - w if dx > w / 2 else dx
    return float(np.hypot(dx, dy))


def scores(path: Path) -> dict:
    if not path.is_file():
        return {}
    return {j["id"]: j for j in (json.loads(line) for line in path.read_text().splitlines() if line.strip())}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("crops")
    ap.add_argument("results")
    ap.add_argument("--ref", default="")
    ap.add_argument("--page", default="1080x1536")
    ap.add_argument("--json", default="")
    args = ap.parse_args()
    crops, results = Path(args.crops), Path(args.results)
    page_w, page_h = (int(v) for v in args.page.split("x"))
    original = scores(results / "original.scores.jsonl")

    rows = []
    variants = sorted(p for p in results.iterdir() if p.is_dir())
    ref_dir = results / args.ref if args.ref else None
    for variant in variants:
        answers = sorted(p for p in variant.glob("*.png") if not p.name.endswith(".composite.png"))
        if not answers:
            continue
        s = scores(variant / "scores.jsonl")
        text, out_diff, drift, vs_ref, base_text, bg_gap, ink_left = [], [], [], [], [], [], []
        per_crop = {}
        for answer in answers:
            rid = answer.stem
            meta = json.loads((crops / rid / "meta.json").read_text())
            crop_rgb = rgb(crops / rid / "crop.png")
            alpha = np.asarray(Image.open(crops / rid / "alpha.png"), dtype=np.float64)
            pw, ph = meta.get("page_size", (page_w, page_h))
            keep = (alpha == 0) & on_page(meta, pw, ph)
            ans = rgb(answer)
            d = float(np.abs(ans - crop_rgb)[keep].mean()) if keep.any() else float("nan")
            shift = phase_shift(gray(answer), gray(crops / rid / "crop.png"), keep.astype(np.float64))
            out_diff.append(d)
            drift.append(shift)
            row = {"out_diff": d, "drift": shift}
            comp_path = variant / f"{rid}.composite.png"
            if comp_path.is_file():
                hint = np.asarray(Image.open(crops / rid / "hint.png"), dtype=np.float64) > 0
                ring = (alpha > 0) & ~hint & on_page(meta, pw, ph)
                hole = hint & on_page(meta, pw, ph)
                if ring.any() and hole.any():
                    g = gray(comp_path)
                    o = gray(crops / rid / "crop.png")
                    row["bg_gap"] = float(abs(g[hole].mean() - o[ring].mean()))
                    bg_gap.append(row["bg_gap"])
                dark = hole & (gray(crops / rid / "crop.png") < 80)
                if dark.sum() >= 50:
                    row["ink_left"] = float((gray(comp_path)[dark] < 80).mean())
                    ink_left.append(row["ink_left"])
            if rid in s and s[rid]["text_on_ink"] is not None:
                text.append(s[rid]["text_on_ink"])
                row["text_ink"] = s[rid]["text_on_ink"]
                base_text.append(original.get(rid, {}).get("text_on_ink") or 0.0)
            if ref_dir and (ref_dir / f"{rid}.composite.png").is_file() and (variant / f"{rid}.composite.png").is_file():
                inside = alpha > 0
                a = rgb(variant / f"{rid}.composite.png")
                r = rgb(ref_dir / f"{rid}.composite.png")
                row["vs_ref"] = float(np.abs(a - r)[inside].mean())
                vs_ref.append(row["vs_ref"])
            per_crop[rid] = row
        stats = json.loads((variant / "stats.json").read_text()) if (variant / "stats.json").is_file() else {}
        warm = sorted(stats.get("per_crop_seconds", [])[1:])
        rows.append({
            "variant": variant.name,
            "n": len(answers),
            "text_ink": float(np.mean(text)) if text else None,
            "text_left": float(np.mean(np.array(text) > 0.2)) if text else None,
            "orig_text_ink": float(np.mean(base_text)) if base_text else None,
            "out_diff": float(np.mean(out_diff)),
            "drift_median": float(np.median(drift)),
            "drift_max": float(np.max(drift)),
            "vs_ref": float(np.mean(vs_ref)) if vs_ref else None,
            "bg_gap": float(np.mean(bg_gap)) if bg_gap else None,
            "ink_left": float(np.mean(ink_left)) if ink_left else None,
            "bg_gap_p90": float(np.percentile(bg_gap, 90)) if bg_gap else None,
            "sec_median": warm[len(warm) // 2] if warm else None,
            "gpu": stats.get("gpu"),
            "load_s": stats.get("load_seconds"),
            "peak_vram": stats.get("peak_vram_gib"),
            "per_crop": per_crop,
        })

    def f(v, spec):
        return "-" if v is None else format(v, spec)

    print(f"{'variant':<22}{'n':>4}{'text_ink':>9}{'orig':>7}{'left':>6}{'out_diff':>9}{'drift~':>8}{'drift^':>8}"
          f"{'bg_gap':>7}{'gap90':>7}{'inkL':>6}{'vs_ref':>8}{'s/crop':>8}{'load':>7}{'VRAM':>7}  gpu")
    for r in rows:
        print(f"{r['variant']:<22}{r['n']:>4}{f(r['text_ink'], '.3f'):>9}{f(r['orig_text_ink'], '.3f'):>7}"
              f"{f(r['text_left'], '.2f'):>6}{f(r['out_diff'], '.2f'):>9}{f(r['drift_median'], '.2f'):>8}"
              f"{f(r['drift_max'], '.2f'):>8}{f(r['bg_gap'], '.1f'):>7}{f(r['bg_gap_p90'], '.1f'):>7}{f(r['ink_left'], '.2f'):>6}{f(r['vs_ref'], '.2f'):>8}{f(r['sec_median'], '.2f'):>8}"
              f"{f(r['load_s'], '.0f'):>7}{f(r['peak_vram'], '.1f'):>7}  {r['gpu'] or ''}")
    if args.json:
        Path(args.json).write_text(json.dumps(rows, indent=1))


if __name__ == "__main__":
    main()
