#!/usr/bin/env python3
"""Benchmark pinned ONNX Runtime CPU or WebGPU graphs on saved page-07 input.

Run in a fresh process for a useful process RSS comparison with
probe_torch_mps.py --device cpu. Both use the same prepared tensor and saved
full-PyTorch FP32 reference. This script keeps PyTorch out of the ORT process.

Example, from this directory:
  artifacts/venv/bin/python probe_ort_page.py --provider cpu
  artifacts/venv/bin/python probe_ort_page.py --provider webgpu
"""

from __future__ import annotations

import argparse
import gc
import hashlib
import json
import platform
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image

from benchmark import (PROVIDER_NAMES, discover_webgpu, profile_assignments,
                       session_options, session_providers)


CHECKPOINT_SHA256 = "bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45"
HF_REVISION = "5dd97423e0fbf2404264979136d47e8101144046"
HI_SAM_REVISION = "69009434d4dba5541f228d8f5acb0754c333d417"
GRAPH_FILES = {"encoder": "koharu_samts_encoder.onnx",
               "text_head": "koharu_samts_text_head.onnx"}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


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


def restore(logits: np.ndarray, resized_wh: tuple[int, int],
            original_wh: tuple[int, int]) -> np.ndarray:
    width, height = resized_wh
    mask = logits[0, 0, :height, :width] > 0
    binary = Image.fromarray(mask.astype(np.uint8) * 255, mode="L")
    return np.asarray(binary.resize(original_wh, Image.Resampling.NEAREST), dtype=np.uint8)


def differences(a: np.ndarray, b: np.ndarray) -> dict:
    if a.shape != b.shape:
        raise ValueError(f"logit shape mismatch: reference={a.shape}, output={b.shape}")
    if not np.isfinite(b).all():
        raise ValueError("provider logits contain nonfinite values")
    delta = np.abs(a.astype(np.float64) - b.astype(np.float64))
    return {"shape": list(a.shape), "max_abs": float(delta.max()),
            "mean_abs": float(delta.mean()),
            "rmse": float(np.sqrt(np.mean(delta * delta))),
            "changed_elements": int(np.count_nonzero(delta))}


def verify_graphs(graph_dir: Path) -> dict:
    manifest_path = graph_dir / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    if manifest.get("model") != "mayocream/koharu-text-sam-ts-l":
        raise ValueError("ONNX manifest is for another model")
    if manifest.get("revision") != HF_REVISION or manifest.get("hi_sam_revision") != HI_SAM_REVISION:
        raise ValueError("ONNX manifest has a different model/source revision")
    if manifest.get("checkpoint", {}).get("sha256") != CHECKPOINT_SHA256:
        raise ValueError("ONNX manifest has a different checkpoint")
    packages = {}
    for stage, graph_file in GRAPH_FILES.items():
        recorded = manifest.get("graphs", {}).get(stage, {}).get("package")
        if not isinstance(recorded, list) or not recorded:
            raise ValueError(f"ONNX manifest has no package for {stage}")
        files = []
        names = set()
        for entry in recorded:
            relative = Path(entry["file"])
            if relative.is_absolute() or ".." in relative.parts or relative.as_posix() in names:
                raise ValueError(f"unsafe or duplicate ONNX package path: {relative}")
            names.add(relative.as_posix())
            path = graph_dir / relative
            if not path.is_file() or path.stat().st_size != entry["bytes"]:
                raise ValueError(f"missing or changed ONNX package file: {path}")
            actual_hash = sha256(path)
            if actual_hash != entry["sha256"]:
                raise ValueError(f"ONNX package SHA-256 mismatch: {path}")
            files.append({"file": str(relative), "bytes": entry["bytes"],
                          "sha256": actual_hash})
        if graph_file not in names:
            raise ValueError(f"ONNX package for {stage} lacks {graph_file}")
        packages[stage] = files
    return {"manifest": str(manifest_path.resolve()),
            "manifest_sha256": sha256(manifest_path), "packages": packages}


