#!/usr/bin/env python3
"""Run the pinned SAM-TS-L mask-only ONNX export on one image.

Choose a local ONNX Runtime provider explicitly. CPU fallback nodes are reported;
non-CPU selections fail when profiling shows no work on the selected provider.
This is an experimental script, not a packaged desktop inference backend.
"""

from __future__ import annotations

import argparse
import gc
import json
import platform
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image

from benchmark import (GRAPH_FILES, PROVIDER_NAMES, discover_webgpu, graph_packages,
                       PROVIDER_PLATFORMS, peak_rss_bytes, profile_assignments, provider_configuration,
                       provider_skip_reason, session_options, session_providers, sha256,
                       verify_graph_packages)
from run_real_pages import differences, prepare, restore


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def backend_availability() -> dict:
    """Distinguish installed EPs from verified devices without loading graphs."""
    installed = ort.get_available_providers()
    result = {}
    for key, provider_name in PROVIDER_NAMES.items():
        if sys.platform not in PROVIDER_PLATFORMS[key]:
            result[key] = {"available": False, "installed": provider_name in installed,
                           "reason": f"not supported on {sys.platform}"}
        elif key == "cpu":
            result[key] = {"available": provider_name in installed,
                           "installed": provider_name in installed,
                           "reason": None if provider_name in installed else "CPU EP absent"}
        elif key == "webgpu":
            mode, devices, error = discover_webgpu(ort)
            if error:
                result[key] = {"available": False, "installed": False,
                               "reason": error}
            else:
                # Built-in EPs may be listed without a working adapter. The
                # plugin discovery path explicitly returns matching devices.
                if mode == "built_in" and hasattr(ort, "get_ep_devices"):
                    devices = [device for device in ort.get_ep_devices()
                               if device.ep_name == provider_name]
                count = len(devices) if devices is not None else None
                result[key] = {"available": bool(count) if count is not None else None,
                               "installed": True, "registration_mode": mode,
                               "device_count": count,
                               "reason": None if count else "device availability unverified"
                               if count is None else "no matching WebGPU device found"}
        elif provider_name not in installed:
            result[key] = {"available": False, "installed": False,
                           "reason": "EP absent from this ONNX Runtime build"}
        else:
            result[key] = {"available": None, "installed": True,
                           "reason": "EP is installed; device and graph execution are unverified"}
    return result


def reference_directory(root: Path, page: str | None) -> Path:
    """Accept either a page directory or the parent containing page-NN folders."""
    if page is None:
        return root
    candidate = root / f"page-{page}"
    if not candidate.is_dir():
        raise FileNotFoundError(f"reference page directory is missing: {candidate}")
    return candidate


def compare_reference(directory: Path, batch: np.ndarray, logits: np.ndarray,
                      mask: np.ndarray) -> dict:
    prepared = np.load(directory / "prepared_rgb_f32.npy")
    reference_logits = np.load(directory / "reference_logits_f32.npy")
    reference_mask = np.load(directory / "reference_restored_mask_u8.npy")
    if prepared.shape != batch.shape or not np.array_equal(prepared, batch):
        raise ValueError(f"prepared input differs from the reference in {directory}; "
                         "check that --input and --reference-page refer to the same image")
    if reference_mask.shape != mask.shape:
        raise ValueError(f"reference mask shape {reference_mask.shape} differs from "
                         f"output shape {mask.shape}")
    return {
        "directory": str(directory),
        "prepared_input_exact": True,
        "logits": differences(reference_logits, logits),
        "changed_canvas_signs": int(np.count_nonzero((reference_logits > 0) != (logits > 0))),
        "changed_restored_pixels": int(np.count_nonzero(reference_mask != mask)),
        "exact_restored_mask_parity": bool(np.array_equal(reference_mask, mask)),
    }


