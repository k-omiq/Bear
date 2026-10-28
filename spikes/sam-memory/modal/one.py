"""Run one SAM-TS-L variant in its own process on Linux. Prints one JSON line.
usage: one.py <variant> <encoder> <head> <out_dir> <input.npy>...
"""
import json, os, resource, sys, threading, time
import numpy as np
import onnxruntime as ort

variant, enc, head, out, *inputs = sys.argv[1:]
backend = variant.split("_")[0]
peak_vram = [0]
stop = threading.Event()


def poll_vram():
    try:
        import pynvml
        pynvml.nvmlInit()
        h = pynvml.nvmlDeviceGetHandleByIndex(0)
        base = pynvml.nvmlDeviceGetMemoryInfo(h).used
        while not stop.is_set():
            peak_vram[0] = max(peak_vram[0], pynvml.nvmlDeviceGetMemoryInfo(h).used - base)
            time.sleep(0.02)
    except Exception as e:  # no GPU visible
        peak_vram[0] = -1


threading.Thread(target=poll_vram, daemon=True).start()
if backend == "cuda":
    ort.preload_dlls()


def session(path):
    so = ort.SessionOptions()
    so.intra_op_num_threads = 4
    if backend == "cpu":
        lean = "lean" in variant
        so.enable_cpu_mem_arena = not lean
        so.enable_mem_pattern = not lean
        return ort.InferenceSession(path, so, providers=["CPUExecutionProvider"])
    if backend == "cuda":
        opts = {"use_tf32": "1" if "tf32" in variant else "0"}
        if "lean" in variant:
            opts.update(arena_extend_strategy="kSameAsRequested", cudnn_conv_algo_search="HEURISTIC")
        s = ort.InferenceSession(path, so, providers=[("CUDAExecutionProvider", opts), "CPUExecutionProvider"])
        assert s.get_providers()[0] == "CUDAExecutionProvider", s.get_providers()
        return s
    if backend == "wgpu":
        so.add_session_config_entry("session.disable_cpu_ep_fallback", "1")
        devs = [d for d in ort.get_ep_devices() if d.ep_name == "WebGpuExecutionProvider"]
        assert devs, [(d.ep_name, str(d.device.type)) for d in ort.get_ep_devices()]
        so.add_provider_for_devices(devs, {})
        return ort.InferenceSession(path, so)
    raise SystemExit(variant)


if backend == "wgpu":
    ort.register_execution_provider_library("WebGpuPlugin", "/ctx/libonnxruntime_providers_webgpu.so")
t = time.time()
e, h = session(enc), session(head)
load = time.time() - t
times = []
for i, f in enumerate(inputs):
    x = np.load(f)
    t = time.time()
    emb = e.run(None, {"prepared_rgb": x})[0]
    logits = h.run(None, {"embedding": emb})[0]
    times.append(round(time.time() - t, 2))
    np.save(f"{out}/{variant}_{i}.npy", logits.reshape(1024, 1024))
stop.set()
time.sleep(0.05)
print(json.dumps({"variant": variant, "load_s": round(load, 1), "page_s": times,
                  "host_peak_gib": round(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 2**20, 2),
                  "vram_peak_gib": round(peak_vram[0] / 2**30, 2) if peak_vram[0] >= 0 else None}))
