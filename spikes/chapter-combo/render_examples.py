#!/usr/bin/env python3
"""Render aligned four-panel examples for the chapter 109 detector comparison."""

import argparse
import hashlib
import json
from pathlib import Path

from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_RT = ROOT / "spikes/chapter-rtdetr/artifacts/chapter-109-fp32-halves.json"
DEFAULT_COO = ROOT / "spikes/coo-mtsv3/artifacts/chapter-109"
DEFAULT_SAM = ROOT / "spikes/sam-ts-l/artifacts/examples/chapter-109-torch-mps"
DEFAULT_OUT = ROOT / "spikes/chapter-combo/.cache/visual-examples"
PAGES = ("02", "07", "16", "24")
COO_MIN_SCORE = 0.4

RT_COLORS = {
    "text_free": (255, 80, 80),
    "text_bubble": (255, 190, 35),
    "bubble": (80, 210, 255),
}
COO_COLOR = (255, 45, 205)
SAM_COLOR = (35, 225, 255)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fit(image: Image.Image, size: tuple[int, int]) -> Image.Image:
    image = image.copy()
    image.thumbnail(size, Image.Resampling.LANCZOS)
    return image


def label(draw: ImageDraw.ImageDraw, xy: tuple[int, int], text: str) -> None:
    draw.text(xy, text, fill=(255, 255, 255), stroke_width=2, stroke_fill=(18, 20, 26))


def render_page(stem: str, rt_page: dict, coo_page: dict, source: Path,
                sam_dir: Path, output: Path) -> dict:
    source_hash = sha256(source)
    image = Image.open(source).convert("RGB")
    width, height = image.size

    coo_size = coo_page["original_size"]
    if source_hash != coo_page["sha256"] or (width, height) != tuple(coo_size):
        raise ValueError(f"COO/source identity or dimensions differ for page {stem}")
    if (width, height) != (rt_page["width"], rt_page["height"]):
        raise ValueError(f"RT/source dimensions differ for page {stem}")

    sam_mask_path = sam_dir / "masks" / f"{stem}.png"
    sam_meta_path = sam_dir / f"page-{stem}.json"
    sam_mask = Image.open(sam_mask_path).convert("L")
    sam_meta = json.loads(sam_meta_path.read_text())
    if (width, height) != sam_mask.size or (width, height) != tuple(sam_meta["original_wh"]):
        raise ValueError(f"SAM/source dimensions differ for page {stem}")
    if source_hash != sam_meta["source_sha256"]:
        raise ValueError(f"SAM/source checksum differs for page {stem}")

    panels: list[tuple[str, Image.Image]] = [("Original", image)]

    rt_panel = image.copy()
    draw = ImageDraw.Draw(rt_panel)
    rt_count = 0
    for box in rt_page["boxes"]:
        color = RT_COLORS.get(box["class"], (255, 255, 255))
        x0, y0 = box["x"], box["y"]
        x1, y1 = x0 + box["width"], y0 + box["height"]
        draw.rectangle((x0, y0, x1, y1), outline=color, width=3)
        label(draw, (x0, max(0, y0 - 19)), f"{box['class']} {box['score']:.2f}")
        rt_count += 1
    panels.append((f"RT-DETR FP32 halves ({rt_count} boxes)", rt_panel))

    coo_panel = image.copy()
    draw = ImageDraw.Draw(coo_panel)
    coo_count = 0
    for index, detection in enumerate(coo_page["detections"]):
        if detection["score"] < COO_MIN_SCORE:
            continue
        polygon = [tuple(point) for point in detection["polygon"]]
        if len(polygon) < 3:
            continue
        draw.line(polygon + [polygon[0]], fill=COO_COLOR, width=3, joint="curve")
        x0, y0, _, _ = detection["bounding_box"]
        label(draw, (int(x0), max(0, int(y0) - 19)), f"COO {index} {detection['score']:.2f}")
        coo_count += 1
    panels.append((f"COO MTSv3 (score ≥ {COO_MIN_SCORE:.1f}; {coo_count} polygons)", coo_panel))

    tint = Image.new("RGB", image.size, SAM_COLOR)
    # Use a fixed tint opacity only where SAM marks foreground.
    alpha = sam_mask.point(lambda value: 92 if value > 0 else 0)
    sam_panel = Image.composite(tint, image, alpha)
    draw = ImageDraw.Draw(sam_panel)
    sam_positive_pixels = sum(1 for value in sam_mask.getdata() if value > 0)
    label(draw, (8, 8), f"SAM-TS-L mask tint ({sam_positive_pixels} px)")
    panels.append(("SAM mask overlay", sam_panel))

    panel_w = 520
    panel_h = round(panel_w * height / width)
    header_h = 32
    gap = 14
    canvas = Image.new("RGB", (2 * panel_w + 3 * gap, 2 * (panel_h + header_h) + 3 * gap), (28, 31, 38))
    draw = ImageDraw.Draw(canvas)
    for index, (title, panel) in enumerate(panels):
        col, row = index % 2, index // 2
        x = gap + col * (panel_w + gap)
        y = gap + row * (panel_h + header_h + gap)
        draw.text((x + 4, y + 7), title, fill=(245, 245, 245))
        canvas.paste(fit(panel, (panel_w, panel_h)), (x, y + header_h))

    output.mkdir(parents=True, exist_ok=True)
    path = output / f"chapter-109-page-{stem}.png"
    canvas.save(path, optimize=True)
    return {
        "page": stem,
        "source_sha256": source_hash,
        "dimensions": [width, height],
        "rt_box_count": rt_count,
        "coo_polygon_count": coo_count,
        "sam_positive_pixels": sam_positive_pixels,
        "output": str(path),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rt-json", type=Path, default=DEFAULT_RT)
    parser.add_argument("--coo-dir", type=Path, default=DEFAULT_COO)
    parser.add_argument("--sam-dir", type=Path, default=DEFAULT_SAM)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--pages", nargs="*", default=list(PAGES), help="page stems; default: 02 07 16 24")
    args = parser.parse_args()

    rt = json.loads(args.rt_json.read_text())
    coo_summary = json.loads((args.coo_dir / "summary.json").read_text())
    rt_by = {Path(page["file"]).stem: page for page in rt["pages"]}
    coo_by = {Path(page["file"]).stem: page for page in coo_summary["pages"]}
    source_root = Path(rt["source"]).resolve(strict=True)

    audit = []
    for stem in args.pages:
        if stem not in rt_by or stem not in coo_by:
            parser.error(f"page {stem} is missing from RT or COO output")
        source = (source_root / f"{stem}.jpg").resolve(strict=True)
        coo_page = json.loads((args.coo_dir / stem / "detections.json").read_text())
        if coo_page["sha256"] != coo_by[stem]["sha256"]:
            parser.error(f"COO page summary/detection checksum differs for page {stem}")
        audit.append(render_page(stem, rt_by[stem], coo_page, source, args.sam_dir, args.output_dir))

    print(json.dumps({"alignment": "verified by source SHA-256 and pixel dimensions", "examples": audit}, indent=2))


if __name__ == "__main__":
    main()
