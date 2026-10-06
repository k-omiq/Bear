"""Page denoise: pinned models, recipe validation and the GPU runtime.

A page denoise run takes one whole page (PNG) and a recipe, an ordered list of 1 to
3 steps, and returns one PNG with the same width, height and colour kind. Steps:

- `denoise`: a 1x grain model (waifu2x `noise0..3`).
- `sharpen`: a 2x or 4x model, then a linear-light Lanczos downscale back to the
  input size. The model redraws edges at the higher resolution; the downscale keeps
  that sharpness and drops the size change, so a recipe never changes page geometry.
- `jpeg`: a 1x JPEG artefact model (MangaJPEG family, one luma channel).

Every step has a `strength` in [0, 1] that blends its output with its input.

waifu2x runs as ONNX through onnxruntime, the same files the desktop can run with
`ort`. The ESRGAN-family models on OpenModelDB ship only as PyTorch `.pth` files, so
they run through spandrel on the cloud worker only.

Weights: every file is pinned by URL and SHA-256. `ensure_models` downloads and
verifies into a directory on the weights volume; a GPU worker calls it with
`download=False` and only verifies. Nothing heavy (numpy, onnxruntime, torch, PIL,
spandrel) is imported at module load, so the gateway can validate recipes cheaply.

Licences: waifu2x (nunif) is MIT. The OpenModelDB models are CC-BY-NC-SA-4.0; the
project is non-commercial, and the attribution travels with `DenoiseModel.license`
and `DenoiseModel.author`.
"""

from __future__ import annotations

import hashlib
import io
import os
import tempfile
import threading
import time
import urllib.request
from contextlib import contextmanager, nullcontext
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, Iterator, List, Mapping, Optional, Sequence, Tuple

DENOISE_DIR_NAME = "denoise"
RECIPE_SCHEMA = 1
MAX_STEPS = 3
# One page, not a strip: the same pixel ceiling as a large scan at 600 dpi.
MAX_PAGE_PIXELS = 40_000_000
# A sharpen step holds its upscaled page as float32 twice (pixels and blend weights);
# this bounds that intermediate to about 2 GB per buffer on the worker.
MAX_UPSCALED_PIXELS = 160_000_000

W2X_REPO = "deepghs/waifu2x_onnx"
W2X_REVISION = "333b95cc88a6a9f39abb6426ab580f0d673f1185"
W2X_SNAPSHOT = "20250502/onnx_models"
OMDB_BUCKET = "https://objectstorage.us-phoenix-1.oraclecloud.com/n/ax6ygfvpvzka/b/open-modeldb-files/o"


class DenoiseError(ValueError):
    """Typed denoise failure; `error_code` is a stable snake_case code."""

    error_code = "denoise_invalid"

    def __init__(self, message: str, error_code: Optional[str] = None) -> None:
        super().__init__(message)
        if error_code:
            self.error_code = error_code


@dataclass(frozen=True)
class DenoiseModel:
    model_id: str
    family: str  # "waifu2x" (ONNX) or "spandrel" (.pth)
    file_name: str
    url: str
    sha256: str
    bytes: int
    scale: int
    # waifu2x only: output border the model consumes on each side, in output pixels.
    offset: int = 0
    # waifu2x only: "swin_unet" or "cunet"; decides the legal tile sizes.
    arch: str = ""
    # spandrel only: 1 for luma-only models, 3 for RGB. The loaded model's own
    # input_channels wins when spandrel reports it.
    channels: int = 3
    license: str = "MIT"
    author: str = "nagadomi (nunif)"
    # Set when `url` is a zip: the member to extract. `sha256` and `bytes` are the
    # member's, so the pin holds even though the release publishes no archive digest.
    archive_member: str = ""
    # MangaJaNai only: the page height the model was trained for.
    height: int = 0


def _w2x(path: str, size: int, sha256: str) -> DenoiseModel:
    arch, domain, name = path.split("/")
    stem = name[: -len(".onnx")]
    scale = 4 if stem.endswith("scale4x") else 2 if stem.endswith("scale2x") else 1
    per_scale = {"swin_unet": {1: 8, 2: 16, 4: 32}, "cunet": {1: 28, 2: 36}}
    return DenoiseModel(
        model_id=f"waifu2x.{arch}.{domain}.{stem}",
        family="waifu2x",
        file_name=f"waifu2x--{arch}--{domain}--{name}",
        url=f"https://huggingface.co/{W2X_REPO}/resolve/{W2X_REVISION}/{W2X_SNAPSHOT}/{path}",
        sha256=sha256,
        bytes=size,
        scale=scale,
        offset=per_scale[arch][scale],
        arch=arch,
    )


def _omdb(name: str, size: int, sha256: str, scale: int, channels: int, author: str) -> DenoiseModel:
    return DenoiseModel(
        model_id=f"omdb.{name}",
        family="spandrel",
        file_name=f"omdb--{name}.pth",
        url=f"{OMDB_BUCKET}/{name}.pth",
        sha256=sha256,
        bytes=size,
        scale=scale,
        channels=channels,
        license="CC-BY-NC-SA-4.0",
        author=author,
    )


