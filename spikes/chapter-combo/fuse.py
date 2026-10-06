#!/usr/bin/env python3
"""Join existing RT-DETR, COO, and SAM chapter outputs without rerunning models.

SAM components are retained independently of detector evidence. Associations are
geometric observations, not semantic truth or an erase mask. Requires Pillow,
NumPy, and SciPy. Coordinates are in each page's original pixel space.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw
from scipy import ndimage


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def verify_sha(path: Path, expected: str, description: str) -> str:
    actual = sha256(path)
    if actual != expected:
        raise ValueError(f"{description} SHA-256 mismatch: {path}: {actual} != {expected}")
    return actual


def page_id(filename: str) -> str:
    return Path(filename).stem


def validate_output_dir(output_dir: Path, rt_json: Path, coo_dir: Path, sam_dir: Path) -> None:
    output = output_dir.resolve()
    rt_file = rt_json.resolve()
    if output == rt_file or output in rt_file.parents:
        raise ValueError(f"Output directory would overwrite RT JSON: {output} and {rt_file}")
    # A sibling directory beside rt.json is safe and used by run_pipeline.py.
    inputs = {"COO directory": coo_dir.resolve(), "SAM directory": sam_dir.resolve()}
    for name, input_dir in inputs.items():
        if output == input_dir or output in input_dir.parents or input_dir in output.parents:
            raise ValueError(f"Output directory overlaps {name}: {output} and {input_dir}")
    if output_dir.exists() and (not output_dir.is_dir() or any(output_dir.iterdir())):
        raise ValueError(f"Output directory must be absent or empty: {output_dir}")


def box_mask(size: tuple[int, int], box: dict) -> np.ndarray:
    width, height = size
    x0 = max(0, min(width, int(box["x"])))
    y0 = max(0, min(height, int(box["y"])))
    x1 = max(0, min(width, int(box["x"] + box["width"])))
    y1 = max(0, min(height, int(box["y"] + box["height"])))
    result = np.zeros((height, width), dtype=bool)
    result[y0:y1, x0:x1] = True
    return result


def polygon_mask(size: tuple[int, int], polygon: list) -> np.ndarray:
    canvas = Image.new("1", size, 0)
    if len(polygon) >= 3:
        ImageDraw.Draw(canvas).polygon([tuple(point) for point in polygon], fill=1)
    return np.asarray(canvas, dtype=bool)


def associations(
    labels: np.ndarray,
    areas: np.ndarray,
    region_mask: np.ndarray,
    region_id: str,
    min_shared_pixels: int,
    min_smaller_fraction: float,
) -> list[dict]:
    region_pixels = int(region_mask.sum())
    if not region_pixels:
        return []
    shared = np.bincount(labels[region_mask].ravel(), minlength=len(areas))
    links = []
    for component_number in np.flatnonzero(shared[1:]) + 1:
        pixels = int(shared[component_number])
        component_area = int(areas[component_number])
        fraction = pixels / min(component_area, region_pixels)
        if pixels >= min_shared_pixels and fraction >= min_smaller_fraction:
            links.append({
                "component_id": f"sam-{component_number:05d}",
                "region_id": region_id,
                "shared_pixels": pixels,
                "fraction_of_component": round(pixels / component_area, 6),
                "fraction_of_region": round(pixels / region_pixels, 6),
                "fraction_of_smaller_region": round(fraction, 6),
            })
    return links


def fuse_page(
    rt_page: dict,
    coo_path: Path,
    coo_manifest: dict,
    sam_meta_path: Path,
    sam_dir: Path,
    output_dir: Path,
    rt_source: Path | None,
    args: argparse.Namespace,
) -> dict:
    page = page_id(rt_page["file"])
    coo_rel = f"{page}/detections.json"
    coo_expected = coo_manifest["files"].get(coo_rel, {}).get("sha256")
    if not coo_expected:
        raise ValueError(f"COO manifest has no hash for {coo_rel}")
    coo_hash = verify_sha(coo_path, coo_expected, "COO detections")
    coo = read_json(coo_path)
    sam = read_json(sam_meta_path)
    mask_path = sam_dir / "masks" / f"{page}.png"
    mask_hash = verify_sha(mask_path, sam["mask_sha256"], "SAM mask")
    size = (int(rt_page["width"]), int(rt_page["height"]))
    if coo["file"] != rt_page["file"] or tuple(coo["original_size"]) != size:
        raise ValueError(f"COO page identity/size mismatch: {page}")
    if sam["page"] != page or tuple(sam["original_wh"]) != size:
        raise ValueError(f"SAM page identity/size mismatch: {page}")
    if Path(sam["source"]).name != rt_page["file"]:
        raise ValueError(f"SAM source filename differs from RT page: {page}")
    if coo["sha256"] != sam["source_sha256"]:
        raise ValueError(f"COO/SAM source image hashes differ: {page}")
    # The RT file has no per-page source hashes. Its file and dimensions are
    # checked; the optional image check adds pixel-source identity when present.
    source_path = Path(sam["source"])
    source_verified = source_path.is_file()
    if source_verified:
        verify_sha(source_path, sam["source_sha256"], "source image")
    rt_source_verified = False
    if rt_source is not None:
        rt_source_image = rt_source / rt_page["file"]
        rt_source_verified = rt_source_image.is_file()
        if rt_source_verified:
            verify_sha(rt_source_image, sam["source_sha256"], "RT source image")
    with Image.open(mask_path) as image:
        if image.size != size:
            raise ValueError(f"SAM mask dimensions differ: {page}")
        raw = np.asarray(image.convert("L"))
    if not np.isin(raw, (0, 255)).all():
        raise ValueError(f"SAM mask is not binary 0/255: {page}")
    foreground = raw != 0
    if int(foreground.sum()) != sam["foreground_pixels"]:
        raise ValueError(f"SAM foreground pixel count differs: {page}")

    # Eight-connectivity is specified explicitly for reproducibility.
    labels, count = ndimage.label(foreground, structure=np.ones((3, 3), dtype=np.uint8))
    areas = np.bincount(labels.ravel(), minlength=count + 1)
    slices = ndimage.find_objects(labels)
    components = []
    for number, bounds in enumerate(slices, 1):
        assert bounds is not None
        ys, xs = bounds
        components.append({
            "id": f"sam-{number:05d}",
            "area_pixels": int(areas[number]),
            "bbox_xywh": [xs.start, ys.start, xs.stop - xs.start, ys.stop - ys.start],
            "rt_box_ids": [],
            "rt_text_box_ids": [],
            "rt_bubble_box_ids": [],
            "coo_polygon_ids": [],
        })
    by_component = {item["id"]: item for item in components}
    rt_boxes = []
    coo_polygons = []
    links = []
    for index, box in enumerate(rt_page["boxes"]):
        region_id = f"rt-{index:04d}"
        if box["class"] in ("text_free", "text_bubble"):
            box_role = "text"
        elif box["class"] == "bubble":
            box_role = "bubble_context"
        else:
            raise ValueError(f"Unrecognized RT class {box['class']!r} on page {page}")
        rt_boxes.append({"id": region_id, **box, "sam_component_ids": []})
        for link in associations(labels, areas, box_mask(size, box), region_id,
                                 args.min_shared_pixels, args.min_smaller_fraction):
            link["region_source"] = "rt_detr"
            link["region_role"] = box_role
            links.append(link)
            component = by_component[link["component_id"]]
            component["rt_box_ids"].append(region_id)
            component["rt_text_box_ids" if box_role == "text" else "rt_bubble_box_ids"].append(region_id)
            rt_boxes[-1]["sam_component_ids"].append(link["component_id"])
    for index, detection in enumerate(coo["detections"]):
        if float(detection["score"]) < args.coo_min_score:
            continue
        region_id = f"coo-{index:04d}"
        coo_polygons.append({"id": region_id, **detection, "sam_component_ids": []})
        for link in associations(labels, areas, polygon_mask(size, detection["polygon"]),
                                 region_id, args.min_shared_pixels, args.min_smaller_fraction):
            link["region_source"] = "coo"
            links.append(link)
            by_component[link["component_id"]]["coo_polygon_ids"].append(region_id)
            coo_polygons[-1]["sam_component_ids"].append(link["component_id"])

    for component in components:
        text = bool(component["rt_text_box_ids"])
        bubble = bool(component["rt_bubble_box_ids"])
        coo_match = bool(component["coo_polygon_ids"])
        component["bubble_context"] = bubble
        component["evidence"] = (
            "sam_with_coo_and_rt_text" if coo_match and text
            else "sam_with_rt_text" if text
            else "sam_with_coo_and_bubble_context" if coo_match and bubble
            else "sam_with_coo" if coo_match
            else "sam_with_bubble_context" if bubble
            else "sam_only"
        )
    for region in rt_boxes + coo_polygons:
        region["association_status"] = "associated" if region["sam_component_ids"] else "detector_only"

    # A candidate is a review unit. Only SAM candidates have exact mask pixels.
    # Detector-only proposals remain visible rather than disappearing when SAM
    # has no overlapping foreground. Bubble boxes are context, never standalone
    # text candidates.
    candidates = []
    for number, component in enumerate(components, 1):
        candidates.append({
            "id": f"candidate-sam-{number:05d}",
            "kind": "sam_component",
            "mask_label_id": number,
            "bbox_xywh": component["bbox_xywh"],
            "area_pixels": component["area_pixels"],
            "source_evidence": component["evidence"],
            "sam_component_id": component["id"],
            "rt_text_box_ids": component["rt_text_box_ids"],
            "rt_bubble_box_ids": component["rt_bubble_box_ids"],
            "coo_polygon_ids": component["coo_polygon_ids"],
            "grouping_suggestion_ids": [],
        })
    candidate_by_component = {candidate["sam_component_id"]: candidate for candidate in candidates}
    for box in rt_boxes:
        if box["class"] not in ("text_free", "text_bubble") or box["sam_component_ids"]:
            continue
        candidates.append({
            "id": f'candidate-{box["id"]}',
            "kind": "detector_only_rt_text",
            "mask_label_id": None,
            "bbox_xywh": [box["x"], box["y"], box["width"], box["height"]],
            "area_pixels": None,
            "source_evidence": "rt_text_only",
            "sam_component_id": None,
            "rt_text_box_ids": [box["id"]],
            "rt_bubble_box_ids": [],
            "coo_polygon_ids": [],
            "grouping_suggestion_ids": [],
        })
    for polygon in coo_polygons:
        if polygon["sam_component_ids"]:
            continue
        x0, y0, x1, y1 = polygon["bounding_box"]
        candidates.append({
            "id": f'candidate-{polygon["id"]}',
            "kind": "detector_only_coo",
            "mask_label_id": None,
            "bbox_xywh": [x0, y0, x1 - x0, y1 - y0],
            "area_pixels": None,
            "source_evidence": "coo_only",
            "sam_component_id": None,
            "rt_text_box_ids": [],
            "rt_bubble_box_ids": [],
            "coo_polygon_ids": [polygon["id"]],
            "grouping_suggestion_ids": [],
        })

    grouping_suggestions = []
    for polygon in coo_polygons:
        component_ids = polygon["sam_component_ids"]
        if len(component_ids) < 2:
            continue
        suggestion_id = f'group-{polygon["id"]}'
        candidate_ids = [candidate_by_component[c]["id"] for c in component_ids]
        grouping_suggestions.append({
            "id": suggestion_id,
            "kind": "possible_duplicate_or_fragment_group",
            "reason": "Multiple SAM components overlap the same COO polygon; review whether they belong to one target.",
            "coo_polygon_id": polygon["id"],
            "sam_component_ids": component_ids,
            "candidate_ids": candidate_ids,
            "mask_merge_performed": False,
        })
        for component_id in component_ids:
            candidate_by_component[component_id]["grouping_suggestion_ids"].append(suggestion_id)

    label_file = None
    if args.write_labels:
        if count > 65535:
            raise ValueError(f"Too many SAM components for 16-bit labels: {page}")
        label_path = output_dir / f"{page}-labels.png"
        Image.fromarray(labels.astype(np.uint16)).save(label_path)
        label_file = {"file": label_path.name, "sha256": sha256(label_path),
                      "format": "16-bit PNG; 0=background; pixel value=component number"}

    result = {
        "page": page,
        "file": rt_page["file"],
        "size_wh": list(size),
        "source_sha256": sam["source_sha256"],
        "source_image_hash_verified": source_verified,
        "rt_source_image_hash_verified": rt_source_verified,
        "inputs": {
            "coo_detections_sha256": coo_hash,
            "sam_metadata_sha256": sha256(sam_meta_path),
            "sam_mask_sha256": mask_hash,
        },
        "rt_boxes": rt_boxes,
        "coo_polygons": coo_polygons,
        "sam_components": components,
        "candidates": candidates,
        "grouping_suggestions": grouping_suggestions,
        "associations": links,
        "component_labels": label_file,
        "counts": {
            "rt_boxes": len(rt_boxes), "coo_polygons": len(coo_polygons),
            "sam_components": count, "associations": len(links),
            "sam_only_components": sum(item["evidence"] == "sam_only" for item in components),
            "sam_with_rt_text_components": sum(bool(item["rt_text_box_ids"]) for item in components),
            "sam_with_coo_components": sum(bool(item["coo_polygon_ids"]) for item in components),
            "sam_with_bubble_context_components": sum(bool(item["rt_bubble_box_ids"]) for item in components),
            "sam_bubble_context_only_components": sum(item["evidence"] == "sam_with_bubble_context" for item in components),
            "rt_text_boxes": sum(box["class"] in ("text_free", "text_bubble") for box in rt_boxes),
            "rt_bubble_boxes": sum(box["class"] == "bubble" for box in rt_boxes),
            "rt_detector_only_boxes": sum(box["association_status"] == "detector_only" for box in rt_boxes),
            "coo_detector_only_polygons": sum(polygon["association_status"] == "detector_only" for polygon in coo_polygons),
            "candidates": len(candidates),
            "sam_candidates": count,
            "detector_only_rt_text_candidates": sum(candidate["kind"] == "detector_only_rt_text" for candidate in candidates),
            "detector_only_coo_candidates": sum(candidate["kind"] == "detector_only_coo" for candidate in candidates),
            "grouping_suggestions": len(grouping_suggestions),
        },
    }
    (output_dir / f"{page}.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rt-json", type=Path, required=True)
    parser.add_argument("--coo-dir", type=Path, required=True)
    parser.add_argument("--sam-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--coo-min-score", type=float, default=0.4)
    parser.add_argument("--min-shared-pixels", type=int, default=1)
    parser.add_argument("--min-smaller-fraction", type=float, default=0.0)
    parser.add_argument("--write-labels", action="store_true",
                        help="Write 16-bit PNG component IDs for exact SAM geometry")
    args = parser.parse_args()
    if not 0 <= args.coo_min_score <= 1 or args.min_shared_pixels < 1 or not 0 <= args.min_smaller_fraction <= 1:
        parser.error("thresholds must be within their documented ranges")
    validate_output_dir(args.output_dir, args.rt_json, args.coo_dir, args.sam_dir)
    rt = read_json(args.rt_json)
    manifest_path = args.coo_dir / "manifest.json"
    manifest = read_json(manifest_path)
    rt_pages = {page_id(item["file"]): item for item in rt["pages"]}
    if len(rt_pages) != len(rt["pages"]):
        raise ValueError("RT output has duplicate page IDs")
    coo_pages = {path.parent.name for path in args.coo_dir.glob("*/detections.json")}
    sam_pages = {path.stem.removeprefix("page-") for path in args.sam_dir.glob("page-*.json")}
    if not coo_pages or coo_pages != sam_pages:
        raise ValueError(f"COO and SAM page sets must match; COO={sorted(coo_pages)}, SAM={sorted(sam_pages)}")
    missing = coo_pages - rt_pages.keys()
    if missing:
        raise ValueError(f"RT output lacks pages: {sorted(missing)}")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    pages = []
    rt_source = Path(rt["source"]) if rt.get("source") else None
    for page in sorted(coo_pages):
        pages.append(fuse_page(rt_pages[page], args.coo_dir / page / "detections.json",
                               manifest, args.sam_dir / f"page-{page}.json", args.sam_dir,
                               args.output_dir, rt_source, args))
    summary = {
        "schema": "chapter-combo-fusion-v3",
        "interpretation": "Geometric candidate associations only; not validated text/SFX labels or an erase mask.",
        "policy": {
            "sam_components": "all 8-connected foreground regions, no detector gate",
            "association": "Every pixel overlap is linked by default; optional minimums filter links only. Neither a link nor a polygon selects an erase mask. All qualifying many-to-many links are retained.",
            "rt_roles": "text_free/text_bubble provide RT text evidence; bubble supplies context only",
            "candidates": "One candidate per SAM component plus detector-only RT text and COO proposals. Only SAM candidates have mask labels. COO grouping suggestions never merge masks.",
            "page_set": "COO and SAM page sets must match; RT may contain additional pages for subset fusion",
            "coo_min_score": args.coo_min_score,
            "min_shared_pixels": args.min_shared_pixels,
            "min_smaller_fraction": args.min_smaller_fraction,
        },
        "inputs": {
            "fusion_script_sha256": sha256(Path(__file__)),
            "rt_json": str(args.rt_json.resolve()), "rt_json_sha256": sha256(args.rt_json),
            "rt_model_sha256": rt.get("model_sha256"),
            "coo_dir": str(args.coo_dir.resolve()), "coo_manifest_sha256": sha256(manifest_path),
            "coo_checkpoint_sha256": manifest.get("checkpoint_sha256"),
            "sam_dir": str(args.sam_dir.resolve()),
        },
        "pages": [{"page": item["page"], "file": f'{item["page"]}.json',
                   "sha256": sha256(args.output_dir / f'{item["page"]}.json'),
                   **item["counts"]} for item in pages],
        "totals": {
            key: sum(item["counts"][key] for item in pages)
            for key in pages[0]["counts"]
        } | {
            "pages": len(pages),
            "source_images_hash_verified": sum(item["source_image_hash_verified"] for item in pages),
            "rt_source_images_hash_verified": sum(item["rt_source_image_hash_verified"] for item in pages),
        },
    }
    (args.output_dir / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    print(f"Fused {len(pages)} pages, {sum(len(p['sam_components']) for p in pages)} SAM components -> {args.output_dir}")


if __name__ == "__main__":
    main()
