#!/usr/bin/env python3
"""Compare 45 native WebGPU logits against saved PyTorch MPS masks exactly."""
import argparse
import json
from pathlib import Path
import numpy as np
from PIL import Image


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("result", type=Path)
    parser.add_argument("reference_masks", type=Path)
    args = parser.parse_args()
    report = json.loads(args.result.read_text())
    inputs = {entry["page"]: entry for entry in json.loads(Path(report["prepared_manifest"]).read_text())}
    changed_total = 0
    for row in report["pages"]:
        page = row["page"]
        info = inputs[page]
        width, height = info["resized_wh"]
        logits = np.fromfile(row["raw_logits"], dtype="<f4").reshape(1, 1, 1024, 1024)
        if not np.isfinite(logits).all():
            raise ValueError(f"nonfinite logits on page {page}")
        mask = np.asarray(Image.fromarray((logits[0, 0, :height, :width] > 0).astype(np.uint8) * 255)
                          .resize(tuple(info["original_wh"]), Image.Resampling.NEAREST))
        reference_path = args.reference_masks / f"{page}.png"
        reference = np.asarray(Image.open(reference_path).convert("L"))
        if reference.shape != mask.shape:
            raise ValueError(f"mask shape mismatch on page {page}")
        changed = int(np.count_nonzero(mask != reference))
        changed_total += changed
        row["mask_parity"] = {"changed_pixels": changed, "exact": changed == 0,
                              "webgpu_positive_pixels": int(np.count_nonzero(mask)),
                              "reference_positive_pixels": int(np.count_nonzero(reference)),
                              "reference_mask": str(reference_path.resolve())}
        if page == "27":
            row["page27_canvas_logit_162_463"] = float(logits[0, 0, 463, 162])
            row["page27_restored_pixels_253_723_724"] = [int(mask[723, 253]), int(mask[724, 253])]
        Path(row.pop("raw_logits")).unlink()
        row["raw_logits_removed_after_verification"] = True
        print(f"{page}: {changed} changed pixels")
    report["mask_parity"] = {"pages": len(report["pages"]), "changed_pixels_total": changed_total,
                             "exact_pages": sum(row["mask_parity"]["exact"] for row in report["pages"]),
                             "reference": "Saved PyTorch MPS masks, Pillow-prepared original JPEGs; distinct from app-decoded JPEG canonical path"}
    report["prepared_inputs"] = inputs
    report.pop("prepared_manifest")
    args.result.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"exact pages: {report['mask_parity']['exact_pages']}/{len(report['pages'])}; changed pixels: {changed_total}")


if __name__ == "__main__":
    main()
