"""Scratch probe for the app's denoise worker (2026-11-14): pins, seeding and a GPU smoke run.

Runs the real deploy/cloud/common/denoise.py in its own ephemeral app and its own volume, with the
worker image's pins (deploy/cloud/modal/app.py). Nothing deployed is touched. Delete the volume after.

    MODAL_PROFILE=yarafiq1971 modal run spikes/denoise/app-probe/probe_app.py::sizes
    MODAL_PROFILE=yarafiq1971 modal volume put mc-denoise-probe-1114 <moesr .pth> /denoise/umzi--4x_dwtp_ds_moesr_v2.pth
    MODAL_PROFILE=yarafiq1971 modal run spikes/denoise/app-probe/probe_app.py::seed
    MODAL_PROFILE=yarafiq1971 modal run spikes/denoise/app-probe/probe_app.py::smoke
    MODAL_PROFILE=yarafiq1971 modal volume delete mc-denoise-probe-1114
"""

from __future__ import annotations

import json
from pathlib import Path

import modal

VOLUME = "mc-denoise-probe-1114"
ROOT = "/weights"

image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch==2.13.0", "torchvision==0.28.0", index_url="https://download.pytorch.org/whl/cu129")
    .pip_install("onnxruntime-gpu==1.23.2", "numpy==2.4.6", "pillow==12.3.0", "scipy==1.18.1",
                 "spandrel==0.4.2", "spandrel_extra_arches==0.2.0")
)
if modal.is_local():
    image = image.add_local_file(Path(__file__).resolve().parents[3] / "deploy/cloud/common/denoise.py",
                                 "/root/mc_denoise.py")
volume = modal.Volume.from_name(VOLUME, create_if_missing=True)
app = modal.App("mc-denoise-probe-1114")


@app.function(image=image, volumes={ROOT: volume}, cpu=2.0, memory=4096, timeout=1200)
def sizes() -> dict:
    """Each IllustrationJaNai member's size and SHA-256, read from the release zip."""
    import hashlib
    import zipfile
    import mc_denoise as d

    out = {}
    directory = Path(ROOT) / "probe"
    directory.mkdir(parents=True, exist_ok=True)
    archive = d._download(d.IJANAI_URL, directory)
    try:
        with zipfile.ZipFile(archive) as z:
            for info in z.infolist():
                digest = hashlib.sha256(z.read(info.filename)).hexdigest()
                out[info.filename] = {"bytes": info.file_size, "sha256": digest}
    finally:
        archive.unlink(missing_ok=True)
    # The descreen model's host, from Modal: the doc says it refuses.
    try:
        d._download(d.MOESR.url, directory).unlink()
        out["moesr_host"] = "downloaded"
    except OSError as exc:
        out["moesr_host"] = f"refused: {exc}"[:200]
    print(json.dumps(out, indent=1))
    return out


@app.function(image=image, volumes={ROOT: volume}, cpu=2.0, memory=4096, timeout=1800)
def seed() -> dict:
    """The installation's seed path: every preset model, MoESR skipped when its host refuses."""
    import mc_denoise as d

    paths = d.ensure_models(ROOT, d.preset_models(), download=True, commit=volume.commit)
    caps = d.denoise_capabilities(ROOT)
    out = {"seeded": sorted(paths), "presets": caps["presets"], "features": caps["features"],
           "sharpen": caps["engines"]["sharpen"]}
    print(json.dumps(out, indent=1))
    return out


def _pages():
    """Three small synthetic pages: grey with grain, grey with a small pink logo, colour art."""
    import numpy as np
    rng = np.random.default_rng(5)
    h, w = 640, 480
    y, x = np.mgrid[0:h, 0:w]
    grey = (128 + 80 * np.sin(x / 9.0) * np.cos(y / 13.0) + rng.normal(0, 6, (h, w))).clip(0, 255).astype(np.uint8)
    logo = np.stack([grey] * 3, axis=-1).copy()
    logo[20:60, 30:150] = (240, 120, 170)  # about 1.6 % of the page
    colour = np.zeros((h, w, 3), dtype=np.uint8)
    colour[..., 0] = (120 + 100 * np.sin(x / 17.0)).clip(0, 255)
    colour[..., 1] = (90 + 60 * np.cos(y / 11.0)).clip(0, 255)
    colour[..., 2] = (160 + 70 * np.sin((x + y) / 23.0)).clip(0, 255)
    colour = (colour + rng.normal(0, 5, colour.shape)).clip(0, 255).astype(np.uint8)
    return {"grey": grey, "logo": logo, "colour": colour}


@app.function(image=image, volumes={ROOT: volume}, gpu=["L4", "A10G"], cpu=4.0, memory=16384, timeout=1200)
def smoke() -> dict:
    """Every preset at page size and 2x on the three pages, through DenoiseRuntime as the worker runs it."""
    import io
    import time
    import numpy as np
    from PIL import Image
    import mc_denoise as d

    def png(array):
        buf = io.BytesIO()
        Image.fromarray(array).save(buf, format="PNG")
        return buf.getvalue()

    def saturation(rgb):
        rgb = rgb.astype(np.float32)
        return float((rgb.max(axis=-1) - rgb.min(axis=-1)).mean())

    runtime = d.DenoiseRuntime(ROOT, reload=volume.reload)
    report = {"install_twice": [d.install_extra_arches(), d.install_extra_arches()], "runs": []}
    pages = {name: png(a) for name, a in _pages().items()}
    for preset, recipe in d.PRESETS.items():
        for out_scale in (1, 2):
            doc = dict(recipe) if out_scale == 1 else {**recipe, "output_scale": 2}
            for name, page in pages.items():
                meta = {"protocol_version": d.DENOISE_VERSION, "recipe": doc,
                        "request_digest": d.request_digest(doc, page)}
                started = time.perf_counter()
                try:
                    answer = runtime.denoise(meta, page)
                except Exception as exc:  # noqa: BLE001
                    report["runs"].append({"preset": preset, "scale": out_scale, "page": name, "error": repr(exc)[:300]})
                    continue
                image_out = Image.open(io.BytesIO(answer["page_png"]))
                arr = np.asarray(image_out.convert("RGB"))
                row = {"preset": preset, "scale": out_scale, "page": name, "mode": image_out.mode,
                       "size": image_out.size, "models": [s["model_id"] for s in answer["info"]["steps"]],
                       "s": round(time.perf_counter() - started, 2)}
                if name == "logo":
                    box = arr[20 * out_scale:60 * out_scale, 30 * out_scale:150 * out_scale]
                    row["logo_saturation"] = round(saturation(box), 1)
                if name == "colour":
                    row["saturation"] = round(saturation(arr), 1)
                report["runs"].append(row)
    report["source_saturation"] = {"logo": round(saturation(_pages()["logo"][20:60, 30:150]), 1),
                                   "colour": round(saturation(_pages()["colour"]), 1)}
    print(json.dumps(report, indent=1))
    return report