_W2X_FILES: Tuple[Tuple[str, int, str], ...] = (
    ("swin_unet/art/scale2x.onnx", 16727049, "581a69575295f5e082531b1ce6974dc27977576601b1d15c4c89d69b48c6ebef"),
    ("swin_unet/art/noise0_scale2x.onnx", 16727049, "9af63e6436b54db0f6c62a8412fb9bb47bbfc9c456987a27fe723854805f19ef"),
    ("swin_unet/art/noise1_scale2x.onnx", 16727049, "d56d2317e721772544763ab7c4b2aa845951786ef3fda285a68e772a9e721d4e"),
    ("swin_unet/art/noise2_scale2x.onnx", 16727049, "623f6cfcecec48170d8d0d80610a8000311e9c219741801e45163b79c9e69e16"),
    ("swin_unet/art/noise3_scale2x.onnx", 16727049, "9982ff6f75ed7d0ada5ff8e007259e699cb40fb13986fc9858a74defda240cae"),
    ("cunet/art/noise0.onnx", 5173170, "ae24d1c22a5b8a35fe25e3998fbb1078c317872b6a2eadc58663b7685272896d"),
    ("cunet/art/noise1.onnx", 5173170, "2aa1f3661d3c231d83d28941a5c6d247c446bad6004186d56f0dff32e75e787c"),
    ("cunet/art/noise2.onnx", 5173170, "f725ac3f49a60775e99fa982d44ff5562585ae7ce8360804616fe5b179516363"),
    ("cunet/art/noise3.onnx", 5173170, "fc8d6b957ef101ef42ee8e2e00f17e78d569f5f8369e54dba76739aab675f94f"),
    ("cunet/art/noise0_scale2x.onnx", 5178591, "d87d5ee80b46e4c25e0236dab789eb57251117a75b81c6a4895b62c00fc5f2a2"),
    ("cunet/art/noise1_scale2x.onnx", 5178591, "8676098939edfb0fc7d7a40839ec650ba9412229f703a9eb04d8ed521938f824"),
    ("cunet/art/noise2_scale2x.onnx", 5178591, "599528077558405cc6dd59075a13c10691e7f2eeee3fa2acad7808bca6857d52"),
    ("cunet/art/noise3_scale2x.onnx", 5178591, "b5f75e37f0c04e316d01bf6a141de46e6fb88d8ae459d2fc347853f212ac5ef0"),
    ("cunet/art/scale2x.onnx", 5178591, "0966d74dd0739a20de358de88c8fa4eb6cb8c3489bb0e941da9751ad4dcdf495"),
    ("swin_unet/art_scan/noise0.onnx", 18905935, "fcd828e47626d8e11ceaeb0b76b1b2123165a8834fd944dae8d14666fbaff680"),
    ("swin_unet/art_scan/noise1.onnx", 18905935, "99ff79a7c0e3bcd3fa51582b761fb0db26ff293c747de94b14521ac01a6aeb71"),
    ("swin_unet/art_scan/noise2.onnx", 18905935, "a3070f2d4b901d1f8c0b4ae6eb1fa9a485afaeb41298f49720200c4b81ab2e75"),
    ("swin_unet/art_scan/noise3.onnx", 18905935, "538a7eb118ffa5f02ccbe3953758c36701473ceaf99ccc53eb6f3c93579547ba"),
    ("swin_unet/art_scan/noise0_scale2x.onnx", 18905935, "c0b78f6a169c9e496674d76ff66fdbff5d7e12adb26316637bddbc87348c9a8f"),
    ("swin_unet/art_scan/noise1_scale2x.onnx", 18905935, "dd62e340e2ac624249b3e1a726b46c61d08090f4e60c9ee212cd99c600a34d4a"),
    ("swin_unet/art_scan/noise2_scale2x.onnx", 18905935, "16bc23b871c459a506e1335a4595a841f7a4bd562a26fbab85d3371491035fa2"),
    ("swin_unet/art_scan/noise3_scale2x.onnx", 18905935, "d20c7e792b65af8896c81c2873a8ba1761b59e2ff88e5b1c9161ff010b900048"),
    ("swin_unet/art_scan/scale2x.onnx", 18905935, "a6d617dd2e6453131779542c31cb44d2b5e4463e2bdbcf1b32fd9e7b4001e4e1"),
    ("swin_unet/art_scan/noise0_scale4x.onnx", 18905524, "4c8db42b8f93237f18b56e71fc177f0d4ec145529dbe0dd144eee64a6f6c280a"),
    ("swin_unet/art_scan/noise1_scale4x.onnx", 18905524, "86f90962d220e0ff2918c86ec3f72a83af287e37cbc30f276d391119e8a80dae"),
    ("swin_unet/art_scan/noise2_scale4x.onnx", 18905524, "532424408a1fd293c6fbfd54b44cd9077b7a4da8452218fd4d8040c7c9c78747"),
    ("swin_unet/art_scan/noise3_scale4x.onnx", 18905524, "8dbda7c4c264bcc1e7bfe7079baf6efaae0a36fa64435942e0a03ecfde4ea55d"),
    ("swin_unet/art_scan/scale4x.onnx", 18905524, "9306bad420baf63aa811588057aea71e9ddde820dc6b65b341ddd7c65f21fff5"),
)

# The seam blending filter nunif ships as a helper graph (utils/), used for every
# waifu2x model: a per-tile weight map that fades tile borders into their neighbours.
SEAM_FILTER = DenoiseModel(
    model_id="waifu2x.utils.create_seam_blending_filter",
    family="helper",
    file_name="waifu2x--utils--create_seam_blending_filter.onnx",
    url=f"https://huggingface.co/{W2X_REPO}/resolve/{W2X_REVISION}/{W2X_SNAPSHOT}/utils/create_seam_blending_filter.onnx",
    sha256="7d825bc0bbba65493cd3e1809a1cd6db4999243d516154b7974522dce6826ad5",
    bytes=26530,
    scale=1,
)

MANGAJANAI_URL = "https://github.com/the-database/MangaJaNai/releases/download/1.0.0/MangaJaNai_V1_ModelsOnly.zip"
_MANGAJANAI_FILES: Tuple[Tuple[str, int, str], ...] = (
    ("2x_MangaJaNai_1200p_V1_ESRGAN_70k.pth", 33709234, "43b784f674bdbf89886a62a64cd5f8d8df92caf4d861bdf4d47dad249ede0267"),
    ("2x_MangaJaNai_1300p_V1_ESRGAN_75k.pth", 33708528, "15ca3c0f75f97f7bf52065bf7c9b8d602de94ce9e3b078ac58793855eed18589"),
    ("2x_MangaJaNai_1400p_V1_ESRGAN_70k.pth", 33708528, "a940ad8ebcf6bea5580f2f59df67deb009f054c9b87dbbc58c2e452722f34858"),
    ("2x_MangaJaNai_1500p_V1_ESRGAN_90k.pth", 33708528, "d91f2d247fa61144c1634a2ba46926acd3956ae90d281a5bed6655f8364a5b2c"),
    ("2x_MangaJaNai_1600p_V1_ESRGAN_90k.pth", 33708528, "6f5923f812dbc5d6aeed727635a21e74cacddce595afe6135cbd95078f6eee44"),
    ("2x_MangaJaNai_1920p_V1_ESRGAN_70k.pth", 33708528, "1ad4aa6f64684baa430da1bb472489bff2a02473b14859015884a3852339c005"),
    ("2x_MangaJaNai_2048p_V1_ESRGAN_95k.pth", 33708528, "146cd009b9589203a8444fe0aa7195709bb5b9fdeaca3808b7fbbd5538f94c41"),
    ("4x_MangaJaNai_1200p_V1_ESRGAN_70k.pth", 33630576, "6e3a8d21533b731eb3d8eaac1a09cf56290fa08faf8473cbe3debded9ab1ebe1"),
    ("4x_MangaJaNai_1300p_V1_ESRGAN_75k.pth", 33698866, "eacf8210543446f3573d4ea1625f6fc11a3b2a5e18b38978873944be146417a8"),
    ("4x_MangaJaNai_1400p_V1_ESRGAN_105k.pth", 33698866, "d77f977a6c6c4bf855dae55f0e9fad6ac2823fa8b2ef883b50e525369fde6a74"),
    ("4x_MangaJaNai_1500p_V1_ESRGAN_105k.pth", 33698866, "5e5174b60316e9abb7875e6d2db208fec4ffc34f3d09fa7f0e0f6476f9d31687"),
    ("4x_MangaJaNai_1600p_V1_ESRGAN_70k.pth", 33698160, "c126ec8d4b7434d8f6a43d24bec1f56d343104ab8a86b5e01d5d25be6b5244c0"),
    ("4x_MangaJaNai_1920p_V1_ESRGAN_105k.pth", 33698866, "d469e96e590a25a86037760b26d51405c77759a55b0966b15dc76b609f72f20b"),
    ("4x_MangaJaNai_2048p_V1_ESRGAN_70k.pth", 33698160, "f70e08c60da372b7207e7348486ea6b498ea8dea6246bb717530a4d45c955b9b"),
)


