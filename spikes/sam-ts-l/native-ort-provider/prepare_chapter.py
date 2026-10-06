#!/usr/bin/env python3
"""Save Pillow-prepared 1024² float inputs for the pinned chapter GPU probe."""
import argparse
import hashlib
import json
import sys
from pathlib import Path
import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from run_real_pages import prepare


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("source_dir", type=Path)
    parser.add_argument("output_dir", type=Path)
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    sources = sorted(args.source_dir.glob("*.jpg"), key=lambda path: int(path.stem))
    rows = []
    for source in sources:
        chw, resized, original = prepare(Image.open(source))
        output = args.output_dir / f"{source.stem}.npy"
        np.save(output, chw[None])
        rows.append({"page": source.stem, "prepared": str(output.resolve()),
                     "resized_wh": list(resized), "original_wh": list(original),
                     "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest()})
    (args.output_dir / "manifest.json").write_text(json.dumps(rows, indent=2) + "\n")
    print(f"saved {len(rows)} inputs to {args.output_dir}")


if __name__ == "__main__":
    main()
