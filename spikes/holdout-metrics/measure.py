#!/usr/bin/env python3
"""Measure a labeled holdout against chapter fusion or applied masks."""

import argparse
import csv
import json
from collections import deque
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw


SLICES = ("japanese", "korean", "bubble", "sfx", "tiny", "outlined",
          "art-contact", "longstrip")


def binary(path, size=None):
    with Image.open(path) as image:
        array = np.asarray(image)
    if array.ndim == 3:
        raise ValueError(f"mask must be one channel: {path}")
    if size is not None and array.shape != size:
        raise ValueError(f"mask dimensions differ: {path}")
    return array != 0


def shape_mask(instance, shape):
    height, width = shape
    image = Image.new("1", (width, height))
    draw = ImageDraw.Draw(image)
    if "box_xywh" in instance or "bbox_xywh" in instance:
        x, y, w, h = instance.get("box_xywh", instance.get("bbox_xywh"))
        if w <= 0 or h <= 0:
            raise ValueError("instance box must have positive dimensions")
        draw.rectangle((x, y, x + w - 1, y + h - 1), fill=1)
    elif "polygon" in instance:
        draw.polygon([tuple(point) for point in instance["polygon"]], fill=1)
    else:
        raise ValueError("instance needs box_xywh or polygon")
    return np.asarray(image, dtype=bool)


def components(mask):
    """Return 8-connected pixel components as boolean masks."""
    height, width = mask.shape
    seen = np.zeros(mask.shape, dtype=bool)
    result = []
    for y, x in zip(*np.nonzero(mask)):
        if seen[y, x]:
            continue
        points = []
        queue = deque([(int(y), int(x))])
        seen[y, x] = True
        while queue:
            cy, cx = queue.popleft()
            points.append((cy, cx))
            for ny in range(max(0, cy - 1), min(height, cy + 2)):
                for nx in range(max(0, cx - 1), min(width, cx + 2)):
                    if mask[ny, nx] and not seen[ny, nx]:
                        seen[ny, nx] = True
                        queue.append((ny, nx))
        component = np.zeros(mask.shape, dtype=bool)
        ys, xs = zip(*points)
        component[ys, xs] = True
        result.append(component)
    return result


def prediction(path, shape):
    if path.suffix.lower() == ".png":
        mask = binary(path, shape)
        return mask, components(mask), {}
    data = json.loads(path.read_text())
    if "component_labels" in data:
        labels_path = path.parent / data["component_labels"]["file"]
        with Image.open(labels_path) as image:
            labels = np.asarray(image)
        if labels.shape != shape:
            raise ValueError(f"component label dimensions differ: {labels_path}")
        candidates = []
        for candidate in data["candidates"]:
            if candidate["kind"] == "sam_component":
                candidates.append(labels == candidate["mask_label_id"])
            else:
                candidates.append(shape_mask(candidate, shape))
        mask = labels != 0
    else:
        mask_path = Path(data["mask_png"])
        if not mask_path.is_absolute():
            mask_path = path.parent / mask_path
        mask = binary(mask_path, shape)
        candidates = components(mask)
    return mask, candidates, data


def page_counts(label, labels_dir, prediction_path, threshold):
    lettering = binary(labels_dir / label["lettering_mask"])
    protected = binary(labels_dir / label["protected_art_mask"], lettering.shape)
    predicted, candidates, metadata = prediction(prediction_path, lettering.shape)
    instances = []
    for instance in label["instances"]:
        footprint = shape_mask(instance, lettering.shape) & lettering
        if not footprint.any():
            raise ValueError(f"instance has no labeled lettering pixels: {instance['id']}")
        instances.append(footprint)
    found = sum(np.count_nonzero(predicted & item) / np.count_nonzero(item) >= threshold
                for item in instances)
    false = sum(not any(np.any(candidate & item) for item in instances)
                for candidate in candidates)
    runtime = metadata.get("runtime_ms", metadata.get("timing_ms", {}).get("page_total"))
    peak = metadata.get("peak_memory_bytes", metadata.get("peak_process_rss_bytes"))
    return {
        "instances": len(instances), "found_instances": found,
        "complete_pages": int(found == len(instances)), "pages": 1,
        "lettering_pixels": int(lettering.sum()),
        "recalled_lettering_pixels": int((lettering & predicted).sum()),
        "non_text_exposure_pixels": int((predicted & ~lettering).sum()),
        "protected_art_damage_pixels": int((predicted & protected).sum()),
        "false_candidates": false, "candidates": len(candidates),
        "runtime_ms": runtime, "peak_memory_bytes": peak,
    }


