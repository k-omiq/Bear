#!/usr/bin/env python3
"""Verify the saved 45-page fusion evidence without running models or cloud jobs."""
from __future__ import annotations

import hashlib
import json
import argparse
from pathlib import Path


ROOT = Path(__file__).resolve().parent / ".cache/live-chapter-109"


def read(path: Path) -> dict:
    return json.loads(path.read_text())


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
    return hasher.hexdigest()


def require_hash(path: Path, expected: str) -> None:
    actual = digest(path)
    if actual != expected:
        raise AssertionError(f"{path}: {actual} != {expected}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="Saved live-chapter-109 artifact directory")
    parser.add_argument("--source-dir", type=Path, help="Original 45-page JPEG directory, if moved")
    args = parser.parse_args()
    root = args.root
    run = read(root / "run.json")
    summary = read(root / "fusion/summary.json")
    assert run["status"] == "complete" and len(run["pages"]) == 45
    assert len(summary["pages"]) == 45
    source_dir = args.source_dir or Path(run["source_dir"])
    require_hash(root / "rt.json", run["artifact_sha256"]["rt"])
    rt = read(root / "rt.json")
    assert rt["model_sha256"] == run["model_sha256"]["rt"]
    rt_pages = {entry["file"]: entry for entry in rt["pages"]}
    totals = {"pages": 0, "sam_components": 0, "detector_only": 0,
              "unchanged_sam_masks": 0, "source_hashes": 0}
    for entry in summary["pages"]:
        stem = entry["page"]
        source = source_dir / f"{stem}.jpg"
        fusion_path = root / "fusion" / entry["file"]
        require_hash(fusion_path, entry["sha256"])
        fused = read(fusion_path)
        sam_meta_path = root / "sam" / f"page-{stem}.json"
        sam = read(sam_meta_path)
        mask_path = root / "sam/masks" / f"{stem}.png"
        require_hash(source, run["pages"][source.name])
        assert sam["source_sha256"] == fused["source_sha256"] == run["pages"][source.name]
        require_hash(mask_path, sam["mask_sha256"])
        assert fused["inputs"]["sam_mask_sha256"] == sam["mask_sha256"]
        require_hash(sam_meta_path, fused["inputs"]["sam_metadata_sha256"])
        require_hash(root / "coo" / stem / "detections.json",
                     fused["inputs"]["coo_detections_sha256"])
        label_path = root / "fusion" / fused["component_labels"]["file"]
        require_hash(label_path, fused["component_labels"]["sha256"])
        components = fused["sam_components"]
        candidates = fused["candidates"]
        sam_candidates = [candidate for candidate in candidates if candidate["kind"] == "sam_component"]
        assert len(sam_candidates) == len(components)
        assert {candidate["sam_component_id"] for candidate in sam_candidates} == {
            component["id"] for component in components}
        assert all(candidate["mask_label_id"] is not None for candidate in sam_candidates)
        detector_only = [candidate for candidate in candidates if candidate["kind"].startswith("detector_only")]
        assert all(candidate["mask_label_id"] is None for candidate in detector_only)
        assert all(region["class"] in ("bubble", "text_free", "text_bubble")
                   for region in fused["rt_boxes"])
        assert [{key: box[key] for key in ("class", "score", "x", "y", "width", "height")}
                for box in fused["rt_boxes"]] == rt_pages[f"{stem}.jpg"]["boxes"]
        totals["pages"] += 1
        totals["source_hashes"] += 1
        totals["unchanged_sam_masks"] += 1
        totals["sam_components"] += len(components)
        totals["detector_only"] += len(detector_only)

    pages = {stem: read(root / "fusion" / f"{stem}.json")
             for stem in ("02", "16", "24", "30", "35")}
    assert len(pages["02"]["rt_boxes"]) == 0
    assert len(pages["02"]["sam_components"]) == 7
    assert len(pages["02"]["grouping_suggestions"]) == 2
    assert len(pages["16"]["sam_components"]) == 2
    assert not pages["16"]["rt_boxes"] and not pages["16"]["coo_polygons"]
    assert any(candidate["kind"] == "detector_only_coo" for candidate in pages["24"]["candidates"])
    assert not pages["30"]["candidates"]
    assert len(pages["35"]["sam_components"]) == 4
    assert all(candidate["source_evidence"] == "sam_only"
               for candidate in pages["35"]["candidates"])
    assert totals == {"pages": 45, "sam_components": 922, "detector_only": 4,
                      "unchanged_sam_masks": 45, "source_hashes": 45}, totals
    print(json.dumps({"status": "verified", "totals": totals,
                      "page_behaviors": ["02", "16", "24", "30", "35"]}, indent=2))


if __name__ == "__main__":
    main()
