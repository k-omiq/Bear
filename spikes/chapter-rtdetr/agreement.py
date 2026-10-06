#!/usr/bin/env python3
"""Compare RT-DETR box geometry with SAM mask pixels; neither is ground truth."""

import argparse
import hashlib
import json
import statistics
from collections import Counter
from pathlib import Path

import numpy as np
from PIL import Image


def inside_box(shape, box, padding):
    height, width = shape
    x0 = max(0, box["x"] - padding)
    y0 = max(0, box["y"] - padding)
    x1 = min(width, box["x"] + box["width"] + padding)
    y1 = min(height, box["y"] + box["height"] + padding)
    return x0, y0, x1, y1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--boxes", type=Path, required=True)
    parser.add_argument("--masks", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = json.loads(args.boxes.read_text())
    output_pages = []
    for page in report["pages"]:
        name = Path(page["file"]).stem
        mask_path = args.masks / f"{name}.png"
        if not mask_path.is_file():
            parser.error(f"missing SAM mask {mask_path}")
        mask = np.asarray(Image.open(mask_path).convert("L")) > 0
        if mask.shape != (page["height"], page["width"]):
            parser.error(f"mask dimensions disagree on {name}")
        text_boxes = [box for box in page["boxes"] if box["class"] in ("text_bubble", "text_free")]
        bubble_boxes = [box for box in page["boxes"] if box["class"] == "bubble"]
        page_result = {
            "page": name,
            "sam_positive_pixels": int(mask.sum()),
            "text_box_count": len(text_boxes),
            "bubble_box_count": len(bubble_boxes),
            "text_boxes_with_any_sam_pixel": sum(bool(mask[y0:y1, x0:x1].any())
                for x0, y0, x1, y1 in (inside_box(mask.shape, box, 0) for box in text_boxes)),
            "agreements_by_box_padding_pixels": {},
        }
        for padding in (0, 2, 5):
            class_counts = {}
            for name_and_boxes, boxes in (("text", text_boxes), ("bubble", bubble_boxes),
                                          ("all", page["boxes"])):
                union = np.zeros(mask.shape, dtype=bool)
                for box in boxes:
                    x0, y0, x1, y1 = inside_box(mask.shape, box, padding)
                    union[y0:y1, x0:x1] = True
                class_counts[name_and_boxes] = {
                    "sam_pixels_inside_boxes": int(np.count_nonzero(mask & union)),
                    "sam_pixels_outside_boxes": int(np.count_nonzero(mask & ~union)),
                    "box_union_pixels": int(union.sum()),
                }
            page_result["agreements_by_box_padding_pixels"][str(padding)] = class_counts
        output_pages.append(page_result)

    totals = Counter()
    for page in output_pages:
        totals["sam_positive_pixels"] += page["sam_positive_pixels"]
        totals["text_boxes"] += page["text_box_count"]
        totals["bubble_boxes"] += page["bubble_box_count"]
        totals["text_boxes_with_any_sam_pixel"] += page["text_boxes_with_any_sam_pixel"]
    totals["pages"] = len(output_pages)
    totals["sam_nonempty_pages"] = sum(p["sam_positive_pixels"] > 0 for p in output_pages)
    totals["sam_empty_pages"] = sum(p["sam_positive_pixels"] == 0 for p in output_pages)
    totals["rtdetr_text_empty_pages"] = sum(p["text_box_count"] == 0 for p in output_pages)
    totals["rtdetr_all_empty_pages"] = sum(p["text_box_count"] + p["bubble_box_count"] == 0 for p in output_pages)
    totals["sam_nonempty_rtdetr_text_empty_pages"] = sum(
        p["sam_positive_pixels"] > 0 and p["text_box_count"] == 0 for p in output_pages)
    totals["sam_empty_rtdetr_text_nonempty_pages"] = sum(
        p["sam_positive_pixels"] == 0 and p["text_box_count"] > 0 for p in output_pages)
    total_agreement = {}
    for padding in (0, 2, 5):
        group = {}
        for kind in ("text", "bubble", "all"):
            inside = sum(p["agreements_by_box_padding_pixels"][str(padding)][kind]["sam_pixels_inside_boxes"]
                         for p in output_pages)
            group[kind] = {
                "sam_pixels_inside_boxes": inside,
                "sam_pixels_outside_boxes": totals["sam_positive_pixels"] - inside,
                "sam_pixel_share_inside_boxes": inside / totals["sam_positive_pixels"]
                    if totals["sam_positive_pixels"] else None,
            }
        total_agreement[str(padding)] = group

    performance = {
        "model_open_ms": report["model_open_ms"],
        "decode_ms_total": sum(p["decode_ms"] for p in report["pages"]),
        "detect_ms_total": sum(p["detect_ms"] for p in report["pages"]),
        "detect_ms_median": statistics.median(p["detect_ms"] for p in report["pages"]),
        "provider": report["provider"],
    }
    compact_pages = [{
        key: page[key] for key in ("page", "sam_positive_pixels", "text_box_count",
                              "bubble_box_count", "text_boxes_with_any_sam_pixel")
    } | {
        "sam_pixels_in_text_boxes": page["agreements_by_box_padding_pixels"]["0"]["text"]
            ["sam_pixels_inside_boxes"]
    } for page in output_pages]
    summary = {
        "metric_meaning": "SAM–RT-DETR model agreement only; no human truth or accuracy estimate",
        "rtdetr_box_file_sha256": hashlib.sha256(args.boxes.read_bytes()).hexdigest(),
        "mask_directory": str(args.masks.resolve()),
        "counts": dict(totals),
        "agreement_by_box_padding_pixels": total_agreement,
        "rtdetr_performance": performance,
        "pages": compact_pages,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({key: value for key, value in summary.items() if key != "pages"}, indent=2))


if __name__ == "__main__":
    main()
