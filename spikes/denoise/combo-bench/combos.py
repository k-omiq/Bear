"""Combos for the 2026-11-14 bench. Every combo ends at the page's own size.

A step is ("app", <recipe step dict>) for a model the app already pins, or
("x", <EXTRA key>) for a new community model run through spandrel.
"""

from __future__ import annotations

from pathlib import Path

# New community models: url, file name, scale, channels. Hashes are recorded by seed().
GH = "https://github.com/the-database/MangaJaNai/releases/download"
YOR = "https://bucket.yor.ovh/torch_models"
OMDB = "https://objectstorage.us-phoenix-1.oraclecloud.com/n/ax6ygfvpvzka/b/open-modeldb-files/o"
# archive: the zip / tar.xz the file lives in; member: exact name inside (None = list only).
EXTRA: dict = {
    # colour
    "ijanai-v3dn-2x": {"url": f"{GH}/3.0.0/IllustrationJaNai_V3denoise.zip", "archive": True,
                       "member": "2x_IllustrationJaNai_V3denoise_FDAT_M_unshuffle_30k_fp16.safetensors", "file": "ijanai-v3dn-2x.pth", "scale": 2, "license": "CC-BY-NC-SA-4.0"},
    "ijanai-v3dn-4x": {"url": f"{GH}/3.0.0/IllustrationJaNai_V3denoise.zip", "archive": True,
                       "member": "4x_IllustrationJaNai_V3denoise_FDAT_M_47k_fp16.safetensors", "file": "ijanai-v3dn-4x.pth", "scale": 4, "license": "CC-BY-NC-SA-4.0"},
    "ijanai-dejpeg-1x": {"url": f"{GH}/2.0.0/IllustrationJaNai_V2_DeJPEG_ModelsOnly.zip", "archive": True,
                         "member": "1x_IllustrationJaNai-DeJPEG_FDAT_M_100k.safetensors", "file": "ijanai-dejpeg-1x.pth", "scale": 1, "license": "CC-BY-NC-SA-4.0"},
    "animesharp-v4-2x": {"url": "https://github.com/Kim2091/Kim2091-Models/releases/download/2x-AnimeSharpV4/2x-AnimeSharpV4_RCAN.safetensors",
                         "file": "2x-AnimeSharpV4_RCAN.safetensors", "scale": 2, "license": "CC-BY-NC-SA-4.0"},
    "dejpg-realplksr-1x": {"url": "https://github.com/Phhofm/models/releases/download/1xDeJPG_realplksr_otf/1xDeJPG_realplksr_otf.safetensors",
                           "file": "1xDeJPG_realplksr_otf.safetensors", "scale": 1, "license": "CC-BY-4.0"},
    "fbcnn-color": {"url": "https://github.com/jiaxi-jiang/FBCNN/releases/download/v1.0/fbcnn_color.pth",
                    "file": "fbcnn_color.pth", "scale": 1, "license": "Apache-2.0"},
    "ultrasharp-v2-4x": {"url": "https://huggingface.co/Kim2091/UltraSharpV2/resolve/main/4x-UltraSharpV2.safetensors",
                         "file": "4x-UltraSharpV2.safetensors", "scale": 4, "license": "CC-BY-NC-SA-4.0"},
    # black and white
    "fbcnn-gray": {"url": "https://github.com/jiaxi-jiang/FBCNN/releases/download/v1.0/fbcnn_gray.pth",
                   "file": "fbcnn_gray.pth", "scale": 1, "channels": 1, "license": "Apache-2.0"},
    "eula-bw-v3-1x": {"url": f"{YOR}/1x_eula_digimanga_bw_v3_nc1_52k.tar.xz", "archive": True, "member": None,
                      "file": "eula-bw-v3-1x.pth", "scale": 1, "channels": 1},
    "ds-atdl3-4x": {"url": f"{YOR}/4x_dwtp_ds_atdl3.tar.xz", "archive": True, "member": None,
                    "file": "ds-atdl3-4x.pth", "scale": 4},
    "ds-moesr-v2-4x": {"url": f"{YOR}/4x_dwtp_ds_moesr_v2.tar.xz", "archive": True, "member": None,
                       "file": "ds-moesr-v2-4x.pth", "scale": 4},
    "ds-dwtp-esrgan5-4x": {"url": f"{OMDB}/4x-DWTP-ds-esrgan-5.pth", "file": "4x-DWTP-ds-esrgan-5.pth", "scale": 4},
}


def _a(op, engine, **kw):
    return ("app", {"op": op, "engine": engine, **kw})


def _x(key):
    return ("x", key)