def _mangajanai(name: str, size: int, sha256: str) -> DenoiseModel:
    scale = int(name[0])
    height = int(name.split("_")[2].rstrip("p"))
    return DenoiseModel(
        model_id=f"mangajanai.{scale}x.{height}p", family="spandrel", file_name=f"mangajanai--{name}",
        url=MANGAJANAI_URL, sha256=sha256, bytes=size, scale=scale, channels=1,
        license="CC-BY-NC-4.0", author="the-database (MangaJaNai)", archive_member=name, height=height,
    )


# Real-CUGAN (bilibili, MIT). bilibili publishes the weights on Google Drive and Baidu
# only; this Hugging Face Space mirror is pinned to a revision and by file hash.
CUGAN_REVISION = "8ba6f5980ff4d3757339cbb8de007f03562616c1"
_CUGAN_FILES: Tuple[Tuple[str, int, str], ...] = (
    ("up2x-latest-conservative", 5147249, "6cfe3b23687915d08ba96010f25198d9cfe8a683aa4131f1acf7eaa58ee1de93"),
    ("up2x-latest-no-denoise", 5147249, "f491f9ecf6964ead9f3a36bf03e83527f32c6a341b683f7378ac6c1e2a5f0d16"),
    ("up2x-latest-denoise1x", 5147249, "2e783c39da6a6394fbc250fdd069c55eaedc43971c4f2405322f18949ce38573"),
    ("up2x-latest-denoise2x", 5147249, "8188b3faef4258cf748c59360cbc8086ebedf4a63eb9d5d6637d45f819d32496"),
    ("up2x-latest-denoise3x", 5147249, "0a14739f3f5fcbd74ec3ce2806d13a47916c916b20afe4a39d95f6df4ca6abd8"),
    ("up3x-latest-conservative", 5154161, "f6ea5fd20380413beb2701182483fd80c2e86f3b3f08053eb3df4975184aefe3"),
    ("up3x-latest-no-denoise", 5154161, "763f0a87e70d744673f1a41db5396d5f334d22de97fff68ffc40deb91404a584"),
    ("up3x-latest-denoise3x", 5154161, "39f1e6e90d50e5528a63f4ba1866bad23365a737cbea22a80769b2ec4c1c3285"),
    ("up4x-latest-conservative", 5636403, "a8c8185def699b0883662a02df0ef2e6db3b0275170b6cc0d28089b64b273427"),
    ("up4x-latest-no-denoise", 5636403, "aaf3ef78a488cce5d3842154925eb70ff8423b8298e2cd189ec66eb7f6f66fae"),
    ("up4x-latest-denoise3x", 5636403, "42bd8fcdae37c12c5b25ed59625266bfa65780071a8d38192d83756cb85e98dd"),
)


def _cugan(stem: str, size: int, sha256: str) -> DenoiseModel:
    return DenoiseModel(
        model_id=f"realcugan.{stem}", family="spandrel", file_name=f"realcugan--{stem}.pth",
        url=f"https://huggingface.co/spaces/yuan2023/Real-CUGAN/resolve/{CUGAN_REVISION}/weights_v3/{stem}.pth",
        sha256=sha256, bytes=size, scale=int(stem[2]), channels=3, license="MIT", author="bilibili (Real-CUGAN)",
    )


MODELS: Dict[str, DenoiseModel] = {
    model.model_id: model
    for model in (
        *(_w2x(path, size, sha) for path, size, sha in _W2X_FILES),
        _omdb("1x-MangaJPEGHQPlus", 20092251,
              "3126339603a3876f541d054bd40cdad417141a32c14e91fd6adaeaf04677be34", 1, 1, "bunzero"),
        _omdb("1x-MangaJPEGHQ", 20092251,
              "e2898bfba74083025a2bd2326b6c159031b882a890bd1cc30fd6910cda2ce4ee", 1, 1, "bunzero"),
        _omdb("1x-MangaJPEGMQ", 20092251,
              "42529335f43ac04f615af3442b2570d23dc64a206e2da4741bf5f835206354ae", 1, 1, "bunzero"),
        _omdb("1x-MangaJPEGLQ", 20092251,
              "af11ec91b6a38e0109cbb926b19e8a01975f947aacb3490f008dcab06e9b922d", 1, 1, "bunzero"),
        _omdb("4x-eula-digimanga-bw-v2-nc1", 67010885,
              "0c3ff9f7b4fc11b21e1262bca06efada0b0723436623db5e2af37fa2291cb750", 4, 1, "eula"),
        *(_mangajanai(name, size, sha) for name, size, sha in _MANGAJANAI_FILES),
        *(_cugan(stem, size, sha) for stem, size, sha in _CUGAN_FILES),
        SEAM_FILTER,
    )
}

# Engine name in a recipe step -> how that step picks its model.
W2X_ENGINES = {"waifu2x-art-scan": ("swin_unet", "art_scan"), "waifu2x-art": ("swin_unet", "art"),
               "waifu2x-cunet-art": ("cunet", "art")}
