#!/usr/bin/env python3
"""Render source-coordinate COO proposals for visual inspection only."""

import argparse
import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


def overlay(image_path, detections_path):
    image = Image.open(image_path).convert("RGB")
    page = json.loads(detections_path.read_text())
    draw = ImageDraw.Draw(image)
    for index, detection in enumerate(page["detections"], 1):
        polygon = [tuple(point) for point in detection["polygon"]]
        if len(polygon) >= 3:
            draw.line(polygon + polygon[:1], fill=(15, 230, 60), width=3)
        bounds = detection["bounding_box"]
        draw.rectangle(bounds, outline=(255, 120, 0), width=2)
        label = f"{index}:{detection['score']:.2f}"
        x, y = int(bounds[0]), max(0, int(bounds[1]) - 15)
        label_width = max(45, int(len(label) * 7))
        draw.rectangle((x, y, x + label_width, y + 15), fill=(0, 0, 0))
        draw.text((x + 2, y), label, fill=(255, 255, 255))
    return image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--results-dir", type=Path, required=True)
    parser.add_argument("--pages", nargs="*", default=("02", "07", "35"))
    args = parser.parse_args()
    report = json.loads((args.results_dir / "summary.json").read_text())
    tile_w, tile_h = 230, 550
    columns = 5
    rows = (len(report["pages"]) + columns - 1) // columns
    sheet = Image.new("RGB", (columns * tile_w, rows * tile_h), "#242424")
    sheet_draw = ImageDraw.Draw(sheet)
    for index, page in enumerate(report["pages"]):
        stem = Path(page["file"]).stem
        marked = overlay(args.source_dir / page["file"], args.results_dir / stem / "detections.json")
        if stem in args.pages:
            marked.save(args.results_dir / f"overlay-{stem}.png")
        marked.thumbnail((tile_w - 8, tile_h - 32), Image.Resampling.LANCZOS)
        x = (index % columns) * tile_w
        y = (index // columns) * tile_h
        sheet.paste(marked, (x + (tile_w - marked.width)//2, y + 28))
        sheet_draw.text((x + 5, y + 6), f"{stem}  {len(page['detections'])} proposals / {page['detections_at_demo_threshold_0_4']} >=.4", fill="white")
    sheet.save(args.results_dir / "contact-sheet.png")


if __name__ == "__main__":
    main()