COMBOS: dict = {
    # --- new, colour pages (also run on a few B&W pages for contrast)
    "col-ijanai-v3dn-2x": [_x("ijanai-v3dn-2x")],
    "col-ijanai-v3dn-4x": [_x("ijanai-v3dn-4x")],
    "col-ijanai-dejpeg-1x": [_x("ijanai-dejpeg-1x")],
    "col-ijanai-dejpeg>animesharp-v4": [_x("ijanai-dejpeg-1x"), _x("animesharp-v4-2x")],
    "col-animesharp-v4-2x": [_x("animesharp-v4-2x")],
    "col-dejpg-realplksr-1x": [_x("dejpg-realplksr-1x")],
    "col-fbcnn-color": [_x("fbcnn-color")],
    "col-ultrasharp-v2-4x": [_x("ultrasharp-v2-4x")],
    # --- new, black and white pages
    "bw-fbcnn-gray": [_x("fbcnn-gray")],
    "bw-fbcnn>mangajanai-2x": [_x("fbcnn-gray"), _a("sharpen", "mangajanai", scale=2)],
    "bw-eula-v3-1x": [_x("eula-bw-v3-1x")],
    "bw-eula-v3>mangajanai-2x": [_x("eula-bw-v3-1x"), _a("sharpen", "mangajanai", scale=2)],
    "bw-descreen-atdl3": [_x("ds-atdl3-4x")],
    "bw-descreen-moesr-v2": [_x("ds-moesr-v2-4x")],
    "bw-descreen-dwtp-esrgan5": [_x("ds-dwtp-esrgan5-4x")],
    # Baselines the app already ships (presets).
    "app-mangajanai-2x": [_a("sharpen", "mangajanai", scale=2)],
    "app-cugan-2x-cons": [_a("sharpen", "realcugan", scale=2)],
    "app-w2x-scan-4x-n2": [_a("sharpen", "waifu2x-art-scan", scale=4, level=2)],
    "app-jpeg-hq": [_a("jpeg", "mangajpeg-hq")],
    # Classic chains with app models only.
    "chain-jpeghq>mangajanai-2x": [_a("jpeg", "mangajpeg-hq"), _a("sharpen", "mangajanai", scale=2)],
    "chain-w2x-n1>mangajanai-2x": [_a("denoise", "waifu2x-art-scan", level=1), _a("sharpen", "mangajanai", scale=2)],
}


def app_steps():
    """Every app-model step any combo uses, resolved, for seeding."""
    import mc_denoise as d
    return [d.resolve_step(spec) for combo in COMBOS.values() for kind, spec in combo if kind == "app"]


def extra_path(extra_dir, spec) -> Path:
    """The model file, whichever weight format the archive held."""
    base = Path(extra_dir) / spec["file"]
    for suffix in (".pth", ".safetensors"):
        if base.with_suffix(suffix).exists():
            return base.with_suffix(suffix)
    return base


def register_extra(d, extra_dir: str) -> dict:
    """Add EXTRA models to the runtime's table under id x.<key>; returns {id: path}."""
    paths = {}
    for key, spec in EXTRA.items():
        path = extra_path(extra_dir, spec)
        if not path.exists():
            continue
        model = d.DenoiseModel(model_id=f"x.{key}", family="spandrel", file_name=spec["file"], url=spec["url"],
                               sha256="", bytes=path.stat().st_size, scale=spec["scale"],
                               channels=spec.get("channels", 3), license=spec.get("license", "?"),
                               author=spec.get("author", "?"))
        d.MODELS[model.model_id] = model
        paths[model.model_id] = path
    return paths


def steps_for(d, combo: str):
    steps = []
    for kind, spec in COMBOS[combo]:
        steps.append(d.resolve_step(spec) if kind == "app" else d.Step("x", spec, f"x.{spec}", 1.0))
    return steps


COLOUR_PAGES = {"c49_0.png", "c50_0.png", "c50_1-1.png", "c50_1-2+3.png"}
# A few B&W pages so colour models are also seen on grey art.
BW_SAMPLE = {"c49_1-5.png", "c50_1-8.png", "c50_1-12.png"}


def wanted(combo: str, page: str) -> bool:
    if combo.startswith("col-"):
        return page in COLOUR_PAGES or page in BW_SAMPLE
    if combo.startswith("bw-"):
        return page not in COLOUR_PAGES
    return True


# ---- 2026-11-14 run 2: the cleaned chapters. The user chose Real-CUGAN 2x conservative
# and MoESR v2 descreen for B&W; colour gets one model, picked on the joined spread.
# A name ending @2x keeps the page at twice its size (the done pages' size).
_BW = {"cugan": [_a("sharpen", "realcugan", scale=2)], "moesr-v2": [_x("ds-moesr-v2-4x")]}
_COL = {
    "cugan": [_a("sharpen", "realcugan", scale=2)],
    "ijanai-v3dn-2x": [_x("ijanai-v3dn-2x")],
    "ijanai-v3dn-4x": [_x("ijanai-v3dn-4x")],
    "animesharp-v4": [_x("animesharp-v4-2x")],
    "ijanai-dejpeg>animesharp-v4": [_x("ijanai-dejpeg-1x"), _x("animesharp-v4-2x")],
    "dejpg-realplksr": [_x("dejpg-realplksr-1x")],
}
COMBOS = {}
for _k, _v in _BW.items():
    COMBOS[f"r2bw-{_k}@1x"] = _v
    COMBOS[f"r2bw-{_k}@2x"] = _v
for _k, _v in _COL.items():
    COMBOS[f"r2col-{_k}@1x"] = _v
    COMBOS[f"r2col-{_k}@2x"] = _v

COLOUR_PAGES = {"c50_001.png", "c50_002+003.png"}


def wanted(combo: str, page: str) -> bool:
    if combo.startswith("r2col-"):
        return page in COLOUR_PAGES
    # B&W models also on 49/001 (B&W with a small pink logo), checked for colour after.
    return page not in COLOUR_PAGES


def steps_for(d, combo: str):
    from dataclasses import replace
    steps = [d.resolve_step(spec) if kind == "app" else d.Step("x", spec, f"x.{spec}", 1.0)
             for kind, spec in COMBOS[combo]]
    if combo.endswith("@2x"):
        last = max(i for i, s in enumerate(steps) if d.step_scale(s) > 1)
        steps[last] = replace(steps[last], out=2)
    return steps
del COMBOS["r2col-dejpg-realplksr@2x"]  # a 1x model has no 2x output
