#!/usr/bin/env python3
"""Finish native Rust/ORT mask parity using the pinned Pillow resize semantics."""
import json
import sys
from pathlib import Path
import numpy as np
from PIL import Image


def main(path: Path) -> None:
    report = json.loads(path.read_text())
    page = Path(report["page"])
    logits = np.fromfile(report["raw_logits_f32le"], dtype="<f4").reshape(1, 1, 1024, 1024)
    reference = np.load(page / "reference_logits_f32.npy", allow_pickle=False)
    resized = report["input_source"]["resized_wh"]
    original = report["input_source"]["original_wh"]
    width, height = resized
    mask = np.asarray(Image.fromarray(((logits[0, 0, :height, :width] > 0).astype(np.uint8) * 255))
                      .resize(tuple(original), Image.Resampling.NEAREST), dtype=np.uint8)
    expected = np.load(page / "reference_restored_mask_u8.npy", allow_pickle=False)
    assert np.array_equal(np.asarray(Image.fromarray(((reference[0, 0, :height, :width] > 0).astype(np.uint8) * 255))
                                    .resize(tuple(original), Image.Resampling.NEAREST)), expected)
    changed = int(np.count_nonzero(mask != expected))
    report["parity"].update(changed_restored_pixels=changed, exact_restored_mask_parity=changed == 0,
                            output_mask_pixels=int(np.count_nonzero(mask)))
    report["parity"].pop("mask_parity_note", None)
    report["parity"]["restoration"] = "crop, threshold >0, Pillow nearest resize; same as saved reference"
    path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    Path(report["raw_logits_f32le"]).unlink()
    report_path = path.resolve()
    print(f"{report_path}: changed restored pixels={changed}")


if __name__ == "__main__":
    main(Path(sys.argv[1]))
