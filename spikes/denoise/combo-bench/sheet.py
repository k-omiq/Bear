"""Build one self-contained HTML compare sheet + a metrics table from bench crop folders.

    python sheet.py <out.html> <round_dir> [<round_dir> ...]

Metrics per combo (median over crops, no ground truth):
  change   mean abs difference to the source, 0-255 (how much it touched the page)
  flatnoise std of pixels in flat areas (lower = cleaner paper / flat colour)
  sharp    mean gradient magnitude on strong edges, relative to source (1.0 = same)
  block    8x8 JPEG blockiness, relative to source (lower = fewer block edges)
"""

from __future__ import annotations

import base64
import io
import json
import statistics
import sys
from pathlib import Path

import numpy as np
from PIL import Image


def gray(p):
    return np.asarray(Image.open(p).convert("L"), dtype=np.float32)


def resid(img):
    """Pixel minus its 3x3 mean: grain and ringing, not light vs dark."""
    from numpy.lib.stride_tricks import sliding_window_view as sw
    return img - sw(np.pad(img, 1, mode="edge"), (3, 3)).mean(axis=(2, 3))


def metrics(src, out):
    gx = np.abs(np.diff(src, axis=1))[:-1]
    gy = np.abs(np.diff(src, axis=0))[:, :-1]
    grad = gx + gy
    # Flat: low local gradient in the source, in a 5x5 neighbourhood.
    from numpy.lib.stride_tricks import sliding_window_view as sw
    loc = sw(np.pad(grad, 2, mode="edge"), (5, 5)).max(axis=(2, 3))
    flat = loc < 6
    strong = grad > max(np.percentile(grad, 95), 1.0)
    ogx = np.abs(np.diff(out, axis=1))[:-1]
    ogy = np.abs(np.diff(out, axis=0))[:, :-1]
    ograd = ogx + ogy

    def block(img):
        d = np.abs(np.diff(img, axis=1))
        on = d[:, 7::8].mean()
        off = np.delete(d, np.s_[7::8], axis=1).mean()
        return on / max(off, 1e-3)

    return {
        "change": float(np.abs(out - src).mean()),
        "flatnoise": float(np.abs(resid(out)[:-1, :-1][flat]).mean()) if flat.sum() > 500 else float("nan"),
        "level": float(np.median(out - src)),
        "sharp": float(ograd[strong].mean() / max(grad[strong].mean(), 1e-3)),
        "block": float(block(out) / max(block(src), 1e-3)),
    }


def thumb(p, q=88):
    buf = io.BytesIO()
    Image.open(p).convert("RGB").save(buf, "WEBP", quality=q)
    return "data:image/webp;base64," + base64.b64encode(buf.getvalue()).decode()


def main():
    out_html = Path(sys.argv[1])
    rounds = [Path(p) for p in sys.argv[2:]]
    src_dir = rounds[0] / "source"
    crops = sorted(p.name for p in src_dir.glob("*.png"))
    combos = {}
    timings = {}
    for r in rounds:
        t = json.loads((r / "timings.json").read_text())["timings"] if (r / "timings.json").exists() else {}
        for d in sorted(r.iterdir()):
            if d.is_dir() and d.name != "source":
                combos[d.name] = d
                ms = [v["total_ms"] for v in t.get(d.name, {}).values() if "total_ms" in v]
                timings[d.name] = statistics.median(ms) / 1000 if ms else float("nan")
    table = {}
    for name, d in combos.items():
        rows = []
        for c in crops:
            if (d / c).exists():
                rows.append(metrics(gray(src_dir / c), gray(d / c)))
        table[name] = {k: statistics.median([r[k] for r in rows if r[k] == r[k]] or [float("nan")])
                       for k in ("change", "flatnoise", "sharp", "block", "level")}
        table[name]["n"] = len(rows)
        table[name]["sec"] = timings[name]
    srcm = [metrics(gray(src_dir / c), gray(src_dir / c)) for c in crops]
    table["source"] = {"change": 0.0, "flatnoise": statistics.median([r["flatnoise"] for r in srcm if r["flatnoise"] == r["flatnoise"]]),
                       "sharp": 1.0, "block": 1.0, "level": 0.0, "n": len(crops), "sec": 0.0}
    (out_html.with_suffix(".json")).write_text(json.dumps(table, indent=1))

    data = {"crops": crops, "combos": ["source", *combos], "img": {}}
    for c in crops:
        data["img"][c] = {"source": thumb(src_dir / c)}
        for name, d in combos.items():
            if (d / c).exists():
                data["img"][c][name] = thumb(d / c)
    rows = "".join(
        f"<tr><td>{k}</td><td>{v['sec']:.1f}</td><td>{v['change']:.1f}</td><td>{v['flatnoise']:.2f}</td>"
        f"<td>{v['sharp']:.2f}</td><td>{v['block']:.2f}</td></tr>"
        for k, v in sorted(table.items(), key=lambda kv: kv[0] != "source"))
    html = TEMPLATE.replace("__ROWS__", rows).replace("__DATA__", json.dumps(data))
    out_html.write_text(html)
    print(out_html, f"{out_html.stat().st_size / 1e6:.1f} MB", len(crops), "crops", len(combos), "combos")