JPEG_ENGINES = {
    "mangajpeg-hqplus": "omdb.1x-MangaJPEGHQPlus",
    "mangajpeg-hq": "omdb.1x-MangaJPEGHQ",
    "mangajpeg-mq": "omdb.1x-MangaJPEGMQ",
    "mangajpeg-lq": "omdb.1x-MangaJPEGLQ",
}
SHARPEN_SPANDREL_ENGINES = {"digimanga-bw-4x": "omdb.4x-eula-digimanga-bw-v2-nc1"}
# MangaJaNai ships one model per source page height; the page picks it at run time,
# as MangaJaNaiConverterGui does. A step names the family, `mangajanai.<s>x.auto`.
MANGAJANAI_HEIGHTS = (1200, 1300, 1400, 1500, 1600, 1920, 2048)
# Real-CUGAN levels: no level = conservative, 0 = no denoise, 1-3 = denoise strength.
CUGAN_LEVELS = {None: "conservative", 0: "no-denoise", 1: "denoise1x", 2: "denoise2x", 3: "denoise3x"}
OPS = ("denoise", "sharpen", "jpeg")


# The recipes the app offers (user choice, 2026-09-28, after two comparison rounds in
# spikes/denoise). Only these presets' models are seeded on an installation; the full
# MODELS table stays for research runs. src/lib/model/denoise-presets.json holds the
# same recipes for the interface, and test_denoise checks the two agree.
PRESETS: Dict[str, Dict[str, Any]] = {
    "waifu2x-scan-4x-n2": {"schema": 1, "steps": [
        {"op": "sharpen", "engine": "waifu2x-art-scan", "scale": 4, "level": 2}]},
    "realcugan-2x-conservative": {"schema": 1, "steps": [
        {"op": "sharpen", "engine": "realcugan", "scale": 2}]},
    "realcugan-3x-conservative": {"schema": 1, "steps": [
        {"op": "sharpen", "engine": "realcugan", "scale": 3}]},
    "realcugan-3x-denoise3": {"schema": 1, "steps": [
        {"op": "sharpen", "engine": "realcugan", "scale": 3, "level": 3}]},
    "mangajanai-2x": {"schema": 1, "steps": [
        {"op": "sharpen", "engine": "mangajanai", "scale": 2}]},
    "mangajanai-4x": {"schema": 1, "steps": [
        {"op": "sharpen", "engine": "mangajanai", "scale": 4}]},
}


@dataclass(frozen=True)
class Step:
    op: str
    engine: str
    model_id: str
    strength: float


def _level(raw: Any) -> Optional[int]:
    if raw is None:
        return None
    if isinstance(raw, bool) or not isinstance(raw, int) or not 0 <= raw <= 3:
        raise DenoiseError("level must be an integer from 0 to 3")
    return raw


def resolve_step(raw: Mapping[str, Any]) -> Step:
    """Validate one recipe step and pick its pinned model."""
    if not isinstance(raw, Mapping):
        raise DenoiseError("each step must be an object")
    unknown = set(raw) - {"op", "engine", "level", "scale", "strength"}
    if unknown:
        raise DenoiseError(f"unknown step fields: {sorted(unknown)}")
    op, engine = raw.get("op"), raw.get("engine")
    if op not in OPS:
        raise DenoiseError(f"op must be one of {OPS}")
    strength = raw.get("strength", 1.0)
    if isinstance(strength, bool) or not isinstance(strength, (int, float)) or not 0.0 <= strength <= 1.0:
        raise DenoiseError("strength must be a number from 0 to 1")
    level = _level(raw.get("level"))
    scale = raw.get("scale")

    if op == "jpeg":
        if engine not in JPEG_ENGINES or level is not None or scale is not None:
            raise DenoiseError(f"jpeg steps take engine in {sorted(JPEG_ENGINES)} and no level or scale")
        return Step(op, engine, JPEG_ENGINES[engine], float(strength))

    if op == "sharpen" and engine == "mangajanai":
        scale = 2 if scale is None else scale
        if level is not None or scale not in (2, 4):
            raise DenoiseError("mangajanai takes scale 2 or 4 and no level")
        return Step(op, engine, f"mangajanai.{scale}x.auto", float(strength))

    if op == "sharpen" and engine == "realcugan":
        scale = 2 if scale is None else scale
        if scale not in (2, 3, 4):
            raise DenoiseError("realcugan scale must be 2, 3 or 4")
        model_id = f"realcugan.up{scale}x-latest-{CUGAN_LEVELS[level]}"
        if model_id not in MODELS:
            raise DenoiseError(f"realcugan has no {scale}x model at level {level}")
        return Step(op, engine, model_id, float(strength))

    if op == "sharpen" and engine in SHARPEN_SPANDREL_ENGINES:
        if level is not None or scale not in (None, 4):
            raise DenoiseError(f"{engine} is a fixed 4x model with no level")
        return Step(op, engine, SHARPEN_SPANDREL_ENGINES[engine], float(strength))

    if engine not in W2X_ENGINES:
        raise DenoiseError(f"engine {engine!r} is not available for {op}")
    arch, domain = W2X_ENGINES[engine]
    if op == "denoise":
        if level is None or scale is not None:
            raise DenoiseError("denoise steps take a level and no scale")
        stem = f"noise{level}"
    else:
        scale = 2 if scale is None else scale
        if scale not in (2, 4):
            raise DenoiseError("sharpen scale must be 2 or 4")
        stem = f"scale{scale}x" if level is None else f"noise{level}_scale{scale}x"
    model_id = f"waifu2x.{arch}.{domain}.{stem}"
    if model_id not in MODELS:
        raise DenoiseError(f"{engine} has no {stem} model")
    return Step(op, engine, model_id, float(strength))


def resolve_recipe(raw: Mapping[str, Any]) -> List[Step]:
    """Validate a recipe document: {"schema": 1, "steps": [...]}."""
    if not isinstance(raw, Mapping) or raw.get("schema") != RECIPE_SCHEMA:
        raise DenoiseError(f"recipe schema must be {RECIPE_SCHEMA}")
    steps = raw.get("steps")
    if not isinstance(steps, list) or not 1 <= len(steps) <= MAX_STEPS:
        raise DenoiseError(f"a recipe has 1 to {MAX_STEPS} steps")
    return [resolve_step(step) for step in steps]


