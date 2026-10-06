"""Hayai on a Modal L4: CUDA EP vs CPU, fp32/fp16/int8. Scratch bench, not app code."""
import modal

app = modal.App("mc-hayai-cudabench")
vol = modal.Volume.from_name("mc-hayai-cudabench", create_if_missing=True)
image = (modal.Image.debian_slim(python_version="3.12")
         .pip_install("onnxruntime-gpu==1.23.2", "numpy", "pillow", "tokenizers",
                      "torch==2.8.0", extra_index_url="https://download.pytorch.org/whl/cu128")
         .add_local_file("run_core.py", "/root/run_core.py"))


@app.function(gpu="L4", image=image, volumes={"/data": vol}, timeout=1500, memory=8192, cpu=4)
def bench():
    import json, time, os, sys
    import torch  # noqa: F401  CUDA and cuDNN libraries for onnxruntime-gpu
    import onnxruntime as ort
    sys.path.insert(0, "/root")
    import run_core
    rows = json.load(open("/data/torch_results.json"))
    out = []
    for variant in ("fp32", "fp16", "int8"):
        for provider in ("CUDAExecutionProvider", "CPUExecutionProvider"):
            if variant != "fp32" and provider == "CPUExecutionProvider":
                continue
            t0 = time.time()
            try:
                r = run_core.Reader(f"/data/models/{variant}", provider, "/data/tokenizer.json")
            except Exception as e:  # noqa: BLE001
                out.append(f"{variant} {provider}: open failed {str(e)[:200]}"); continue
            build = time.time() - t0
            from PIL import Image
            r.read(Image.open("/data/crops/" + rows[0]["name"]))
            t1 = time.time(); same = 0
            for row in rows:
                text, _ = r.read(Image.open("/data/crops/" + row["name"]))
                same += text == row["text"]
            per = (time.time() - t1) / len(rows) * 1000
            placed = r.vision.get_providers()
            out.append(f"{variant} {provider}: build {build:.1f}s per-crop {per:.0f}ms identical {same}/{len(rows)} providers {placed}")
            print(out[-1], flush=True)
    return out


@app.local_entrypoint()
def main():
    for line in bench.remote():
        print(line)