def main() -> None:
    started = time.perf_counter()
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list-backends", action="store_true",
                        help="print JSON EP/device availability without loading graphs")
    parser.add_argument("--input", type=Path, help="any Pillow-readable image")
    parser.add_argument("--output-dir", type=Path,
                        help="directory for mask.png and run.json")
    parser.add_argument("--graph-dir", type=Path, default=root / "artifacts/proof")
    parser.add_argument("--provider", choices=tuple(PROVIDER_NAMES))
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--coreml-model-format", choices=("NeuralNetwork", "MLProgram"),
                        default="NeuralNetwork")
    parser.add_argument("--coreml-compute-units",
                        choices=("ALL", "CPUOnly", "CPUAndGPU", "CPUAndNeuralEngine"),
                        default="ALL")
    parser.add_argument("--coreml-static-shapes", choices=(0, 1), type=int, default=0)
    parser.add_argument("--reference-dir", type=Path,
                        help="optional PyTorch reference page directory or its parent")
    parser.add_argument("--reference-page", help="page ID, when reference-dir is a parent")
    args = parser.parse_args()
    if args.list_backends:
        print(json.dumps({"backend": "onnxruntime", "platform": platform.platform(),
                          "onnxruntime": ort.__version__,
                          "backends": backend_availability()}, indent=2))
        return
    if args.input is None or args.output_dir is None or args.provider is None:
        parser.error("--input, --output-dir, and --provider are required for inference")
    if args.threads < 1:
        parser.error("--threads must be positive")
    if args.reference_page and not args.reference_dir:
        parser.error("--reference-page requires --reference-dir")

    source = args.input.resolve(strict=True)
    graph_dir = args.graph_dir.resolve(strict=True)
    destination = args.output_dir.resolve()
    if source in ((destination / "mask.png").resolve(),
                  (destination / "run.json").resolve()):
        parser.error("--output-dir would overwrite the input image")
    preflight_started = time.perf_counter()
    packages = graph_packages(graph_dir)
    manifest = verify_graph_packages(graph_dir, packages)
    preflight_ms = (time.perf_counter() - preflight_started) * 1000

    requested = PROVIDER_NAMES[args.provider]
    available = ort.get_available_providers()
    webgpu_mode, webgpu_devices = None, None
    # A plugin WebGPU EP is absent from get_available_providers until registration.
    check_available = [requested] if args.provider == "webgpu" else available
    unavailable = provider_skip_reason(args.provider, check_available, sys.platform)
    if unavailable:
        raise RuntimeError(unavailable)
    if args.provider == "webgpu":
        webgpu_mode, webgpu_devices, unavailable = discover_webgpu(ort)
        if unavailable:
            raise RuntimeError(unavailable)
    providers = session_providers(args.provider, args, webgpu_mode)

    prepare_started = time.perf_counter()
    with Image.open(source) as image:
        prepared, resized_size, original_size = prepare(image)
    batch = prepared[np.newaxis, ...]
    prepare_ms = (time.perf_counter() - prepare_started) * 1000

    sessions = {}
    load_ms = {}
    active = {}
    for stage, graph_name in GRAPH_FILES.items():
        options = session_options(ort, args.threads, args.provider,
                                  webgpu_devices=webgpu_devices)
        load_started = time.perf_counter()
        session = ort.InferenceSession(str(graph_dir / graph_name),
                                       sess_options=options, providers=providers)
        load_ms[stage] = (time.perf_counter() - load_started) * 1000
        active[stage] = session.get_providers()
        if requested not in active[stage]:
            raise RuntimeError(f"{stage} session omitted requested {requested}; "
                               f"active providers: {active[stage]}")
        sessions[stage] = session

    encoder_started = time.perf_counter()
    embedding = sessions["encoder"].run(None, {"prepared_rgb": batch})[0]
    encoder_ms = (time.perf_counter() - encoder_started) * 1000
    if not np.isfinite(embedding).all():
        raise ValueError("encoder embedding contains nonfinite values")
    head_started = time.perf_counter()
    logits = sessions["head"].run(None, {"embedding": embedding})[0]
    head_ms = (time.perf_counter() - head_started) * 1000
    if not np.isfinite(logits).all():
        raise ValueError("head logits contain nonfinite values")
    restore_started = time.perf_counter()
    mask = restore(logits, resized_size, original_size)
    restore_ms = (time.perf_counter() - restore_started) * 1000
    measured_peak_rss = peak_rss_bytes()
    del sessions
    gc.collect()

    parity = None
    if args.reference_dir:
        directory = reference_directory(args.reference_dir.resolve(strict=True),
                                        args.reference_page)
        parity = compare_reference(directory, batch, logits, mask)

    # Separate profiling sessions keep the reported inference latency free of
    # profiler overhead. Count actual node assignments, not just EP presence.
    profile_started = time.perf_counter()
    assignments = {}
    for stage, input_array in (("encoder", batch), ("head", embedding)):
        with tempfile.TemporaryDirectory(prefix="samts-infer-profile-") as temporary:
            options = session_options(ort, args.threads, args.provider,
                                      profiling=True,
                                      profile_prefix=str(Path(temporary) / stage),
                                      webgpu_devices=webgpu_devices)
            session = ort.InferenceSession(str(graph_dir / GRAPH_FILES[stage]),
                                           sess_options=options, providers=providers)
            if requested not in session.get_providers():
                raise RuntimeError(f"profiled {stage} session omitted {requested}")
            session.run(None, {session.get_inputs()[0].name: input_array})
            assignments[stage] = profile_assignments(Path(session.end_profiling()))
            del session
    profile_ms = (time.perf_counter() - profile_started) * 1000
    requested_nodes_by_graph = {
        stage: value["node_events_by_provider"].get(requested, 0)
        for stage, value in assignments.items()
    }
    requested_nodes = sum(requested_nodes_by_graph.values())
    cpu_fallback_nodes = (sum(value["node_events_by_provider"].get(
        "CPUExecutionProvider", 0) for value in assignments.values())
                          if args.provider != "cpu" else 0)

    result = {
        "status": "ok" if args.provider == "cpu" or all(requested_nodes_by_graph.values())
                  else "provider_not_used",
        "source": str(source),
        "source_sha256": sha256(source),
        "output_mask": str(destination / "mask.png"),
        "provider": {
            "selected": args.provider,
            "requested": requested,
            "options": provider_configuration(args.provider, args),
            "webgpu_registration_mode": webgpu_mode,
            "webgpu_plugin_device_count": len(webgpu_devices) if webgpu_devices is not None else None,
            "available": ort.get_available_providers(),
            "active_by_graph": active,
            "profile_assignments": assignments,
            "requested_node_events_by_graph": requested_nodes_by_graph,
            "requested_node_events": requested_nodes,
            "cpu_fallback_node_events": cpu_fallback_nodes,
        },
        "environment": {"platform": platform.platform(), "python": sys.version,
                        "onnxruntime": ort.__version__, "threads": args.threads},
        "graphs": {"directory": str(graph_dir), "packages": packages,
                   "manifest_verification": manifest},
        "image": {"original_wh": list(original_size), "resized_wh": list(resized_size),
                  "mask_pixels": int(np.count_nonzero(mask)),
                  "prepared_bytes": int(batch.nbytes),
                  "embedding_bytes": int(embedding.nbytes),
                  "logits_bytes": int(logits.nbytes)},
        "timing_ms": {"graph_hash_preflight": preflight_ms, "prepare": prepare_ms,
                      "session_load": load_ms, "encoder_run": encoder_ms,
                      "head_run": head_ms, "restore": restore_ms,
                      "profile": profile_ms},
        "peak_process_rss_bytes": measured_peak_rss,
        "peak_process_rss_bytes_after_profile": peak_rss_bytes(),
        "measurement": {
            "session_load": "first InferenceSession in this process; OS file cache uncontrolled",
            "inference": "single timed run per graph; separate profiling runs excluded",
            "peak_process_rss": "process peak resident/working set, excludes GPU/device memory",
            "cpu_fallback": "node events assigned to CPU in separate profile runs",
        },
    }
    if parity is not None:
        result["reference_parity"] = parity
    result["timing_ms"]["end_to_end_before_write"] = (time.perf_counter() - started) * 1000
    destination.mkdir(parents=True, exist_ok=True)
    if result["status"] == "ok":
        Image.fromarray(mask).save(destination / "mask.png")
    write_json(destination / "run.json", result)
    if result["status"] != "ok":
        raise RuntimeError(f"selected {requested} executed zero profiled nodes on at least "
                           f"one graph ({requested_nodes_by_graph}); "
                           f"CPU fallback nodes={cpu_fallback_nodes}; details: {destination / 'run.json'}")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