def step_scale(step: Step) -> int:
    if step.model_id.endswith(".auto"):
        return int(step.model_id.split(".")[1][0])
    return MODELS[step.model_id].scale


def preset_models() -> List[DenoiseModel]:
    """Every model the shipped presets can use, which is what an installation seeds."""
    steps = [step for recipe in PRESETS.values() for step in resolve_recipe(recipe)]
    return models_for(steps)


def mangajanai_for(scale: int, page_height: int) -> DenoiseModel:
    """The MangaJaNai model trained for the height nearest this page's."""
    height = min(MANGAJANAI_HEIGHTS, key=lambda h: (abs(h - page_height), h))
    return next(m for m in MODELS.values() if m.model_id.startswith(f"mangajanai.{scale}x.{height}p"))


def models_for(steps: Iterable[Step]) -> List[DenoiseModel]:
    needed = set()
    for step in steps:
        if step.model_id.endswith(".auto"):
            prefix = step.model_id[: -len("auto")]
            needed.update(m for m in MODELS if m.startswith(prefix))
        else:
            needed.add(step.model_id)
    models = [MODELS[model_id] for model_id in sorted(needed)]
    if any(model.family == "waifu2x" for model in models):
        models.append(SEAM_FILTER)
    return models


# ---------------------------------------------------------------- weights


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def ensure_models(root: os.PathLike | str, models: Sequence[DenoiseModel], download: bool = True,
                  commit: Optional[Callable[[], Any]] = None) -> Dict[str, Path]:
    """Return model_id -> verified path. Downloads what is missing when `download`."""
    directory = Path(root) / DENOISE_DIR_NAME
    directory.mkdir(parents=True, exist_ok=True)
    paths: Dict[str, Path] = {}
    archives: Dict[str, Path] = {}
    changed = False
    try:
        changed = _ensure_each(models, directory, download, paths, archives)
    finally:
        for archive in archives.values():
            archive.unlink(missing_ok=True)
    if changed and commit is not None:
        commit()
    return paths


def _download(url: str, directory: Path) -> Path:
    with tempfile.NamedTemporaryFile(dir=directory, delete=False) as part:
        with urllib.request.urlopen(url, timeout=120) as response:
            for chunk in iter(lambda: response.read(1 << 20), b""):
                part.write(chunk)
    return Path(part.name)


def _ensure_each(models: Sequence[DenoiseModel], directory: Path, download: bool,
                 paths: Dict[str, Path], archives: Dict[str, Path]) -> bool:
    changed = False
    for model in models:
        target = directory / model.file_name
        if target.exists() and target.stat().st_size == model.bytes and _sha256(target) == model.sha256:
            paths[model.model_id] = target
            continue
        if not download:
            raise DenoiseError(f"{model.model_id} is not seeded", "weights_missing")
        if model.archive_member:
            if model.url not in archives:
                archives[model.url] = _download(model.url, directory)
            import zipfile
            with zipfile.ZipFile(archives[model.url]) as archive, \
                    tempfile.NamedTemporaryFile(dir=directory, delete=False) as part:
                with archive.open(model.archive_member) as member:
                    for chunk in iter(lambda: member.read(1 << 20), b""):
                        part.write(chunk)
            part_path = Path(part.name)
        else:
            part_path = _download(model.url, directory)
        actual = _sha256(part_path)
        if actual != model.sha256 or part_path.stat().st_size != model.bytes:
            part_path.unlink(missing_ok=True)
            raise DenoiseError(f"{model.model_id} failed verification (sha256 {actual})", "weights_corrupt")
        part_path.replace(target)
        paths[model.model_id] = target
        changed = True
    return changed


# ---------------------------------------------------------------- image helpers


def srgb_to_linear(x):
    import numpy as np
    return np.where(x <= 0.04045, x / 12.92, ((x + 0.055) / 1.055) ** 2.4)


def linear_to_srgb(x):
    import numpy as np
    x = np.clip(x, 0.0, 1.0)
    return np.where(x <= 0.0031308, x * 12.92, 1.055 * np.power(x, 1 / 2.4) - 0.055)


def downscale_linear_lanczos(chw, width: int, height: int):
    """CHW float in [0, 1] -> CHW at (height, width), Lanczos in linear light."""
    import numpy as np
    from PIL import Image

    linear = srgb_to_linear(chw.astype(np.float32))
    out = np.stack([
        np.asarray(Image.fromarray(channel.astype(np.float32)).resize(
            (width, height), Image.Resampling.LANCZOS), dtype=np.float32)
        for channel in linear
    ])
    return linear_to_srgb(out).astype(np.float32)


@dataclass
class Page:
    rgb: Any  # float32 CHW in [0, 1], 3 channels
    alpha: Any  # uint8 HxW or None when opaque
    gray: bool
    sixteen_bit: bool


def decode_page(png: bytes) -> Page:
    import numpy as np
    from PIL import Image

    image = Image.open(io.BytesIO(png))
    if image.width * image.height > MAX_PAGE_PIXELS:
        raise DenoiseError("page is too large", "page_too_large")
    if image.mode in ("1", "P", "CMYK"):
        # Mirrors the desktop rule: 1-bit, indexed and CMYK sources are declined.
        raise DenoiseError(f"{image.mode} pages are not denoised", "unsupported_mode")
    sixteen = image.mode in ("I;16", "I;16B", "I") or image.info.get("bits") == 16
    alpha = None
    if image.mode in ("LA", "RGBA"):
        a = np.asarray(image.getchannel("A"))
        alpha = None if a.min() == 255 else a
    if image.mode in ("I;16", "I;16B", "I"):
        luma = np.asarray(image, dtype=np.float32) / 65535.0
        rgb = np.stack([luma, luma, luma])
    else:
        rgb = np.asarray(image.convert("RGB"), dtype=np.float32).transpose(2, 0, 1) / 255.0
    gray = bool(np.abs(rgb[0] - rgb[1]).max() < 1.5 / 255 and np.abs(rgb[1] - rgb[2]).max() < 1.5 / 255)
    return Page(rgb=rgb, alpha=alpha, gray=gray, sixteen_bit=sixteen)


