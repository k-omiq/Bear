"""Scratch Modal app: SAM-TS-L on CUDA and the WebGPU plugin (Vulkan) on one L4.
Reads the seeded graphs from the live weights volume, read only.
"""
import json, os, pathlib, shutil, subprocess, sys
import modal

CTX = pathlib.Path(__file__).parent / "ctx"
image = (
    modal.Image.debian_slim(python_version="3.12")
    .apt_install("libvulkan1", "vulkan-tools")
    .pip_install("onnxruntime-gpu[cuda,cudnn]==1.28.0", "onnx==1.23.2", "numpy<2.3", "pillow", "nvidia-ml-py")
    .env({"NVIDIA_DRIVER_CAPABILITIES": "all"})
    .add_local_dir(CTX, "/ctx")
)
app = modal.App("sam-gpu-bench-scratch", image=image)
weights = modal.Volume.from_name("mc-ab3xgz-weights").read_only()


@app.function(gpu="L4", cpu=8, memory=32768, timeout=1800, volumes={"/vol": weights})
def bench():
    sys.path.insert(0, "/ctx")
    import numpy as np
    import onnx
    import sam_mem_lib as lib

    report = {}
    report["vulkaninfo"] = subprocess.run(["vulkaninfo", "--summary"], capture_output=True, text=True).stdout[-1500:] \
        or subprocess.run(["vulkaninfo", "--summary"], capture_output=True, text=True).stderr[-800:]
    report["nvidia_smi"] = subprocess.run(["nvidia-smi", "--query-gpu=name,driver_version,memory.total",
                                           "--format=csv,noheader"], capture_output=True, text=True).stdout.strip()
    work = pathlib.Path("/tmp/w"); (work / "out").mkdir(parents=True)
    src = pathlib.Path("/vol/analysis/sam-ts-l")
    shutil.copy(src / "koharu_samts_encoder.onnx", work / "enc.onnx")
    shutil.copy(src / "koharu_samts_text_head.onnx", work / "head.onnx")
    subprocess.run([sys.executable, "/ctx/chunk_attn.py", work / "enc.onnx", work / "enc_s16.onnx", "16"], check=True)
    (work / "ext").mkdir()
    m = onnx.load(str(work / "enc_s16.onnx"))
    onnx.save(m, str(work / "ext/enc.onnx"), save_as_external_data=True, all_tensors_to_one_file=True,
              location="enc.onnx.data", size_threshold=1024)
    del m
    inputs = []
    for n in ("010", "115", "175"):
        p = work / f"{n}.npy"; np.save(p, lib.prepare(f"/ctx/{n}.jpg")); inputs.append(str(p))
    variants = [("cpu_orig", "enc.onnx"), ("cpu_lean", "ext/enc.onnx"),
                ("cuda_orig_tf32", "enc.onnx"), ("cuda_orig", "enc.onnx"), ("cuda_lean", "ext/enc.onnx"),
                ("cuda_lean_tf32", "ext/enc.onnx"), ("wgpu_lean", "ext/enc.onnx")]
    runs = {}
    for name, graph in variants:
        r = subprocess.run([sys.executable, "/ctx/one.py", name, str(work / graph), str(work / "head.onnx"),
                            str(work / "out"), *inputs], capture_output=True, text=True, timeout=900)
        line = [l for l in r.stdout.splitlines() if l.startswith("{")]
        runs[name] = json.loads(line[-1]) if line else {"error": (r.stderr or r.stdout)[-1200:]}
        print(name, runs[name], flush=True)
    ref = lambda i: np.load(work / "out" / f"cpu_orig_{i}.npy")
    for name, run in runs.items():
        if "error" in run or name == "cpu_orig":
            continue
        diffs = []
        for i in range(3):
            a, b = ref(i), np.load(work / "out" / f"{name}_{i}.npy")
            diffs.append({"max_abs": float(np.abs(a - b).max()), "mask_px": int(((a > 0) != (b > 0)).sum())})
        run["vs_cpu"] = diffs
    report["runs"] = runs
    return report


@app.local_entrypoint()
def main():
    out = pathlib.Path(__file__).parent / "result.json"
    out.write_text(json.dumps(bench.remote(), indent=1))
    print(out.read_text())
