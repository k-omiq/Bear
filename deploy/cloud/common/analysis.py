"""Offline-capable, review-only ONNX analysis worker. Graphs are deployment seeded."""
from __future__ import annotations

import hashlib
import io
import time
from pathlib import Path
from typing import Any, Callable, Dict, List

import numpy as np
from PIL import Image

from deploy.cloud.common.contract import (
    ANALYSIS_RT, ANALYSIS_SAM, ANALYSIS_MAX_COMPONENTS, ANALYSIS_MAX_BOXES, ANALYSIS_VERSION,
    ContractValidationError, validate_analysis_request, validate_analysis_result,
)


class AnalysisWorker:
    def __init__(self, graphs: Dict[str, List[str]], revisions: Dict[str, str],
                 session_factory: Callable[[str], Any],
                 verified_digests: Dict[str, str] | None = None):
        self.graphs = graphs
        self.revisions = revisions
        self.session_factory = session_factory
        # A GPU runtime verifies graph files on load. Rehashing a 1.3 GB SAM
        # encoder for every tile would dominate analysis; standalone workers
        # retain the original per-request digest check when no cache is given.
        self.verified_digests = verified_digests or {}

    def capabilities(self) -> Dict[str, Any]:
        return {
            "protocol_version": ANALYSIS_VERSION,
            "capabilities": [
                {"capability": key, "graph_sha256s": [self.verified_digests.get(path) or self._sha(path) for path in paths],
                 "model_revision": self.revisions[key]}
                for key, paths in self.graphs.items() if key in (ANALYSIS_SAM, ANALYSIS_RT)
            ],
            "limits": {"max_tile_side": 1024, "max_tile_pixels": 1048576,
                       "max_png_bytes": 4194304, "max_components": 4096, "max_boxes": 4096},
        }

    @staticmethod
    def _sha(path: str) -> str:
        digest = hashlib.sha256()
        with Path(path).open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()

    def analyze(self, request: Dict[str, Any], tile_png: bytes) -> Dict[str, Any]:
        validate_analysis_request(request, tile_png)
        capability = request["capability"]
        paths = self.graphs.get(capability)
        if paths is None or len(paths) != (2 if capability == ANALYSIS_SAM else 1):
            raise ContractValidationError("analysis capability unavailable")
        if request["model_revision"] != self.revisions.get(capability):
            raise ContractValidationError("analysis model revision mismatch")
        started = time.monotonic()
        for path, expected in zip(paths, request["graph_sha256s"]):
            if (self.verified_digests.get(path) or self._sha(path)) != expected:
                raise ContractValidationError("analysis graph SHA-256 mismatch")
        sessions = [self.session_factory(path) for path in paths]
        loaded = time.monotonic()
        with Image.open(io.BytesIO(tile_png)) as image:
            image.load()
            rgb = image.convert("RGB")
        if capability == ANALYSIS_SAM:
            prepared, resized = sam_prepare(rgb)
        else:
            prepared = rt_prepare(rgb)
            resized = None
        preprocessed = time.monotonic()
        if capability == ANALYSIS_SAM:
            embedding = sessions[0].run(["embedding"], {"prepared_rgb": prepared})[0]
            logits = sessions[1].run(["high_res_logits"], {"embedding": embedding})[0]
            inferred = time.monotonic()
            mask = sam_restore(logits, rgb.size, resized)
            components = connected_components(mask)
            boxes = []
            output = io.BytesIO()
            Image.fromarray(mask).save(output, format="PNG")
            mask_png = output.getvalue()
        else:
            labels, raw_boxes, scores = sessions[0].run(
                ["labels", "boxes", "scores"],
                {"images": prepared, "orig_target_sizes": np.asarray([[rgb.width, rgb.height]], dtype=np.int64)},
            )
            inferred = time.monotonic()
            boxes = rt_boxes(labels, raw_boxes, scores, rgb.size)
            components = []
            mask_png = None
        finished = time.monotonic()
        ms = lambda first, last: min(2**32 - 1, max(0, round((last - first) * 1000)))
        result = {
            "protocol_version": ANALYSIS_VERSION, "capability": capability,
            "request_digest": request["request_digest"], "tile_id": request["tile_id"],
            "tile_rect": request["tile_rect"], "graph_sha256s": request["graph_sha256s"],
            "model_revision": request["model_revision"], "mask_png": mask_png,
            "components": components, "boxes": boxes,
            "timings": {"load_ms": ms(started, loaded), "preprocess_ms": ms(loaded, preprocessed),
                        "inference_ms": ms(preprocessed, inferred), "postprocess_ms": ms(inferred, finished)},
            "reported_cost_usd": None,
        }
        validate_analysis_result(result, request)
        return result