def encode_page(page: Page, rgb) -> bytes:
    import numpy as np
    from PIL import Image

    rgb = np.clip(rgb, 0.0, 1.0)
    if page.gray:
        luma = rgb.mean(axis=0)
        if page.sixteen_bit:
            image = Image.fromarray((luma * 65535.0 + 0.5).astype(np.uint16))
        else:
            image = Image.fromarray((luma * 255.0 + 0.5).astype(np.uint8))
            if page.alpha is not None:
                image.putalpha(Image.fromarray(page.alpha))
    else:
        image = Image.fromarray((rgb.transpose(1, 2, 0) * 255.0 + 0.5).astype(np.uint8))
        if page.alpha is not None:
            image.putalpha(Image.fromarray(page.alpha))
    out = io.BytesIO()
    image.save(out, format="PNG", compress_level=6)
    return out.getvalue()


# ---------------------------------------------------------------- waifu2x (ONNX)


BLEND_SIZE = 16


def _swin_tile(tile: int) -> int:
    while (tile - 16) % 12 or (tile - 16) % 16:
        tile += 1
    return tile


def _cunet_tile(tile: int, scale: int, offset: int) -> int:
    adj = 16 if scale == 1 else 32
    tile = ((tile * scale + offset * 2) - adj) // scale
    return tile - tile % 4