def load_saved_page(page_dir: Path) -> tuple[np.ndarray, np.ndarray, np.ndarray,
                                                   tuple[int, int], tuple[int, int], dict]:
    manifest = json.loads((page_dir / "manifest.json").read_text())
    if manifest.get("model") != "mayocream/koharu-text-sam-ts-l":
        raise ValueError("saved page is for another model")
    if manifest.get("revision") != HF_REVISION or manifest.get("hi_sam_revision") != HI_SAM_REVISION:
        raise ValueError("saved page has a different model/source revision")
    if manifest.get("checkpoint", {}).get("sha256") != CHECKPOINT_SHA256:
        raise ValueError("saved page has a different checkpoint")
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
        raise ValueError("saved mask does not match saved logits and dimensions")
    return prepared, reference_logits, reference_mask, resized_wh, original_wh, manifest


def run(args: argparse.Namespace) -> dict:
    page_dir = args.page_dir.resolve()
    graph_dir = args.graph_dir.resolve()
    preflight_started = time.perf_counter()
    prepared, reference_logits, reference_mask, resized_wh, original_wh, page_manifest = (
        load_saved_page(page_dir))
    graph_verification = verify_graphs(graph_dir)
    preflight_ms = (time.perf_counter() - preflight_started) * 1000

    webgpu_mode, webgpu_devices = None, None
    if args.provider == "webgpu":
        webgpu_mode, webgpu_devices, unavailable = discover_webgpu(ort)
        if unavailable:
            raise RuntimeError(unavailable)
    providers = session_providers(args.provider, args, webgpu_mode)
    options = session_options(ort, args.threads, args.provider,
                              webgpu_devices=webgpu_devices)
    load_started = time.perf_counter()
    encoder = ort.InferenceSession(str(graph_dir / GRAPH_FILES["encoder"]),
                                   sess_options=options, providers=providers)
    encoder_load_ms = (time.perf_counter() - load_started) * 1000
    load_started = time.perf_counter()
    head = ort.InferenceSession(str(graph_dir / GRAPH_FILES["text_head"]),
                                sess_options=options, providers=providers)
    head_load_ms = (time.perf_counter() - load_started) * 1000
    active = {"encoder": encoder.get_providers(), "text_head": head.get_providers()}
    if args.provider == "cpu" and any(available != providers for available in active.values()):
        raise RuntimeError(f"unexpected ONNX Runtime providers: {active}")
    if len(encoder.get_inputs()) != 1 or len(head.get_inputs()) != 1:
        raise ValueError("unexpected graph input count")
    rss_after_session_load = peak_process_rss_bytes()

    def infer() -> tuple[np.ndarray, dict]:
        chain_started = time.perf_counter()
        embedding = encoder.run(None, {encoder.get_inputs()[0].name: prepared})[0]
        encoder_done = time.perf_counter()
        logits = head.run(None, {head.get_inputs()[0].name: embedding})[0]
        head_done = time.perf_counter()
        return logits, {"encoder": (encoder_done - chain_started) * 1000,
                        "head": (head_done - encoder_done) * 1000,
                        "chain": (head_done - chain_started) * 1000}

    warmup_ms = []
    for _ in range(args.warmups):
        _, timing = infer()
        warmup_ms.append(timing)
    latency_ms = []
    last_logits = None
    for _ in range(args.repeats):
        last_logits, timing = infer()
        latency_ms.append(timing)
    rss_after_inference = peak_process_rss_bytes()
    if last_logits is None:
        raise RuntimeError("at least one timed repeat is required")
    restored = restore(last_logits, resized_wh, original_wh)
    # The profiling pass has separate sessions and executes after latency/RSS
    # capture. Device registration and options match the timed sessions.
    del infer, encoder, head
    gc.collect()
    assignments = None
    if args.provider == "webgpu":
        assignments = {}
        try:
            with tempfile.TemporaryDirectory(prefix="samts-page-webgpu-profile-") as temporary:
                profile_embedding = None
                for stage in ("encoder", "text_head"):
                    profile_options = session_options(
                        ort, args.threads, args.provider, profiling=True,
                        profile_prefix=str(Path(temporary) / stage),
                        webgpu_devices=webgpu_devices)
                    session = ort.InferenceSession(str(graph_dir / GRAPH_FILES[stage]),
                                                   sess_options=profile_options,
                                                   providers=providers)
                    input_array = prepared if stage == "encoder" else profile_embedding
                    profile_output = session.run(None, {session.get_inputs()[0].name: input_array})[0]
                    if stage == "encoder":
                        profile_embedding = profile_output
                    profile_path = Path(session.end_profiling())
                    assignments[stage] = profile_assignments(profile_path)
                    del session, profile_output
                    gc.collect()
        except Exception as error:
            assignments["error"] = f"{type(error).__name__}: {error}"
    profiled_webgpu_nodes = None
    if assignments is not None:
        profiled_webgpu_nodes = {
            stage: assignments.get(stage, {}).get("node_events_by_provider", {}).get(
                PROVIDER_NAMES["webgpu"], 0) > 0
            for stage in GRAPH_FILES
        }
    return {
        "description": "Pinned ONNX Runtime encoder + text head on saved real comic input",
        "environment": {
            "platform": platform.platform(), "machine": platform.machine(),
            "python": sys.version, "onnxruntime": ort.__version__,
            "provider_requested": PROVIDER_NAMES[args.provider],
            "webgpu_registration_mode": webgpu_mode,
            "available_providers": ort.get_available_providers(),
            "active_providers": active,
            "intra_op_num_threads": options.intra_op_num_threads,
            "inter_op_num_threads": options.inter_op_num_threads,
        },
        "input": {
            "page_dir": str(page_dir), "prepared_sha256": sha256(page_dir / "prepared_rgb_f32.npy"),
            "reference_logits_sha256": sha256(page_dir / "reference_logits_f32.npy"),
            "source": page_manifest["input"], "resized_wh": list(resized_wh),
            "original_wh": list(original_wh),
        },
        "graphs": graph_verification,
        "profile_assignments": assignments,
        "requested_provider_has_profiled_nodes_by_graph": profiled_webgpu_nodes,
        "requested_provider_has_profiled_nodes_in_every_graph": (
            all(profiled_webgpu_nodes.values()) if profiled_webgpu_nodes is not None else None),
        "timing_ms": {
            "page_and_graph_hash_preflight": preflight_ms,
            "cold_encoder_session_load": encoder_load_ms,
            "cold_head_session_load": head_load_ms,
            "warmup_inference": warmup_ms, "timed_inference": latency_ms,
            "timed_median_encoder": float(np.median([x["encoder"] for x in latency_ms])),
            "timed_median_head": float(np.median([x["head"] for x in latency_ms])),
            "timed_median_chain": float(np.median([x["chain"] for x in latency_ms])),
        },
        "memory": {
            "peak_process_rss_bytes": rss_after_inference,
            "process_rss_high_water_after_session_load_bytes": rss_after_session_load,
            "notes": "Process high-water RSS was sampled before optional profiling; it includes "
                     "Python, ONNX Runtime, both timed sessions, and prior runs, but excludes "
                     "GPU/device allocations. "
                     "Windows uses PeakWorkingSetSize; macOS ru_maxrss is bytes; "
                     "Linux ru_maxrss KiB is converted to bytes.",
        },
        "parity": {
            "logits": differences(reference_logits, last_logits),
            "changed_canvas_signs": int(np.count_nonzero((reference_logits > 0) != (last_logits > 0))),
            "changed_restored_pixels": int(np.count_nonzero(reference_mask != restored)),
            "exact_restored_mask_parity": bool(np.array_equal(reference_mask, restored)),
            "reference_mask_pixels": int(np.count_nonzero(reference_mask)),
            "output_mask_pixels": int(np.count_nonzero(restored)),
        },
    }


def main() -> None:
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--page-dir", type=Path,
                        default=root / "artifacts/examples/apocalypse-109/page-07")
    parser.add_argument("--graph-dir", type=Path, default=root / "artifacts/proof")
    parser.add_argument("--output", type=Path, default=None)
    parser.add_argument("--provider", choices=("cpu", "webgpu"), default="cpu")
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--threads", type=int, default=4)
    args = parser.parse_args()
    if args.warmups < 0 or args.repeats < 1 or args.threads < 1:
        parser.error("--warmups must be nonnegative; --repeats and --threads must be positive")
    result = run(args)
    output = args.output or args.page_dir / f"ort_{args.provider}_probe.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"output": str(output.resolve()), "timing_ms": result["timing_ms"],
                      "memory": result["memory"], "parity": result["parity"]}, indent=2))


if __name__ == "__main__":
    main()
