#!/usr/bin/env python3
"""Compatibility entry point for the original Apple MPS benchmark.

Use probe_torch.py to choose CPU, Apple MPS, or NVIDIA CUDA explicitly.
"""

import sys
from pathlib import Path

from probe_torch import main


if __name__ == "__main__":
    if "--device" not in sys.argv and "--list-backends" not in sys.argv:
        sys.argv.extend(("--device", "mps"))
    if ("--page-dir" not in sys.argv and "--input" not in sys.argv and
            "--list-backends" not in sys.argv):
        default_page = Path(__file__).resolve().parent / "artifacts/examples/apocalypse-109/page-07"
        sys.argv.extend(("--page-dir", str(default_page)))
    main()