class Waifu2x:
    """Tiled waifu2x inference, ported from nunif's unlimited:waifu2x renderer (MIT)."""

    def __init__(self, sessions: "SessionCache", model: DenoiseModel, tile: int = 400) -> None:
        self.model = model
        self.session = sessions.get(model)
        self.filter_session = sessions.get(SEAM_FILTER)
        self.tile = _swin_tile(tile) if model.arch == "swin_unet" else _cunet_tile(tile, model.scale, model.offset)

    def _blend_filter(self):
        import numpy as np
        feeds = {name: np.array(value, dtype=np.int64)
                 for name, value in (("scale", self.model.scale), ("offset", self.model.offset),
                                     ("tile_size", self.tile))}
        return self.filter_session.run(None, feeds)[0].astype(np.float32)

    def run(self, rgb):
        import numpy as np

        scale, offset, tile = self.model.scale, self.model.offset, self.tile
        _, x_h, x_w = rgb.shape
        in_offset = -(-offset // scale)
        in_blend = -(-BLEND_SIZE // scale)
        in_step = tile - (in_offset * 2 + in_blend)
        out_step = in_step * scale
        h_blocks = w_blocks = 0
        in_h = in_w = 0
        while in_h < x_h + in_offset * 2:
            in_h = h_blocks * in_step + tile
            h_blocks += 1
        while in_w < x_w + in_offset * 2:
            in_w = w_blocks * in_step + tile
            w_blocks += 1
        # Both art models use replication padding in nunif's renderer.
        x = np.pad(rgb, ((0, 0), (in_offset, in_h - (x_h + in_offset)), (in_offset, in_w - (x_w + in_offset))),
                   mode="edge").astype(np.float32)
        blend = self._blend_filter()
        _, f_h, f_w = blend.shape
        pixels = np.zeros((3, in_h * scale, in_w * scale), dtype=np.float32)
        weights = np.zeros_like(pixels)
        input_name = self.session.get_inputs()[0].name
        for h_i in range(h_blocks):
            for w_i in range(w_blocks):
                i, j = h_i * in_step, w_i * in_step
                tile_x = x[None, :, i:i + tile, j:j + tile]
                tile_y = self.session.run(None, {input_name: tile_x})[0][0]
                ii, jj = h_i * out_step, w_i * out_step
                region = (slice(None), slice(ii, ii + f_h), slice(jj, jj + f_w))
                old = weights[region]
                new = old + blend
                pixels[region] = (pixels[region] * (old / new) + tile_y * (1.0 - old / new))
                weights[region] = new
        return np.clip(pixels[:, : x_h * scale, : x_w * scale], 0.0, 1.0)


# ---------------------------------------------------------------- spandrel (.pth)


class Spandrel:
    """Tiled PyTorch inference for OpenModelDB models, loaded through spandrel."""

    TILE = 512
    OVERLAP = 32

    def __init__(self, sessions: "SessionCache", model: DenoiseModel) -> None:
        self.model = model
        self.net = sessions.get(model)

    def _infer(self, planes):
        import numpy as np
        import torch

        c, h, w = planes.shape
        scale = self.model.scale
        out = np.zeros((c, h * scale, w * scale), dtype=np.float32)
        weight = np.zeros((1, h * scale, w * scale), dtype=np.float32)
        step = self.TILE - self.OVERLAP
        ys = list(range(0, max(h - self.OVERLAP, 1), step))
        xs = list(range(0, max(w - self.OVERLAP, 1), step))
        ramp = np.minimum(np.arange(self.TILE * scale) + 1, self.OVERLAP * scale).astype(np.float32)
        ramp = np.minimum(ramp, ramp[::-1])
        device = next(self.net.model.parameters()).device
        dtype = next(self.net.model.parameters()).dtype
        for y in ys:
            for x in xs:
                y0, x0 = min(y, max(h - self.TILE, 0)), min(x, max(w - self.TILE, 0))
                tile = planes[:, y0:y0 + self.TILE, x0:x0 + self.TILE]
                with torch.inference_mode():
                    t = torch.from_numpy(np.ascontiguousarray(tile))[None].to(device=device, dtype=dtype)
                    y_t = self.net(t)[0].float().clamp_(0, 1).cpu().numpy()
                th, tw = y_t.shape[1], y_t.shape[2]
                wmap = (ramp[:th, None] * ramp[None, :tw])[None]
                oy, ox = y0 * scale, x0 * scale
                out[:, oy:oy + th, ox:ox + tw] += y_t * wmap
                weight[:, oy:oy + th, ox:ox + tw] += wmap
        return out / np.maximum(weight, 1e-8)

    def run(self, rgb, gray: bool):
        import numpy as np

        if getattr(self.net, "input_channels", self.model.channels) == 3:
            return self._infer(rgb)
        # Luma-only model. A grey page runs its one plane; a colour page runs its luma
        # (BT.601) and carries the chroma through unchanged, resized if the model scales.
        r, g, b = rgb
        luma = 0.299 * r + 0.587 * g + 0.114 * b
        y_out = self._infer(luma[None].astype(np.float32))[0]
        if gray:
            return np.stack([y_out, y_out, y_out])
        cb, cr = b - luma, r - luma
        if self.model.scale != 1:
            from PIL import Image
            size = (y_out.shape[1], y_out.shape[0])
            cb, cr = (np.asarray(Image.fromarray(p.astype(np.float32)).resize(
                size, Image.Resampling.BICUBIC), dtype=np.float32) for p in (cb, cr))
        r2, b2 = y_out + cr, y_out + cb
        g2 = (y_out - 0.299 * r2 - 0.114 * b2) / 0.587
        return np.clip(np.stack([r2, g2, b2]), 0.0, 1.0)


# ---------------------------------------------------------------- runtime


class SessionCache:
    """Loads each model once per container: ONNX sessions on CUDA, spandrel on the GPU.

    Pages run on several threads at once (AnalysisGPU is @modal.concurrent). Threads
    share a loaded model: InferenceSession.run is thread safe and an eval-mode spandrel
    net under inference_mode keeps no per-call state. Loads, lookups and eviction hold
    one lock, and a model a page is using is pinned (`pinned`), so eviction never drops
    it while it runs: dropping it would only free nothing and make the next page load
    a second copy beside it.
    """

    # Models kept loaded at once. Each ONNX CUDA session keeps its own memory arena and
    # a recipe uses at most three models plus the seam helper, so older ones are
    # dropped rather than letting a container that serves many recipes fill the GPU.
    MAX_LOADED = 4

    def __init__(self, paths: Mapping[str, Path], providers: Optional[Sequence[Any]] = None) -> None:
        self.paths = dict(paths)
        self.providers = list(providers or (
            ("CUDAExecutionProvider", {"arena_extend_strategy": "kSameAsRequested",
                                       "cudnn_conv_algo_search": "HEURISTIC"}),
            "CPUExecutionProvider",
        ))
        self._loaded: Dict[str, Any] = {}
        self._pins: Dict[str, int] = {}
        self._lock = threading.RLock()

    # Locks do not copy; a copy (a snapshot stand-in in the tests) gets fresh ones.
    def __getstate__(self) -> Dict[str, Any]:
        return {key: value for key, value in self.__dict__.items() if key != "_lock"}

    def __setstate__(self, state: Dict[str, Any]) -> None:
        self.__dict__.update(state)
        self._lock = threading.RLock()

    @contextmanager
    def pinned(self, *models: DenoiseModel) -> Iterator[List[Any]]:
        """Load `models` and keep them loaded until the block ends."""
        with self._lock:
            loaded = [self.get(model) for model in models]
            for model in models:
                self._pins[model.model_id] = self._pins.get(model.model_id, 0) + 1
        try:
            yield loaded
        finally:
            with self._lock:
                for model in models:
                    left = self._pins.get(model.model_id, 0) - 1
                    if left > 0:
                        self._pins[model.model_id] = left
                    else:
                        self._pins.pop(model.model_id, None)

    def _evict(self) -> None:
        # Least recently used first, never a pinned one. With every loaded model
        # pinned the cache holds one more than MAX_LOADED until a page ends.
        while len(self._loaded) >= self.MAX_LOADED:
            oldest = next((key for key in self._loaded if not self._pins.get(key)), None)
            if oldest is None:
                break
            del self._loaded[oldest]
        try:
            import torch
            if torch.cuda.is_available():
                torch.cuda.empty_cache()
        except ImportError:
            pass

    def get(self, model: DenoiseModel):
        with self._lock:
            return self._get_locked(model)

    def _get_locked(self, model: DenoiseModel):
        if model.model_id in self._loaded:
            # Most recently used last, so eviction drops the least recently used.
            self._loaded[model.model_id] = self._loaded.pop(model.model_id)
        else:
            self._evict()
            path = str(self.paths[model.model_id])
            if model.family in ("waifu2x", "helper"):
                # torch first: it loads the CUDA and cuDNN libraries from its wheels,
                # which the ONNX Runtime CUDA provider then finds in the process. The
                # analysis runtime does the same (analysis_runtime.py).
                import torch  # noqa: F401
                import onnxruntime as ort
                providers = self.providers if model.family == "waifu2x" else ["CPUExecutionProvider"]
                options = ort.SessionOptions()
                # swin_unet's ScatterND warns once per node on every run; errors still show.
                options.log_severity_level = 3
                self._loaded[model.model_id] = ort.InferenceSession(path, options, providers=providers)
            else:
                import torch
                from spandrel import ModelLoader
                net = ModelLoader().load_from_file(path).eval()
                if torch.cuda.is_available():
                    net = net.cuda()
                    if net.supports_half:
                        net = net.half()
                self._loaded[model.model_id] = net
        return self._loaded[model.model_id]


def _pinned(sessions: Any, *models: DenoiseModel):
    """`sessions.pinned(...)`, or nothing for a stand-in cache without pins."""
    pinned = getattr(sessions, "pinned", None)
    return pinned(*models) if pinned is not None else nullcontext()


def run_step(sessions: SessionCache, step: Step, page: Page, rgb):
    import numpy as np

    _, h, w = rgb.shape
    model = mangajanai_for(int(step.model_id.split(".")[1][0]), h) if step.model_id.endswith(".auto") \
        else MODELS[step.model_id]
    if model.family == "waifu2x":
        # nunif's own renderer defaults to small tiles; these keep one tile of the
        # 4x swin model, the largest, well inside an L4's memory.
        tile = 256 if model.scale == 4 or model.arch == "cunet" else 400
        with _pinned(sessions, model, SEAM_FILTER):
            out = Waifu2x(sessions, model, tile=tile).run(rgb)
        if page.gray:
            out = np.repeat(out.mean(axis=0, keepdims=True), 3, axis=0)
    else:
        with _pinned(sessions, model):
            out = Spandrel(sessions, model).run(rgb, page.gray)
    if model.scale != 1:
        out = downscale_linear_lanczos(out, w, h)
    if step.strength < 1.0:
        out = rgb * (1.0 - step.strength) + out * step.strength
    return out.astype(np.float32), model.model_id


def denoise_page(sessions: SessionCache, steps: Sequence[Step], png: bytes) -> Tuple[bytes, Dict[str, Any]]:
    """Run `steps` over one PNG page; returns the PNG and per-step timings in ms."""
    page = decode_page(png)
    rgb = page.rgb
    pixels = rgb.shape[1] * rgb.shape[2]
    if any(pixels * step_scale(step) ** 2 > MAX_UPSCALED_PIXELS for step in steps):
        raise DenoiseError("page is too large for this sharpen scale", "page_too_large")
    timings = []
    for step in steps:
        started = time.perf_counter()
        rgb, model_id = run_step(sessions, step, page, rgb)
        # The model that actually ran: an `.auto` step names the one this page picked.
        timings.append({"model_id": model_id, "ms": round((time.perf_counter() - started) * 1000)})
    return encode_page(page, rgb), {
        "width": int(page.rgb.shape[2]),
        "height": int(page.rgb.shape[1]),
        "gray": page.gray,
        "steps": timings,
    }


# ---------------------------------------------------------------- wire protocol

DENOISE_VERSION = "1.0.0"
DENOISE_MAX_PNG_BYTES = 16 * 1024 * 1024
# Base64 of the largest page plus the metadata, and the same for the answer.
DENOISE_MAX_BODY_BYTES = 24_000_000
DENOISE_MAX_RESPONSE_BYTES = 24_000_000
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def canonical_recipe(recipe: Mapping[str, Any]) -> bytes:
    import json
    return json.dumps(recipe, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")


def request_digest(recipe: Mapping[str, Any], page_png: bytes) -> str:
    """SHA-256 over the canonical recipe and the page bytes; the client computes the same."""
    digest = hashlib.sha256(b"mc-denoise-v1\0")
    digest.update(canonical_recipe(recipe))
    digest.update(b"\0")
    digest.update(page_png)
    return digest.hexdigest()


def validate_denoise_request(metadata: Any, page_png: bytes) -> List[Step]:
    """Check one page request; returns its resolved steps or raises DenoiseError."""
    if type(metadata) is not dict or set(metadata) != {"protocol_version", "request_digest", "recipe"}:
        raise DenoiseError("metadata must hold exactly protocol_version, request_digest and recipe")
    if metadata["protocol_version"] != DENOISE_VERSION:
        raise DenoiseError(f"protocol_version must be {DENOISE_VERSION}")
    if not isinstance(page_png, bytes) or not page_png.startswith(PNG_SIGNATURE):
        raise DenoiseError("page must be a PNG")
    if len(page_png) > DENOISE_MAX_PNG_BYTES:
        raise DenoiseError("page PNG is too large", "page_too_large")
    steps = resolve_recipe(metadata["recipe"])
    if metadata["request_digest"] != request_digest(metadata["recipe"], page_png):
        raise DenoiseError("request_digest does not match the recipe and page")
    return steps


def seeded_models(root: os.PathLike | str) -> List[str]:
    """Model ids whose file is present at its pinned size (the hash is checked on load)."""
    directory = Path(root) / DENOISE_DIR_NAME
    return [model.model_id for model in MODELS.values()
            if (directory / model.file_name).is_file()
            and (directory / model.file_name).stat().st_size == model.bytes]


def denoise_capabilities(root: os.PathLike | str) -> Dict[str, Any]:
    seeded = set(seeded_models(root))
    engines: Dict[str, List[str]] = {op: [] for op in OPS}
    for engine in W2X_ENGINES:
        for op in ("denoise", "sharpen"):
            probe = {"op": op, "engine": engine, **({"level": 1} if op == "denoise" else {"scale": 2})}
            try:
                if resolve_step(probe).model_id in seeded:
                    engines[op].append(engine)
            except DenoiseError:
                pass  # this engine has no model for this op (waifu2x-art: no 1x models)
    if any(m.startswith("mangajanai.2x.") for m in seeded):
        engines["sharpen"].append("mangajanai")
    if "realcugan.up2x-latest-conservative" in seeded:
        engines["sharpen"].append("realcugan")
    engines["sharpen"] += [e for e, m in SHARPEN_SPANDREL_ENGINES.items() if m in seeded]
    engines["jpeg"] += [e for e, m in JPEG_ENGINES.items() if m in seeded]
    presets = [name for name, recipe in PRESETS.items()
               if all(m.model_id in seeded for m in models_for(resolve_recipe(recipe)))]
    return {
        "protocol_version": DENOISE_VERSION,
        "available": bool(presets) or (SEAM_FILTER.model_id in seeded and any(engines.values())),
        "presets": presets,
        "engines": engines,
        "models": [{"model_id": m.model_id, "license": m.license, "author": m.author,
                    "seeded": m.model_id in seeded} for m in MODELS.values() if m.family != "helper"],
    }


class DenoiseRuntime:
    """Per-container denoise state: verified model paths and loaded sessions.

    Models are verified (SHA-256) the first time a recipe needs them and never
    downloaded here. waifu2x must run on CUDA: a CPU fallback would be many times
    slower and bill GPU time for it, so it is refused, as the analysis runtime does.
    """

    def __init__(self, root: os.PathLike | str, reload: Optional[Callable[[], Any]] = None,
                 require_cuda: bool = True) -> None:
        self.root = root
        self.reload = reload
        self.require_cuda = require_cuda
        self.sessions = SessionCache({})
        # Pages run on several threads; verifying and adding model paths is locked.
        self._prepare_lock = threading.Lock()

    def __getstate__(self) -> Dict[str, Any]:
        return {key: value for key, value in self.__dict__.items() if key != "_prepare_lock"}

    def __setstate__(self, state: Dict[str, Any]) -> None:
        self.__dict__.update(state)
        self._prepare_lock = threading.Lock()

    def _prepare(self, steps: Sequence[Step]) -> None:
        with self._prepare_lock:
            self._prepare_locked(steps)

    def _prepare_locked(self, steps: Sequence[Step]) -> None:
        missing = [m for m in models_for(steps) if m.model_id not in self.sessions.paths]
        if not missing:
            return
        if self.reload is not None:
            self.reload()
        self.sessions.paths.update(ensure_models(self.root, missing, download=False))
        if self.require_cuda and any(m.family == "waifu2x" for m in missing):
            import torch  # noqa: F401
            import onnxruntime as ort
            if "CUDAExecutionProvider" not in ort.get_available_providers():
                raise DenoiseError("ONNX Runtime CUDA provider is unavailable", "inference_failed")

    def denoise(self, metadata: Dict[str, Any], page_png: bytes) -> Dict[str, Any]:
        steps = validate_denoise_request(metadata, page_png)
        self._prepare(steps)
        out, info = denoise_page(self.sessions, steps, page_png)
        return {"protocol_version": DENOISE_VERSION, "request_digest": metadata["request_digest"],
                "page_png": out, "info": info}
