"""Page denoise comparison on Modal: every variant below over one chapter.

Runs the production runtime (deploy/cloud/common/denoise.py) in an ephemeral app on
its own volume, so the installation's deployed app and weights volume are untouched.

    modal run spikes/denoise/modal_compare.py --chapter "<dir of PNG pages>" --out <dir> [--limit N]
    modal volume delete mc-denoise-spike   # afterwards

Writes <out>/<variant>/<page>.png and <out>/timings.json.
"""

from __future__ import annotations

import json
import time
from pathlib import Path

import modal

RUNTIME = Path(__file__).resolve().parents[2] / "deploy" / "cloud" / "common" / "denoise.py" if modal.is_local() else None
VOLUME = "mc-denoise-spike"
ROOT = "/weights"

image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch==2.13.0", "torchvision==0.28.0", index_url="https://download.pytorch.org/whl/cu129")
    .pip_install("onnxruntime-gpu==1.23.2", "numpy==2.4.6", "pillow==12.3.0", "spandrel==0.4.2")
)
if modal.is_local():
    image = image.add_local_file(RUNTIME, "/root/mc_denoise.py")
volume = modal.Volume.from_name(VOLUME, create_if_missing=True)
app = modal.App("mc-denoise-spike")


def _w(op, engine, **extra):
    return {"op": op, "engine": engine, **extra}


VARIANTS = {
    "grain-scan-n1": [_w("denoise", "waifu2x-art-scan", level=1)],
    "grain-scan-n2": [_w("denoise", "waifu2x-art-scan", level=2)],
    "grain-scan-n3": [_w("denoise", "waifu2x-art-scan", level=3)],
    "grain-cunet-n2": [_w("denoise", "waifu2x-cunet-art", level=2)],
    "sharpen-scan-2x": [_w("sharpen", "waifu2x-art-scan", scale=2)],
    "sharpen-scan-2x-n1": [_w("sharpen", "waifu2x-art-scan", scale=2, level=1)],
    "sharpen-scan-4x-n1": [_w("sharpen", "waifu2x-art-scan", scale=4, level=1)],
    "sharpen-digimanga-4x": [_w("sharpen", "digimanga-bw-4x")],
    "jpeg-hqplus": [_w("jpeg", "mangajpeg-hqplus")],
    "jpeg-hq": [_w("jpeg", "mangajpeg-hq")],
    "jpeg-mq": [_w("jpeg", "mangajpeg-mq")],
    "jpeg-hq+sharpen-2x": [_w("jpeg", "mangajpeg-hq"), _w("sharpen", "waifu2x-art-scan", scale=2)],
}


# Round 2: community recipes (MangaJaNaiConverterGui defaults, Real-CUGAN native 3x,
# waifu2x art for clean raws, dejpeg before upscale). Every step ends at the source size.
VARIANTS_R2 = {
    "mangajanai-2x": [_w("sharpen", "mangajanai", scale=2)],
    "mangajanai-4x": [_w("sharpen", "mangajanai", scale=4)],
    "jpeg-hq+mangajanai-2x": [_w("jpeg", "mangajpeg-hq"), _w("sharpen", "mangajanai", scale=2)],
    "cugan-2x-conservative": [_w("sharpen", "realcugan", scale=2)],
    "cugan-3x-conservative": [_w("sharpen", "realcugan", scale=3)],
    "cugan-3x-denoise3": [_w("sharpen", "realcugan", scale=3, level=3)],
    "w2x-art-2x-n0": [_w("sharpen", "waifu2x-art", scale=2, level=0)],
    "w2x-scan-4x-n2": [_w("sharpen", "waifu2x-art-scan", scale=4, level=2)],
}
VARIANTS.update(VARIANTS_R2)


def _all_models():
    import mc_denoise as d
    steps = [step for spec in VARIANTS.values() for step in d.resolve_recipe({"schema": 1, "steps": spec})]
    return d.models_for(steps)


@app.function(image=image, volumes={ROOT: volume}, cpu=2.0, memory=4096, timeout=1800)
def seed() -> dict:
    import mc_denoise as d
    started = time.time()
    paths = d.ensure_models(ROOT, _all_models(), download=True, commit=volume.commit)
    return {"models": len(paths), "seconds": round(time.time() - started, 1)}


@app.cls(image=image, volumes={ROOT: volume}, gpu="L4", cpu=4.0, memory=16384, timeout=1800,
         scaledown_window=60, max_containers=4)
class Denoiser:
    @modal.enter()
    def load(self) -> None:
        import mc_denoise as d
        self.d = d
        self.sessions = d.SessionCache(d.ensure_models(ROOT, _all_models(), download=False))

    @modal.method()
    def page(self, name: str, png: bytes, variants: list) -> dict:
        results = {}
        for variant in variants:
            steps = self.d.resolve_recipe({"schema": 1, "steps": VARIANTS[variant]})
            started = time.perf_counter()
            out, info = self.d.denoise_page(self.sessions, steps, png)
            info["total_ms"] = round((time.perf_counter() - started) * 1000)
            results[variant] = (out, info)
        return {"name": name, "results": results}


@app.local_entrypoint()
def main(chapter: str, out: str, limit: int = 0, only: str = "", warm: bool = False) -> None:
    pages = sorted(Path(chapter).glob("*.png"))
    if limit:
        pages = pages[:limit]
    variants = [v for v in VARIANTS if not only or v in only.split(",")]
    print("seed:", seed.remote())
    out_dir = Path(out)
    timings = {}
    started = time.time()
    # --warm runs every variant twice per page and keeps the second timing, so the
    # numbers exclude model loading (what a user sees after the first page).
    args = [(p.name, p.read_bytes(), variants * 2 if warm else variants) for p in pages]
    for result in Denoiser().page.starmap(args, order_outputs=False):
        for variant, (png, info) in result["results"].items():
            target = out_dir / variant / result["name"]
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(png)
            timings.setdefault(variant, {})[result["name"]] = info
        print("done", result["name"], flush=True)
    (out_dir / "timings.json").write_text(json.dumps(
        {"wall_seconds": round(time.time() - started, 1), "variants": VARIANTS, "timings": timings}, indent=1))
    print(f"{len(pages)} pages x {len(variants)} variants in {time.time() - started:.0f} s")
