#!/usr/bin/env python3
"""Measure candidate-region agreement; SAM pixels are not ground truth."""

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw


def raster_polygon(size, points):
    canvas = Image.new("L", size)
    if len(points) >= 3:
        ImageDraw.Draw(canvas).polygon([(float(x), float(y)) for x, y in points], fill=1)
    return np.asarray(canvas, dtype=bool)


def box_iou(a, b):
    x0 = max(a[0], b[0])
    y0 = max(a[1], b[1])
    x1 = min(a[2], b[2])
    y1 = min(a[3], b[3])
    intersection = max(0, x1 - x0) * max(0, y1 - y0)
    area_a = max(0, a[2] - a[0]) * max(0, a[3] - a[1])
    area_b = max(0, b[2] - b[0]) * max(0, b[3] - b[1])
    union = area_a + area_b - intersection
    return intersection / union if union else 0.0


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rt-json", type=Path, required=True)
    parser.add_argument("--coo-dir", type=Path, required=True)
    parser.add_argument("--sam-dir", type=Path, required=True)
    parser.add_argument("--coo-min-score", type=float, default=0.1)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    rt = json.loads(args.rt_json.read_text())
    coo_summary_path = args.coo_dir / "summary.json"
    coo_summary = json.loads(coo_summary_path.read_text())
    manifest = json.loads((args.coo_dir / "manifest.json").read_text())
    if (coo_summary["checkpoint_sha256"] != manifest["checkpoint_sha256"]
            or coo_summary["checkpoint_revision"] != manifest["checkpoint_revision"]
            or coo_summary["upstream_source_revision"] != manifest["source_revision"]):
        parser.error("COO checkpoint/source identity differs between summary and manifest")
    rt_stems = [Path(page["file"]).stem for page in rt["pages"]]
    coo_stems = [Path(page["file"]).stem for page in coo_summary["pages"]]
    if len(rt_stems) != len(set(rt_stems)) or set(rt_stems) != set(coo_stems):
        parser.error("RT and COO page identities differ or repeat")
    coo_summary_pages = {Path(page["file"]).stem: page for page in coo_summary["pages"]}
    rt_source_dir = Path(rt["source"]).resolve(strict=True)
    pages = []
    for rt_page in rt["pages"]:
        stem = Path(rt_page["file"]).stem
        coo_path = args.coo_dir / stem / "detections.json"
        sam_path = args.sam_dir / f"{stem}.png"
        if not coo_path.is_file() or not sam_path.is_file():
            parser.error(f"missing COO or SAM output for page {stem}")
        coo = json.loads(coo_path.read_text())
        sam_page = json.loads((args.sam_dir.parent / f"page-{stem}.json").read_text())
        if (coo["sha256"] != coo_summary_pages[stem]["sha256"]
                or coo["detections"] != coo_summary_pages[stem]["detections"]):
            parser.error(f"COO summary and detection JSON differ for {stem}")
        for path in (coo_summary_path, coo_path):
            rel = str(path.relative_to(args.coo_dir))
            expected = manifest["files"].get(rel)
            if expected is None or path.stat().st_size != expected["bytes"] or sha256(path) != expected["sha256"]:
                parser.error(f"COO artifact manifest mismatch for {rel}")
        rt_source = rt_source_dir / rt_page["file"]
        if (coo["sha256"] != sam_page["source_sha256"]
                or coo["sha256"] != sha256(rt_source)
                or Path(sam_page["source"]).resolve(strict=True) != rt_source.resolve(strict=True)):
            parser.error(f"RT, COO, and SAM source image identities differ for {stem}")
        sam = np.asarray(Image.open(sam_path).convert("L")) > 0
        if sha256(sam_path) != sam_page["mask_sha256"] or int(np.count_nonzero(sam)) != sam_page["foreground_pixels"]:
            parser.error(f"SAM mask and page metadata differ for {stem}")
        height, width = sam.shape
        if (width, height) != (rt_page["width"], rt_page["height"]):
            parser.error(f"RT/SAM page dimensions differ for {stem}")
        if (width, height) != tuple(coo["original_size"]):
            parser.error(f"COO/SAM page dimensions differ for {stem}")
        rt_text = np.zeros_like(sam)
        text_boxes = []
        for box in rt_page["boxes"]:
            if box["class"] not in ("text_bubble", "text_free"):
                continue
            bounds = [box["x"], box["y"], box["x"] + box["width"], box["y"] + box["height"]]
            text_boxes.append(bounds)
            x0, y0, x1, y1 = bounds
            rt_text[max(0, y0):min(height, y1), max(0, x0):min(width, x1)] = True
        coo_union = np.zeros_like(sam)
        proposals = []
        for index, detection in enumerate(coo["detections"]):
            if detection["score"] < args.coo_min_score:
                continue
            proposal = raster_polygon((width, height), detection["polygon"])
            coo_union |= proposal
            bounds = detection["bounding_box"]
            proposals.append({
                "index": index,
                "score": detection["score"],
                "bounding_box": bounds,
                "sam_pixels_in_polygon": int(np.count_nonzero(sam & proposal)),
                "sam_pixels_in_polygon_outside_rt_text": int(np.count_nonzero(sam & proposal & ~rt_text)),
                "max_rt_text_box_iou": max((box_iou(bounds, box) for box in text_boxes), default=0.0),
            })
        sam_pixels = int(np.count_nonzero(sam))
        in_rt = int(np.count_nonzero(sam & rt_text))
        in_coo = int(np.count_nonzero(sam & coo_union))
        added = int(np.count_nonzero(sam & coo_union & ~rt_text))
        pages.append({
            "page": stem,
            "rt_text_boxes": len(text_boxes),
            "coo_proposals": len(proposals),
            "sam_pixels": sam_pixels,
            "sam_pixels_in_rt_text_boxes": in_rt,
            "sam_pixels_in_coo_polygons": in_coo,
            "sam_pixels_in_either_candidate": in_rt + added,
            "coo_added_sam_pixels_outside_rt": added,
            "coo_proposals_with_any_sam_pixel": sum(p["sam_pixels_in_polygon"] > 0 for p in proposals),
            "proposals": proposals,
        })
    totals = {key: sum(page[key] for page in pages) for key in (
        "rt_text_boxes", "coo_proposals", "sam_pixels", "sam_pixels_in_rt_text_boxes",
        "sam_pixels_in_coo_polygons", "sam_pixels_in_either_candidate",
        "coo_added_sam_pixels_outside_rt", "coo_proposals_with_any_sam_pixel")}
    totals["pages"] = len(pages)
    totals["pages_with_coo_added_sam_pixels"] = sum(page["coo_added_sam_pixels_outside_rt"] > 0 for page in pages)
    result = {
        "meaning": "candidate-region agreement with SAM pixel mask; none of the three is human ground truth",
        "rt_model_id": rt.get("model_id", rt.get("model")),
        "rt_model_sha256": rt.get("model_sha256"),
        "rt_layout": rt.get("layout"),
        "coo_checkpoint_sha256": coo_summary["checkpoint_sha256"],
        "coo_min_score": args.coo_min_score,
        "rt_result_sha256": sha256(args.rt_json),
        "coo_result_sha256": sha256(coo_summary_path),
        "coo_manifest_sha256": sha256(args.coo_dir / "manifest.json"),
        "totals": totals,
        "pages": pages,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(totals, indent=2))


if __name__ == "__main__":
    main()
