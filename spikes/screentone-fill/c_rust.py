"""Candidate S5: the Rust port (`crates/cleaner-core/src/engines/screentone.rs`), for parity with c09_d2.

Build first: cargo build -p cleaner-core --release --example screentone_fill
"""
import os
import subprocess
import tempfile

import numpy as np

BIN = os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "examples", "screentone_fill")


def fill(gray, hole, avoid=None):
    avoid = np.zeros_like(hole) if avoid is None else avoid
    h, w = gray.shape
    with tempfile.TemporaryDirectory() as d:
        p = {k: os.path.join(d, k + ".bin") for k in ("gray", "hole", "avoid", "out", "used")}
        gray.astype(np.uint8).tofile(p["gray"])
        hole.astype(np.uint8).tofile(p["hole"])
        avoid.astype(np.uint8).tofile(p["avoid"])
        subprocess.run([BIN, str(w), str(h), p["gray"], p["hole"], p["avoid"], p["out"], p["used"]], check=True)
        out = np.fromfile(p["out"], np.uint8).reshape(h, w)
        used = np.fromfile(p["used"], np.uint8).reshape(h, w).astype(bool)
    return out, used