def sam_prepare(image: Image.Image) -> tuple[np.ndarray, tuple[int, int]]:
    width, height = image.size
    longest = max(width, height)
    resized = (max(1, round(width * 1024 / longest)), max(1, round(height * 1024 / longest)))
    canvas = Image.new("RGB", (1024, 1024), (128, 128, 128))
    canvas.paste(image.resize(resized, Image.Resampling.BILINEAR), (0, 0))
    return np.asarray(canvas, dtype=np.float32).transpose(2, 0, 1)[None].copy(), resized


def sam_restore(logits: Any, size: tuple[int, int], resized: tuple[int, int]) -> np.ndarray:
    values = np.asarray(logits, dtype=np.float32).squeeze()
    if values.shape != (1024, 1024) or not np.isfinite(values).all():
        raise ContractValidationError("invalid SAM logits")
    binary = (values[:resized[1], :resized[0]] > 0).astype(np.uint8) * 255
    return np.asarray(Image.fromarray(binary).resize(size, Image.Resampling.NEAREST))


def connected_components(mask: np.ndarray) -> List[Dict[str, int]]:
    from collections import deque
    height, width = mask.shape
    seen = np.zeros((height, width), dtype=np.bool_)
    result = []
    for y in range(height):
        for x in range(width):
            if mask[y, x] == 0 or seen[y, x]:
                continue
            if len(result) >= ANALYSIS_MAX_COMPONENTS:
                raise ContractValidationError("too many SAM components")
            queue = deque([(x, y)])
            seen[y, x] = True
            x1 = x2 = x
            y1 = y2 = y
            while queue:
                px, py = queue.popleft()
                x1, x2 = min(x1, px), max(x2, px)
                y1, y2 = min(y1, py), max(y2, py)
                for nx, ny in ((px - 1, py), (px + 1, py), (px, py - 1), (px, py + 1)):
                    if 0 <= nx < width and 0 <= ny < height and mask[ny, nx] and not seen[ny, nx]:
                        seen[ny, nx] = True
                        queue.append((nx, ny))
            result.append({"x": x1, "y": y1, "width": x2 - x1 + 1, "height": y2 - y1 + 1})
    return result


def rt_prepare(image: Image.Image) -> np.ndarray:
    resized = image.resize((640, 640), Image.Resampling.BILINEAR)
    return (np.asarray(resized, dtype=np.float32).transpose(2, 0, 1)[None] / 255.0).copy()


def rt_boxes(labels: Any, raw_boxes: Any, scores: Any, size: tuple[int, int]) -> List[Dict[str, Any]]:
    labels = np.asarray(labels).reshape(-1)
    raw_boxes = np.asarray(raw_boxes).reshape(-1, 4)
    scores = np.asarray(scores).reshape(-1)
    if len(labels) != len(raw_boxes) or len(labels) != len(scores) or len(labels) > ANALYSIS_MAX_BOXES:
        raise ContractValidationError("invalid RT output count")
    found = []
    for label, box, score in zip(labels, raw_boxes, scores):
        if not np.isfinite(score) or not np.isfinite(box).all():
            raise ContractValidationError("non-finite RT output")
        if score < 0.35 or int(label) not in (0, 1, 2):
            continue
        x1, y1, x2, y2 = [int(round(float(v))) for v in box]
        x1, x2 = max(0, min(size[0], x1)), max(0, min(size[0], x2))
        y1, y2 = max(0, min(size[1], y1)), max(0, min(size[1], y2))
        if x2 > x1 and y2 > y1:
            found.append({"rect": {"x": x1, "y": y1, "width": x2 - x1, "height": y2 - y1},
                          "class": int(label), "score": float(score)})
    return found