def summary(rows, correction_times):
    totals = {key: sum(row[key] for row in rows) for key in
              ("instances", "found_instances", "complete_pages", "pages",
               "lettering_pixels", "recalled_lettering_pixels", "non_text_exposure_pixels",
               "protected_art_damage_pixels", "false_candidates", "candidates")}
    pages = totals["pages"]
    timed = [row["runtime_ms"] for row in rows if row["runtime_ms"] is not None]
    totals.update({
        "instance_recall": totals["found_instances"] / totals["instances"] if totals["instances"] else None,
        "complete_page_recall": totals["complete_pages"] / pages if pages else None,
        "text_pixel_recall": totals["recalled_lettering_pixels"] / totals["lettering_pixels"] if totals["lettering_pixels"] else None,
        "false_candidates_per_page": totals["false_candidates"] / pages if pages else None,
        "correction_time_seconds_total": sum(correction_times) if correction_times else None,
        "correction_time_seconds_per_page": sum(correction_times) / len(correction_times) if correction_times else None,
        "correction_time_pages": len(correction_times),
        "runtime_ms_total": sum(timed) if timed else None,
        "runtime_pages": len(timed),
        "peak_memory_bytes_max": max((row["peak_memory_bytes"] for row in rows
                                      if row["peak_memory_bytes"] is not None), default=None),
    })
    return totals


def measure(labels_path, predictions_dir, corrections_path=None, threshold=0.5):
    labels = json.loads(labels_path.read_text())
    times = {}
    if corrections_path:
        with corrections_path.open(newline="") as stream:
            for row in csv.DictReader(stream):
                times[row["page_id"]] = times.get(row["page_id"], 0) + float(row["seconds"])
    rows = []
    seen = set()
    for page in labels["pages"]:
        page_id = page["id"]
        if page_id in seen:
            raise ValueError(f"duplicate page id: {page_id}")
        seen.add(page_id)
        file = page.get("prediction", f"{page_id}.json")
        path = predictions_dir / file
        if not path.is_file():
            raise FileNotFoundError(path)
        counts = page_counts(page, labels_path.parent, path, threshold)
        tags = set(page.get("tags", []))
        for instance in page["instances"]:
            tags.update(instance.get("tags", []))
        if unknown := tags - set(SLICES):
            raise ValueError(f"unknown slice tags for {page_id}: {sorted(unknown)}")
        rows.append((page_id, tags, counts))
    def group(selected):
        return summary([counts for _, _, counts in selected],
                       [times[page_id] for page_id, _, _ in selected if page_id in times])
    return {"definition": {"instance_found_fraction": threshold,
                            "candidate_connectivity": 8},
            "overall": group(rows),
            "slices": {tag: group([row for row in rows if tag in row[1]]) for tag in SLICES},
            "pages": {page_id: counts for page_id, _, counts in rows}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--labels", type=Path, required=True)
    parser.add_argument("--predictions", type=Path, required=True)
    parser.add_argument("--corrections", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--instance-threshold", type=float, default=0.5)
    args = parser.parse_args()
    if not 0 < args.instance_threshold <= 1:
        parser.error("instance threshold must be in (0, 1]")
    result = measure(args.labels, args.predictions, args.corrections,
                     args.instance_threshold)
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(encoded)
    else:
        print(encoded, end="")


if __name__ == "__main__":
    main()
