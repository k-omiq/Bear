#!/usr/bin/env python3
"""Run the pinned full PyTorch mask model on an image.

The saved FP32 CPU reference and prepared input, when used, come from
export_mask.py --phase reference. Run different devices as separate processes
for comparable RSS high-water marks. Devices are explicitly selected;
unavailable accelerators fail instead of silently running on CPU.
This does not measure an ONNX Runtime execution provider.

Example, from this directory:
  artifacts/venv/bin/python probe_torch.py --list-backends
  artifacts/venv/bin/python probe_torch.py --device cpu --input page.png
  artifacts/venv/bin/python probe_torch.py --device mps --input page.png
  artifacts/venv/bin/python probe_torch.py --device cuda --cuda-index 0 --input page.png
  artifacts/venv/bin/python probe_torch.py --device cpu --input page.png --page-dir saved-reference
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import sys
import threading
import time
from pathlib import Path

# A successful MPS run must not silently execute unsupported operators on CPU.
# PyTorch reads this setting when its MPS backend initializes, so set it before
# importing torch (including through export_mask), even for an availability query.
os.environ["PYTORCH_ENABLE_MPS_FALLBACK"] = "0"

import numpy as np
import torch

from PIL import Image

from export_mask import (CHECKPOINT_SHA256, HF_REVISION, HI_SAM_REVISION,
                         differences, load_model, prepare, restore, sha256)


def backend_availability() -> dict[str, dict]:
    """Report actual device availability without loading the 1.36 GB model."""
    mps_backend = getattr(torch.backends, "mps", None)
    mps_built = bool(mps_backend and mps_backend.is_built())
    mps_available = bool(mps_backend and mps_backend.is_available())
    cuda_built = torch.version.cuda is not None
    cuda_available = bool(torch.cuda.is_available())
    cuda_devices = []
    if cuda_available:
        for index in range(torch.cuda.device_count()):
            properties = torch.cuda.get_device_properties(index)
            cuda_devices.append({"index": index, "name": properties.name,
                                 "total_memory_bytes": int(properties.total_memory)})
    return {
        "cpu": {"available": True, "reason": None},
        "mps": {"available": mps_available,
                "reason": None if mps_available else
                ("PyTorch was built without MPS" if not mps_built else
                 "no usable Apple Metal device is available")},
        "cuda": {"available": cuda_available and bool(cuda_devices),
                 "reason": None if cuda_available and cuda_devices else
                 ("PyTorch was built without CUDA" if not cuda_built else
                  "no usable NVIDIA CUDA device is available"),
                 "devices": cuda_devices},
    }


def select_device(requested: str, cuda_index: int, availability: dict[str, dict]) -> torch.device:
    status = availability[requested]
    if not status["available"]:
        raise RuntimeError(f"PyTorch {requested} unavailable: {status['reason']}")
    if requested == "cuda":
        if cuda_index < 0 or cuda_index >= len(status["devices"]):
            raise ValueError(f"CUDA device index {cuda_index} unavailable; "
                             f"available indices: {[d['index'] for d in status['devices']]}")
        return torch.device("cuda", cuda_index)
    return torch.device(requested)


def synchronize(device: torch.device) -> None:
    if device.type == "mps":
        torch.mps.synchronize()
    elif device.type == "cuda":
        torch.cuda.synchronize(device)


def cuda_memory(device: torch.device) -> dict[str, int] | None:
    if device.type != "cuda":
        return None
    return {
        "allocated_bytes": int(torch.cuda.memory_allocated(device)),
        "reserved_bytes": int(torch.cuda.memory_reserved(device)),
        "max_allocated_bytes": int(torch.cuda.max_memory_allocated(device)),
        "max_reserved_bytes": int(torch.cuda.max_memory_reserved(device)),
    }


def peak_process_rss_bytes() -> int:
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes

        class ProcessMemoryCountersEx(ctypes.Structure):
            _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
                        ("PeakWorkingSetSize", ctypes.c_size_t),
                        ("WorkingSetSize", ctypes.c_size_t),
                        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                        ("PagefileUsage", ctypes.c_size_t),
                        ("PeakPagefileUsage", ctypes.c_size_t),
                        ("PrivateUsage", ctypes.c_size_t)]

        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        psapi = ctypes.WinDLL("psapi", use_last_error=True)
        kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE,
                                               ctypes.POINTER(ProcessMemoryCountersEx),
                                               wintypes.DWORD]
        psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
        counters = ProcessMemoryCountersEx()
        counters.cb = ctypes.sizeof(counters)
        if not psapi.GetProcessMemoryInfo(kernel32.GetCurrentProcess(),
                                         ctypes.byref(counters), counters.cb):
            raise ctypes.WinError(ctypes.get_last_error())
        return int(counters.PeakWorkingSetSize)

    import resource

    value = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return int(value if sys.platform == "darwin" else value * 1024)


def mps_memory() -> dict[str, int] | None:
    if not torch.backends.mps.is_available():
        return None
    names = ("current_allocated_memory", "driver_allocated_memory",
             "recommended_max_memory")
    result = {}
    for name in names:
        function = getattr(torch.mps, name, None)
        if function is not None:
            try:
                result[name + "_bytes"] = int(function())
            except RuntimeError:
                pass
    return result or None


class MpsSampler:
    """Approximate transient MPS peaks; polling cannot guarantee the true peak."""

    def __init__(self, interval_seconds: float = 0.02):
        self.interval_seconds = interval_seconds
        self.stop_event = threading.Event()
        self.samples = 0
        self.sampled_max_bytes: dict[str, int] = {}
        self.thread: threading.Thread | None = None

    def sample(self) -> None:
        memory = mps_memory()
        if memory:
            self.samples += 1
            for name, value in memory.items():
                self.sampled_max_bytes[name] = max(self.sampled_max_bytes.get(name, 0), value)

    def start(self) -> None:
        self.sample()

        def poll() -> None:
            while not self.stop_event.wait(self.interval_seconds):
                self.sample()

        self.thread = threading.Thread(target=poll, name="mps-memory-sampler", daemon=True)
        self.thread.start()

    def stop(self) -> None:
        self.stop_event.set()
        if self.thread is not None:
            self.thread.join()
        self.sample()


def check_saved_page(page_dir: Path, weights: Path, input_path: Path | None) -> tuple[np.ndarray, np.ndarray,
                                                              np.ndarray, tuple[int, int],
                                                              tuple[int, int], dict, Path]:
    manifest = json.loads((page_dir / "manifest.json").read_text())
    if manifest.get("model") != "mayocream/koharu-text-sam-ts-l":
        raise ValueError("saved page is for another model")
    if manifest.get("revision") != HF_REVISION or manifest.get("hi_sam_revision") != HI_SAM_REVISION:
        raise ValueError("saved page has a different model/source revision")
    if manifest.get("checkpoint", {}).get("sha256") != CHECKPOINT_SHA256:
        raise ValueError("saved page has a different checkpoint")
    if sha256(weights) != CHECKPOINT_SHA256:
        raise ValueError("checkpoint SHA-256 mismatch")
    config = manifest.get("configuration", {})
    if (config.get("model_type") != "vit_l" or config.get("canvas") != 1024 or
            config.get("batch") != 1 or config.get("attn_layers") != 1 or
            config.get("prompt_len") != 12 or config.get("hier_det") is not False or
            config.get("dtype") != "float32"):
        raise ValueError("saved page has an unexpected model configuration")
    image_info = manifest["input"]
    resized_wh = tuple(image_info["resized_wh"])
    original_wh = tuple(image_info["original_wh"])
    prepared = np.load(page_dir / "prepared_rgb_f32.npy", allow_pickle=False)
    reference_logits = np.load(page_dir / "reference_logits_f32.npy", allow_pickle=False)
    reference_mask = np.load(page_dir / "reference_restored_mask_u8.npy", allow_pickle=False)
    if prepared.dtype != np.float32 or prepared.shape != (1, 3, 1024, 1024):
        raise ValueError(f"unexpected prepared tensor: {prepared.dtype} {prepared.shape}")
    if reference_logits.dtype != np.float32 or reference_logits.shape != (1, 1, 1024, 1024):
        raise ValueError(f"unexpected reference logits: {reference_logits.dtype} {reference_logits.shape}")
    if reference_mask.dtype != np.uint8 or reference_mask.shape != original_wh[::-1]:
        raise ValueError(f"unexpected reference mask: {reference_mask.dtype} {reference_mask.shape}")
    if np.count_nonzero(reference_mask != restore(reference_logits, resized_wh, original_wh)):
        raise ValueError("saved mask does not match its saved logits and dimensions")
    source = (input_path or Path(image_info["file"])).resolve()
    if sha256(source) != image_info["sha256"]:
        raise ValueError(f"input image SHA-256 differs from saved reference: {source}")
    with Image.open(source) as image:
        fresh, fresh_resized, fresh_original = prepare(image)
    if fresh_resized != resized_wh or fresh_original != original_wh:
        raise ValueError("input image dimensions differ from saved reference")
    if not np.array_equal(fresh.unsqueeze(0).numpy(), prepared):
        raise ValueError("input image preprocessing differs from saved reference")
    return prepared, reference_logits, reference_mask, resized_wh, original_wh, manifest, source


def run(args: argparse.Namespace) -> tuple[dict, np.ndarray]:
    availability = backend_availability()
    device = select_device(args.device, args.cuda_index, availability)
    torch.set_num_threads(args.torch_threads)
    weights = args.weights.resolve()
    page_dir = args.page_dir.resolve() if args.page_dir else None
    if page_dir is not None:
        prepared, reference_logits, reference_mask, resized_wh, original_wh, manifest, input_path = (
            check_saved_page(page_dir, weights, args.input))
    else:
        if args.input is None:
            raise ValueError("--input is required when --page-dir is omitted")
        input_path = args.input.resolve()
        with Image.open(input_path) as image:
            tensor, resized_wh, original_wh = prepare(image)
        prepared = tensor.unsqueeze(0).contiguous().numpy()
        reference_logits = reference_mask = manifest = None
    sampler = MpsSampler() if args.device == "mps" else None
    if sampler:
        sampler.start()
    try:
        load_started = time.perf_counter()
        model, _, _, source = load_model(args.hi_sam_root, weights)
        model_load_cpu_ms = (time.perf_counter() - load_started) * 1000
        memory_after_cpu_load = mps_memory() if sampler else None
        transfer_started = time.perf_counter()
        model.to(device).eval()
        prepared_tensor = torch.from_numpy(prepared[0].copy()).to(device)
        synchronize(device)
        device_transfer_ms = (time.perf_counter() - transfer_started) * 1000
        memory_after_transfer = mps_memory() if sampler else None
        cuda_after_transfer = cuda_memory(device)
        rss_after_transfer = peak_process_rss_bytes()

        def infer() -> np.ndarray:
            with torch.inference_mode():
                result = model([{"image": prepared_tensor,
                                 "original_size": (1024, 1024)}], multimask_output=False)
            synchronize(device)
            logits = result[3].detach().cpu().numpy()
            synchronize(device)
            return logits

        warmup_ms = []
        for _ in range(args.warmups):
            started = time.perf_counter()
            output = infer()
            warmup_ms.append((time.perf_counter() - started) * 1000)
            del output
        latency_ms = []
        last_logits = None
        for _ in range(args.repeats):
            started = time.perf_counter()
            last_logits = infer()
            latency_ms.append((time.perf_counter() - started) * 1000)
        if last_logits is None:
            raise RuntimeError("at least one timed repeat is required")
        memory_after_inference = mps_memory() if sampler else None
        cuda_after_inference = cuda_memory(device)
        rss_after_inference = peak_process_rss_bytes()
    finally:
        if sampler:
            sampler.stop()

    restored = restore(last_logits, resized_wh, original_wh)
    parity = None
    if reference_logits is not None and reference_mask is not None:
        parity = {
            "logits": differences(reference_logits, last_logits),
            "changed_canvas_signs": int(np.count_nonzero((reference_logits > 0) != (last_logits > 0))),
            "changed_restored_pixels": int(np.count_nonzero(reference_mask != restored)),
            "exact_restored_mask_parity": bool(np.array_equal(reference_mask, restored)),
            "reference_mask_pixels": int(np.count_nonzero(reference_mask)),
            "output_mask_pixels": int(np.count_nonzero(restored)),
        }
    report = {
        "description": "Pinned full-checkpoint PyTorch mask inference",
        "status": "completed",
        "backend": "pytorch",
        "selected_device": str(device),
        "backend_availability": availability,
        "environment": {
            "platform": platform.platform(), "machine": platform.machine(),
            "python": sys.version, "torch": torch.__version__,
            "device_requested": args.device, "cuda_index_requested": args.cuda_index,
            "mps_available": availability["mps"]["available"],
            "cuda_available": availability["cuda"]["available"],
            "mps_fallback_enabled": False, "torch_threads": args.torch_threads,
        },
        "input": {
            "page_dir": str(page_dir) if page_dir else None,
            "prepared_sha256": sha256(page_dir / "prepared_rgb_f32.npy") if page_dir else None,
            "reference_logits_sha256": sha256(page_dir / "reference_logits_f32.npy") if page_dir else None,
            "source": manifest["input"] if manifest else
                      {"file": str(input_path), "sha256": sha256(input_path)},
            "resolved_input": str(input_path),
            "preprocessing_exact_reference_match": True if page_dir else None,
            "resized_wh": list(resized_wh),
            "original_wh": list(original_wh),
        },
        "model": {"checkpoint_sha256": CHECKPOINT_SHA256, "source": source},
        "timing_ms": {
            "model_construct_and_checkpoint_load_on_cpu": model_load_cpu_ms,
            "model_and_input_transfer_to_device": device_transfer_ms,
            "warmup_inference": warmup_ms, "timed_inference": latency_ms,
            "timed_inference_median": float(np.median(latency_ms)),
        },
        "memory": {
            "peak_process_rss_bytes": rss_after_inference,
            "process_rss_high_water_after_device_transfer_bytes": rss_after_transfer,
            "mps_after_cpu_model_load": memory_after_cpu_load,
            "mps_after_device_transfer": memory_after_transfer,
            "mps_after_inference": memory_after_inference,
            "mps_sampled_max_bytes": sampler.sampled_max_bytes if sampler else None,
            "mps_samples": sampler.samples if sampler else 0,
            "cuda_after_device_transfer": cuda_after_transfer,
            "cuda_after_inference": cuda_after_inference,
            "notes": "RSS is process high-water; MPS current/driver values are samples, "
                     "including a 20 ms poll during execution, not guaranteed exact peaks. "
                     "CUDA maximums are PyTorch allocator peaks, not total GPU use. "
                     "On Apple silicon, GPU allocation draws from unified memory and is not additive to RSS.",
        },
        "parity": parity,
    }
    return report, restored


def main() -> None:
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list-backends", action="store_true",
                        help="print JSON availability for CPU, Apple MPS and NVIDIA CUDA, then exit")
    parser.add_argument("--device", choices=("cpu", "mps", "cuda"), default="cpu")
    parser.add_argument("--cuda-index", type=int, default=0)
    parser.add_argument("--page-dir", type=Path, default=None,
                        help="optional saved reference directory for strict input and output parity checks")
    parser.add_argument("--input", type=Path, default=None,
                        help="source image; defaults to saved manifest path when --page-dir is provided")
    parser.add_argument("--weights", type=Path, default=root / "artifacts/model.safetensors")
    parser.add_argument("--hi-sam-root", type=Path, default=root / "artifacts/Hi-SAM")
    parser.add_argument("--output", type=Path, default=None)
    parser.add_argument("--output-mask", type=Path, default=None,
                        help="PNG mask path; defaults beside the JSON report")
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--torch-threads", type=int, default=4)
    args = parser.parse_args()
    if args.list_backends:
        print(json.dumps({"backend": "pytorch", "platform": platform.platform(),
                          "torch": torch.__version__, "backends": backend_availability()}, indent=2))
        return
    if args.warmups < 0 or args.repeats < 1 or args.torch_threads < 1:
        parser.error("--warmups must be nonnegative; --repeats and --torch-threads must be positive")
    if args.cuda_index < 0:
        parser.error("--cuda-index must be nonnegative")
    if args.input is None and args.page_dir is None:
        parser.error("--input is required unless --page-dir supplies a saved source path")
    result, mask = run(args)
    suffix = f"cuda-{args.cuda_index}" if args.device == "cuda" else args.device
    base = args.page_dir if args.page_dir else args.input.parent
    default_name = (f"torch_{suffix}_probe.json" if args.page_dir else
                    f"{args.input.stem}_torch_{suffix}.json")
    output = args.output or base / default_name
    output_mask = args.output_mask or output.with_name(output.stem + "_mask.png")
    source_path = Path(result["input"]["resolved_input"])
    if output.resolve() == source_path or output_mask.resolve() == source_path:
        parser.error("output paths must differ from the source image")
    if output.resolve() == output_mask.resolve():
        parser.error("--output and --output-mask must be different paths")
    output.parent.mkdir(parents=True, exist_ok=True)
    output_mask.parent.mkdir(parents=True, exist_ok=True)
    Image.fromarray(mask).save(output_mask)
    result["output"] = {"mask_png": str(output_mask.resolve()),
                        "mask_sha256": sha256(output_mask), "json": str(output.resolve())}
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"output": result["output"], "selected_device": result["selected_device"],
                      "timing_ms": result["timing_ms"], "parity": result["parity"]}, indent=2))


if __name__ == "__main__":
    main()
