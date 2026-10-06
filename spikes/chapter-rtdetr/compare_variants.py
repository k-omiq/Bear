#!/usr/bin/env python3
"""Compare pinned ogkalu ONNX checkpoints on whole webtoon pages and vertical halves."""

from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image

CLASS_NAMES = {0: "bubble", 1: "text_bubble", 2: "text_free"}
SCORE_THRESHOLD = 0.35
INPUT_SIZE = (640, 640)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def prepare(image: Image.Image) -> np.ndarray:
    # preprocessor_config.json: 640 square, resample=2 (PIL bilinear),
    # rescale=1/255, do_normalize=false, do_pad=false.
    rgb = image.convert("RGB").resize(INPUT_SIZE, Image.Resampling.BILINEAR)
    return np.asarray(rgb, dtype=np.float32).transpose(2, 0, 1)[None] / 255.0


def run_tile(session: ort.InferenceSession, tile: Image.Image, y_offset: int) -> tuple[list[dict], float]:
    tensor = prepare(tile)
    sizes = np.array([[tile.width, tile.height]], dtype=np.int64)
    started = time.perf_counter()
    labels, boxes, scores = session.run(None, {"images": tensor, "orig_target_sizes": sizes})
    elapsed_ms = (time.perf_counter() - started) * 1000
    found = []
    for label, box, score in zip(labels[0], boxes[0], scores[0]):
        if score < SCORE_THRESHOLD or int(label) not in CLASS_NAMES:
            continue
        x1, y1, x2, y2 = box
        x1 = int(np.clip(np.rint(x1), 0, tile.width))
        y1 = int(np.clip(np.rint(y1), 0, tile.height)) + y_offset
        x2 = int(np.clip(np.rint(x2), 0, tile.width))
        y2 = int(np.clip(np.rint(y2), 0, tile.height)) + y_offset
        if x2 <= x1 or y2 <= y1:
            continue
        found.append({"class": CLASS_NAMES[int(label)], "score": float(score),
                      "x": x1, "y": y1, "width": x2 - x1, "height": y2 - y1})
    return found, elapsed_ms


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--model-id", required=True)
    parser.add_argument("--layout", choices=("whole", "halves"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--threads", type=int, default=4)
    args = parser.parse_args()

    options = ort.SessionOptions()
    options.intra_op_num_threads = args.threads
    options.inter_op_num_threads = 1
    start = time.perf_counter()
    session = ort.InferenceSession(str(args.model), sess_options=options,
                                   providers=["CPUExecutionProvider"])
    load_ms = (time.perf_counter() - start) * 1000
    source = args.source.resolve(strict=True)
    pages = []
    for path in sorted(source.iterdir()):
        if path.suffix.lower() not in {".jpg", ".jpeg", ".png"}:
            continue
        with Image.open(path) as image:
            image.load()
            rgb = image.convert("RGB")
        if args.layout == "whole":
            tiles = [(rgb, 0)]
        else:
            mid = rgb.height // 2
            tiles = [(rgb.crop((0, 0, rgb.width, mid)), 0),
                     (rgb.crop((0, mid, rgb.width, rgb.height)), mid)]
        boxes = []
        times = []
        for tile, y_offset in tiles:
            detected, elapsed_ms = run_tile(session, tile, y_offset)
            boxes.extend(detected)
            times.append(elapsed_ms)
        boxes.sort(key=lambda box: (box["y"], box["x"], -box["score"]))
        pages.append({"file": path.name, "width": rgb.width, "height": rgb.height,
                      "tile_count": len(tiles), "inference_ms": sum(times),
                      "tile_inference_ms": times, "boxes": boxes})
        print(f"{args.model_id} {args.layout} {path.name}: {len(boxes)} boxes, {sum(times):.1f} ms", flush=True)
    result = {
        "model_id": args.model_id, "model": str(args.model.resolve(strict=True)),
        "model_sha256": sha256(args.model), "source": str(source),
        "layout": args.layout, "tile_policy": "whole page" if args.layout == "whole"
            else "two contiguous vertical halves, no overlap",
        "preprocessing": "PIL bilinear 640x640 RGB float32 /255; no normalization or padding",
        "orig_target_sizes_order": "width,height", "score_threshold": SCORE_THRESHOLD,
        "provider": "CPUExecutionProvider", "threads": args.threads,
        "session_load_ms": load_ms, "pages": pages,
        "summary": {"page_count": len(pages),
                    "box_count": sum(len(page["boxes"]) for page in pages),
                    "text_box_count": sum(sum(box["class"] != "bubble" for box in page["boxes"])
                                          for page in pages),
                    "empty_pages": sum(not page["boxes"] for page in pages),
                    "median_inference_ms_per_page": statistics.median(page["inference_ms"] for page in pages),
                    "total_inference_ms": sum(page["inference_ms"] for page in pages)}
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(f"wrote {args.output}")


if __name__ == "__main__":
    main()
