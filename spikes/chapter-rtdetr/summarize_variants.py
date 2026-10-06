#!/usr/bin/env python3
"""Summarize four RT-DETR runs and their geometric overlap with saved SAM masks."""

import json
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
MASKS = HERE.parent / "sam-ts-l/artifacts/examples/chapter-109-torch-mps/masks"
RAW = HERE / "artifacts"
OUTPUT = HERE / "results/chapter-109-variants.json"
RUNS = {
    "small_whole": RAW / "chapter-109-v4s-int8-whole.json",
    "small_halves": RAW / "chapter-109-v4s-int8-halves.json",
    "full_whole": RAW / "chapter-109-fp32-whole.json",
    "full_halves": RAW / "chapter-109-fp32-halves.json",
}
OLD = HERE.parent / "sam-ts-l/artifacts/chapter-109-rtdetr.json"


def summary(report):
    pages = []
    inside_total = 0
    mask_total = 0
    text_empty_mask_nonempty = 0
    class_totals = {name: 0 for name in ("bubble", "text_bubble", "text_free")}
    for page in report["pages"]:
        mask = np.asarray(Image.open(MASKS / f"{Path(page['file']).stem}.png").convert("L")) > 0
        union = np.zeros(mask.shape, dtype=bool)
        classes = {name: 0 for name in class_totals}
        for box in page["boxes"]:
            classes[box["class"]] += 1
            class_totals[box["class"]] += 1
            if box["class"] in ("text_bubble", "text_free"):
                x, y = box["x"], box["y"]
                union[y:y + box["height"], x:x + box["width"]] = True
        positive = int(mask.sum())
        inside = int(np.count_nonzero(mask & union))
        mask_total += positive
        inside_total += inside
        if positive and not classes["text_bubble"] and not classes["text_free"]:
            text_empty_mask_nonempty += 1
        pages.append({"file": page["file"], "classes": classes,
                      "sam_pixels": positive, "sam_pixels_in_text_boxes": inside,
                      "inference_ms": page.get("inference_ms", page.get("detect_ms"))})
    return {
        "model": report["model"], "model_sha256": report.get("model_sha256"),
        "layout": report.get("layout", "whole"),
        "preprocessing": report.get("preprocessing", "app nearest-neighbor RGB float32 /255"),
        "class_totals": class_totals,
        "empty_pages": sum(sum(page["classes"].values()) == 0 for page in pages),
        "text_empty_pages": sum(page["classes"]["text_bubble"] + page["classes"]["text_free"] == 0
                                for page in pages),
        "all_empty_sam_nonempty_pages": sum(sum(page["classes"].values()) == 0
                                            and page["sam_pixels"] > 0 for page in pages),
        "text_empty_sam_nonempty_pages": text_empty_mask_nonempty,
        "sam_pixels_in_text_boxes": inside_total, "sam_pixels": mask_total,
        "sam_pixel_share_in_text_boxes": inside_total / mask_total,
        "inference_ms_total": sum(page["inference_ms"] for page in pages),
        "pages": pages,
    }


def main():
    runs = {key: summary(json.loads(path.read_text())) for key, path in RUNS.items()}
    runs["app_small_whole_nearest"] = summary(json.loads(OLD.read_text()))
    result = {
        "metric_meaning": "box/mask geometric agreement only; neither model is human ground truth",
        "chapter": "Apocalypse 109", "source_pages": 45,
        "huggingface_revision": "16e8a622f91fabc6b5b65c96d32d1183f8843546",
        "score_threshold": 0.35, "runs": runs,
    }
    OUTPUT.parent.mkdir(exist_ok=True)
    OUTPUT.write_text(json.dumps(result, indent=2) + "\n")
    for key, run in runs.items():
        print(key, run["class_totals"], "empty", run["empty_pages"],
              "SAM share", f"{run['sam_pixel_share_in_text_boxes']:.1%}")
    print("wrote", OUTPUT)


if __name__ == "__main__":
    main()
