#!/usr/bin/env python3
"""Compare pinned COO demo, evaluation, and M4C feature input paths."""

import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image

from probe import array_sha256, prepare, sha256


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--pages", nargs="*", default=("02", "07"))
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    pages = []
    for stem in args.pages:
        path = args.source_dir / f"{stem}.jpg"
        image = Image.open(path)
        arrays = {}
        descriptions = {}
        for mode in ("eval_pil", "feature_cv2", "demo_pil_800"):
            tensor, sizes = prepare(image, mode)
            array = tensor.numpy()
            arrays[mode] = array
            np.save(args.output_dir / f"{stem}-{mode}.npy", array)
            descriptions[mode] = {"shape": list(array.shape), "size": sizes, "sha256": array_sha256(array)}
        a, b = arrays["eval_pil"], arrays["feature_cv2"]
        difference = np.abs(a - b)
        pages.append({"file": path.name, "image_sha256": sha256(path), "inputs": descriptions,
                      "eval_vs_feature": {"same_shape": a.shape == b.shape,
                                          "equal_values": bool(np.array_equal(a, b)),
                                          "changed_values": int(np.count_nonzero(difference)),
                                          "mean_abs_difference": float(difference.mean()),
                                          "max_abs_difference": float(difference.max())}})
    (args.output_dir / "comparison.json").write_text(json.dumps({"pages": pages}, indent=2))


if __name__ == "__main__":
    main()
