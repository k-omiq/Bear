#!/usr/bin/env python3
"""Render compact source and fused-candidate previews from one pipeline run.

These previews show model candidates and geometric associations. They are not
validated text labels or erase masks. Requires Pillow and NumPy.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont


PAGES = ("02", "07", "16", "20", "24", "30", "35", "37")
PALETTE = {
    "both": (70, 255, 120),
    "rt_text": (255, 92, 75),
    "coo": (255, 45, 205),
    "sam_only": (255, 220, 45),
    "bubble_context": (55, 205, 255),
}
RT_COLORS = {"text_free": (255, 92, 75), "text_bubble": (255, 170, 40),
             "bubble": (55, 205, 255)}
COO_COLOR = (255, 45, 205)
BG = (25, 28, 35)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def check_hash(path: Path, expected: str, what: str) -> str:
    if not path.is_file():
        raise ValueError(f"Missing {what}: {path}")
    actual = sha256(path)
    if actual != expected:
        raise ValueError(f"{what} SHA-256 mismatch for {path}: {actual} != {expected}")
    return actual


def dashed_line(draw: ImageDraw.ImageDraw, points: list[tuple[float, float]],
                fill: tuple[int, int, int], width: int = 3, dash: int = 9) -> None:
    """Draw a closed dashed polyline, used for detector-only proposals."""
    if len(points) < 2:
        return
    path = points + [points[0]]
    for start, end in zip(path, path[1:]):
        x0, y0 = start
        x1, y1 = end
        length = float(np.hypot(x1 - x0, y1 - y0))
        if length <= 0:
            continue
        cursor = 0.0
        while cursor < length:
            stop = min(length, cursor + dash)
            a, b = cursor / length, stop / length
            draw.line((x0 + (x1 - x0) * a, y0 + (y1 - y0) * a,
                       x0 + (x1 - x0) * b, y0 + (y1 - y0) * b), fill=fill, width=width)
            cursor += dash * 2


def draw_label(draw: ImageDraw.ImageDraw, xy: tuple[int, int], text: str,
               color: tuple[int, int, int]) -> None:
    draw.text(xy, text, fill=color, stroke_width=2, stroke_fill=BG)


def component_colors(fusion: dict) -> dict[int, tuple[int, int, int]]:
    result = {}
    for component in fusion["sam_components"]:
        has_rt = bool(component["rt_text_box_ids"])
        has_coo = bool(component["coo_polygon_ids"])
        has_bubble = bool(component["rt_bubble_box_ids"])
        if has_rt and has_coo:
            kind = "both"
        elif has_rt:
            kind = "rt_text"
        elif has_coo:
            kind = "coo"
        elif has_bubble:
            kind = "bubble_context"
        else:
            kind = "sam_only"
        number = int(component["id"].rsplit("-", 1)[1])
        result[number] = PALETTE[kind]
    return result


def make_overlay(source: Image.Image, labels: np.ndarray, fusion: dict) -> Image.Image:
    rgb = np.asarray(source.convert("RGB"), dtype=np.uint8).copy()
    colors = component_colors(fusion)
    for number, color in colors.items():
        selected = labels == number
        if selected.any():
            # Keep source visible while making component evidence legible.
            rgb[selected] = ((rgb[selected].astype(np.uint16) +
                              np.asarray(color, dtype=np.uint16)) // 2).astype(np.uint8)
    overlay = Image.fromarray(rgb)
    draw = ImageDraw.Draw(overlay)

    for box in fusion["rt_boxes"]:
        color = RT_COLORS[box["class"]]
        x, y, w, h = (float(box[key]) for key in ("x", "y", "width", "height"))
        corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
        label = f"{box['id']} {box['class']} {float(box['score']):.2f}"
        if box["association_status"] == "detector_only":
            label += " no SAM"
            dashed_line(draw, corners, color)
        else:
            draw.rectangle((x, y, x + w, y + h), outline=color, width=3)
        draw_label(draw, (int(x), max(1, int(y) - 19)), label, color)

    for polygon in fusion["coo_polygons"]:
        points = [tuple(map(float, point)) for point in polygon["polygon"]]
        if len(points) < 3:
            continue
        label = f"{polygon['id']} COO {float(polygon['score']):.2f}"
        if polygon["association_status"] == "detector_only":
            label += " no SAM"
            dashed_line(draw, points, COO_COLOR)
        else:
            draw.line(points + [points[0]], fill=COO_COLOR, width=3, joint="curve")
        x0 = int(min(point[0] for point in points))
        y0 = int(min(point[1] for point in points))
        draw_label(draw, (x0, max(1, y0 - 19)), label, COO_COLOR)
    return overlay


def page_paths(run_dir: Path, stem: str, run: dict) -> tuple[Path, ...]:
    filename = next((name for name in run["pages"] if Path(name).stem == stem), None)
    if filename is None:
        raise ValueError(f"Page {stem} is not listed in run.json")
    root = Path(run.get("input_dir") or run["source_dir"]).resolve(strict=True)
    source = (root / filename).resolve(strict=True)
    rt_path = run_dir / "rt.json"
    coo_path = run_dir / "coo" / stem / "detections.json"
    coo_manifest_path = run_dir / "coo" / "manifest.json"
    sam_meta_path = run_dir / "sam" / f"page-{stem}.json"
    sam_mask_path = run_dir / "sam" / "masks" / f"{stem}.png"
    fusion_path = run_dir / "fusion" / f"{stem}.json"
    labels_path = run_dir / "fusion" / f"{stem}-labels.png"
    return source, rt_path, coo_path, coo_manifest_path, sam_meta_path, sam_mask_path, fusion_path, labels_path


def render_one(run_dir: Path, stem: str, run: dict, output_dir: Path) -> dict:
    (source_path, rt_path, coo_path, coo_manifest_path, sam_meta_path,
     sam_mask_path, fusion_path, labels_path) = page_paths(run_dir, stem, run)
    source_hash = check_hash(source_path, run["pages"][source_path.name], "source image")
    rt = read_json(rt_path)
    rt_page = next((page for page in rt["pages"] if Path(page["file"]).stem == stem), None)
    if rt_page is None:
        raise ValueError(f"RT output has no page {stem}")
    coo = read_json(coo_path)
    coo_manifest = read_json(coo_manifest_path)
    coo_meta = coo_manifest["files"].get(f"{stem}/detections.json")
    if not coo_meta:
        raise ValueError(f"COO manifest has no detections entry for page {stem}")
    check_hash(coo_path, coo_meta["sha256"], "COO detections")
    sam_meta = read_json(sam_meta_path)
    fusion = read_json(fusion_path)
    fusion_summary = read_json(run_dir / "fusion" / "summary.json")
    fusion_page = next((page for page in fusion_summary["pages"] if page["page"] == stem), None)
    if fusion_page is None:
        raise ValueError(f"Fusion summary has no page {stem}")
    check_hash(fusion_path, fusion_page["sha256"], "fusion page JSON")
    source = Image.open(source_path).convert("RGB")
    width, height = source.size
    size = (width, height)
    if ((rt_page["width"], rt_page["height"]) != size
            or tuple(coo["original_size"]) != size
            or tuple(sam_meta["original_wh"]) != size
            or tuple(fusion["size_wh"]) != size):
        raise ValueError(f"Source/RT/COO/SAM/fusion dimensions differ for page {stem}")
    expected_source_hashes = (run["pages"][source_path.name], coo["sha256"],
                              sam_meta["source_sha256"], fusion["source_sha256"])
    if any(value != source_hash for value in expected_source_hashes):
        raise ValueError(f"Source SHA-256 differs across run artifacts for page {stem}")
    if coo["file"] != source_path.name or fusion["file"] != source_path.name:
        raise ValueError(f"Page filename identity differs for page {stem}")
    if sam_meta["page"] != stem:
        raise ValueError(f"SAM page identity differs for page {stem}")

    check_hash(sam_mask_path, sam_meta["mask_sha256"], "SAM mask")
    check_hash(sam_meta_path, fusion["inputs"]["sam_metadata_sha256"], "SAM metadata")
    if fusion["inputs"]["sam_mask_sha256"] != sam_meta["mask_sha256"]:
        raise ValueError(f"Fusion/SAM mask checksum differs for page {stem}")
    if fusion["inputs"]["coo_detections_sha256"] != sha256(coo_path):
        raise ValueError(f"Fusion/COO checksum differs for page {stem}")
    if coo["sha256"] != source_hash:
        raise ValueError(f"COO source checksum differs for page {stem}")

    raw_sam = np.asarray(Image.open(sam_mask_path).convert("L"))
    if raw_sam.shape != (height, width) or not np.isin(raw_sam, (0, 255)).all():
        raise ValueError(f"SAM mask dimensions or binary values are invalid for page {stem}")
    if int(np.count_nonzero(raw_sam)) != int(sam_meta["foreground_pixels"]):
        raise ValueError(f"SAM mask pixel count differs for page {stem}")
    label_image = Image.open(labels_path)
    if label_image.size != size:
        raise ValueError(f"Component label dimensions differ for page {stem}")
    if not fusion.get("component_labels"):
        raise ValueError(f"Fusion output has no component label reference for page {stem}")
    check_hash(labels_path, fusion["component_labels"]["sha256"], "component labels")
    labels = np.asarray(label_image)
    if labels.ndim != 2 or labels.shape != (height, width):
        raise ValueError(f"Component label map is not a single-channel page image: {stem}")
    if np.any((labels > 0) != (raw_sam > 0)):
        raise ValueError(f"Component labels do not exactly cover SAM foreground for page {stem}")
    known_ids = {int(component["id"].rsplit("-", 1)[1])
                 for component in fusion["sam_components"]}
    if set(map(int, np.unique(labels))) - {0} != known_ids:
        raise ValueError(f"Component label IDs differ from fusion metadata for page {stem}")

    overlay = make_overlay(source, labels, fusion)
    panel_w = 370
    panel_h = round(panel_w * height / width)
    gap = 14
    header_h = 82
    footer_h = 94
    canvas = Image.new("RGB", (2 * panel_w + 3 * gap,
                               header_h + panel_h + footer_h + 3 * gap), BG)
    draw = ImageDraw.Draw(canvas)
    draw.text((gap, 10), f"Page {stem} · fused detection candidates", fill=(255, 255, 255))
    draw.text((gap, 33), "Candidate associations only · overlays are not erase masks",
              fill=(245, 210, 120))
    draw.text((gap, 55), "Dashed outlines = detector candidate with no linked SAM component",
              fill=(215, 218, 225))
    y = header_h + gap
    draw.text((gap, y - 17), "Original", fill=(245, 245, 245))
    draw.text((2 * gap + panel_w, y - 17), "SAM components + RT / COO outlines",
              fill=(245, 245, 245))
    canvas.paste(source.resize((panel_w, panel_h), Image.Resampling.LANCZOS), (gap, y))
    canvas.paste(overlay.resize((panel_w, panel_h), Image.Resampling.LANCZOS),
                 (2 * gap + panel_w, y))
    legend_y = y + panel_h + gap
    draw.text((gap, legend_y), "SAM pixels by component evidence:", fill=(235, 235, 240))
    entries = [("both", "RT text + COO"), ("rt_text", "RT text"),
               ("coo", "COO"), ("sam_only", "SAM only"),
               ("bubble_context", "bubble context")]
    x, row_y = gap, legend_y + 20
    for index, (key, title) in enumerate(entries):
        if index == 3:
            x, row_y = gap, row_y + 23
        draw.rectangle((x, row_y + 2, x + 12, row_y + 14), fill=PALETTE[key])
        draw.text((x + 17, row_y), title, fill=(235, 235, 240))
        x += (125 if index in (0, 2) else 110)

    output_dir.mkdir(parents=True, exist_ok=True)
    output = output_dir / f"fused-page-{stem}.png"
    canvas.save(output, optimize=True)
    return {"page": stem, "source_sha256": source_hash, "dimensions": list(size),
            "component_count": len(fusion["sam_components"]),
            "rt_box_count": len(fusion["rt_boxes"]),
            "coo_polygon_count": len(fusion["coo_polygons"]), "output": str(output)}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run_dir", type=Path, help="One-command pipeline output directory")
    parser.add_argument("--pages", nargs="*", default=list(PAGES),
                        help="Page stems; unavailable requested pages are skipped")
    parser.add_argument("--output-dir", type=Path,
                        help="Preview directory; defaults to RUN_DIR/fusion/previews")
    args = parser.parse_args()
    run_dir = args.run_dir.resolve(strict=True)
    run = read_json(run_dir / "run.json")
    if run.get("status") != "complete":
        parser.error(f"run is not complete: {run.get('status')}")
    output_dir = args.output_dir or run_dir / "fusion" / "previews"
    available = {path.stem for path in (run_dir / "fusion").glob("*.json")
                 if path.name != "summary.json"}
    selected = [stem for stem in args.pages if stem in available]
    missing = [stem for stem in args.pages if stem not in available]
    if not selected:
        parser.error(f"none of the requested pages are in this run: {args.pages}")
    examples = [render_one(run_dir, stem, run, output_dir) for stem in selected]
    print(json.dumps({"interpretation": "candidate association preview, not an erase mask",
                      "skipped_pages_not_in_run": missing, "examples": examples}, indent=2))


if __name__ == "__main__":
    main()