TEMPLATE = """<!doctype html><html><head><meta charset="utf-8"><title>Denoise combos</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
:root{--bg:#f6f5f2;--fg:#1d1d1f;--mut:#6b6b70;--line:#d9d7d2;--acc:#2f5bd3}
@media (prefers-color-scheme:dark){:root{--bg:#161618;--fg:#ececee;--mut:#9a9aa0;--line:#333338;--acc:#8aa8ff}}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.4 system-ui,sans-serif;padding:16px}
h1{font-size:18px;margin:0 0 8px}table{border-collapse:collapse;margin:8px 0 16px;font-variant-numeric:tabular-nums}
td,th{border-bottom:1px solid var(--line);padding:3px 10px;text-align:right}td:first-child,th:first-child{text-align:left}
.bar{display:flex;gap:8px;flex-wrap:wrap;align-items:center;margin:8px 0;position:sticky;top:0;background:var(--bg);padding:6px 0;z-index:2}
select,button{font:inherit;padding:4px 8px;background:transparent;color:var(--fg);border:1px solid var(--line);border-radius:6px}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:10px}
figure{margin:0}figure img{width:100%;image-rendering:pixelated;display:block;border:1px solid var(--line)}
figcaption{font-size:12px;color:var(--mut);margin-top:2px}.z img{width:200%}figure{overflow:hidden}
.note{color:var(--mut);font-size:12px;max-width:70ch}
</style></head><body>
<h1>Denoise combos, chapters 49 and 50</h1>
<p class="note">Seconds = median GPU time per page on an L4. change = how much it altered the page (0-255).
flatnoise = grain left in flat areas (lower is cleaner). sharp = edge strength vs source (1.0 same; below 1 = softer).
block = JPEG 8x8 block edges vs source (lower is better).</p>
<table><tr><th>combo</th><th>sec</th><th>change</th><th>flatnoise</th><th>sharp</th><th>block</th></tr>__ROWS__</table>
<div class="bar"><button id="prev">◀</button><select id="crop"></select><button id="next">▶</button>
<label><input type="checkbox" id="zoom"> 2x zoom</label></div>
<div class="grid" id="grid"></div>
<script>
const D=__DATA__;const sel=document.getElementById('crop');
D.crops.forEach((c,i)=>{const o=document.createElement('option');o.value=i;o.textContent=c;sel.appendChild(o)});
function show(){const c=D.crops[sel.value];const g=document.getElementById('grid');g.innerHTML='';
g.classList.toggle('z',document.getElementById('zoom').checked);
D.combos.forEach(k=>{const src=D.img[c][k];if(!src)return;const f=document.createElement('figure');
f.innerHTML=`<img src="${src}"><figcaption>${k}</figcaption>`;g.appendChild(f)})}
sel.onchange=show;document.getElementById('zoom').onchange=show;
document.getElementById('prev').onclick=()=>{sel.value=Math.max(0,+sel.value-1);show()};
document.getElementById('next').onclick=()=>{sel.value=Math.min(D.crops.length-1,+sel.value+1);show()};
show();
</script></body></html>"""

if __name__ == "__main__":
    main()
