"""Export only the SAM-TS-L text head and fingerprint every initializer.

Used to compare the head export across macOS, Windows and Linux.
"""
import argparse
import hashlib
import json
import platform
import sys
from pathlib import Path

import numpy as np
import torch


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--repo", type=Path, required=True)
    p.add_argument("--hi-sam-root", type=Path, required=True)
    p.add_argument("--weights", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    sys.path.insert(0, str(a.repo / "spikes/sam-ts-l"))
    import export_mask as em
    import onnx
    from onnx import numpy_helper

    torch.set_num_threads(4)
    model, *_ = em.load_model(a.hi_sam_root, a.weights.resolve())
    head = em.TextHead(model).eval()
    a.out.mkdir(parents=True, exist_ok=True)
    path = a.out / "koharu_samts_text_head.onnx"
    torch.backends.mha.set_fastpath_enabled(False)
    with torch.no_grad():
        em.export_graph(head, torch.zeros(1, 256, 64, 64), path, "embedding", "high_res_logits")
    graph = onnx.load(str(path))
    inits = {}
    for t in graph.graph.initializer:
        inits[t.name] = hashlib.sha256(numpy_helper.to_array(t).tobytes()).hexdigest()[:16]
    pe = head.image_pe.numpy()
    report = {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "torch": torch.__version__,
        "torch_cpu_capability": torch.backends.cpu.get_cpu_capability(),
        "head_sha256": em.sha256(path),
        "head_bytes": path.stat().st_size,
        "image_pe_sha256": hashlib.sha256(pe.tobytes()).hexdigest(),
        "graph_without_initializers_sha256": None,
        "initializers": inits,
    }
    del graph.graph.initializer[:]
    report["graph_without_initializers_sha256"] = hashlib.sha256(graph.SerializeToString()).hexdigest()
    np.save(a.out / "image_pe.npy", pe)
    (a.out / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True))
    print(json.dumps({k: v for k, v in report.items() if k != "initializers"}, indent=2))


if __name__ == "__main__":
    main()
