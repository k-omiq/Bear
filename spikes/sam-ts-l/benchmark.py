#!/usr/bin/env python3
"""Benchmark the proven, mask-only SAM-TS-L ONNX export on local ORT providers.

Run only after reviewing export_mask.py's FP32 CPU parity.json and confirming
the result with --parity-confirmed. This tool is conversion evidence, not a
desktop runtime dependency. Each stage/provider uses a fresh worker process;
"cold" means its first InferenceSession, not an uncached filesystem read.
"""

from __future__ import annotations

import argparse
import gc
import hashlib
import json
import platform
import subprocess
import sys
import tempfile
import time
from collections import Counter
from pathlib import Path

GRAPH_FILES = {
    "encoder": "koharu_samts_encoder.onnx",
    "head": "koharu_samts_text_head.onnx",
}
PROVIDER_NAMES = {
    "cpu": "CPUExecutionProvider",
    "coreml": "CoreMLExecutionProvider",
    "cuda": "CUDAExecutionProvider",
    "directml": "DmlExecutionProvider",
    "webgpu": "WebGpuExecutionProvider",
    "migraphx": "MIGraphXExecutionProvider",
}
PROVIDER_PLATFORMS = {
    "cpu": ("darwin", "linux", "win32"),
    "coreml": ("darwin",),
    "cuda": ("linux", "win32"),
    "directml": ("win32",),
    "webgpu": ("darwin", "linux", "win32"),
    "migraphx": ("linux",),
}
STAGES = ("encoder", "head", "chain")


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_parity(proof: Path) -> dict:
    parity_path = proof / "parity.json"
    parity = json.loads(parity_path.read_text())
    required = ("encoder_embedding", "text_head_same_embedding_logits",
                "source_vs_onnx_logits", "source_vs_onnx_restored_mask_pixels")
    missing = [name for name in required if name not in parity]
    if missing or "CPUExecutionProvider" not in parity.get("providers", []):
        raise ValueError(f"CPU parity evidence is incomplete: missing={missing}, "
                         f"providers={parity.get('providers')}")
    return {name: parity[name] for name in required}


def graph_packages(proof: Path) -> dict:
    import onnx

    packages = {}
    for stage, filename in GRAPH_FILES.items():
        graph_path = proof / filename
        graph = onnx.load(str(graph_path), load_external_data=False)
        locations = {item.value for tensor in graph.graph.initializer
                     for item in tensor.external_data if item.key == "location"}
        files = [graph_path]
        for location in sorted(locations):
            relative = Path(location)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError(f"unsafe external data path: {location}")
            files.append(proof / relative)
        entries = [{"file": str(path.relative_to(proof)), "bytes": path.stat().st_size}
                   for path in files]
        packages[stage] = {"files": entries,
                           "total_bytes": sum(item["bytes"] for item in entries)}
    return packages


def verify_graph_packages(proof: Path, packages: dict) -> dict:
    manifest_path = proof / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    recorded_graphs = manifest.get("graphs", {})
    for stage, actual_package in packages.items():
        manifest_key = "text_head" if stage == "head" else stage
        recorded_files = recorded_graphs.get(manifest_key, {}).get("package")
        if not isinstance(recorded_files, list):
            raise ValueError(f"export manifest is missing {stage} graph package")
        expected = {entry["file"]: entry for entry in recorded_files}
        actual = {entry["file"]: entry for entry in actual_package["files"]}
        if set(expected) != set(actual):
            raise ValueError(f"{stage} package file mismatch: manifest={sorted(expected)}, "
                             f"current={sorted(actual)}")
        for filename, entry in actual.items():
            recorded = expected[filename]
            if entry["bytes"] != recorded["bytes"]:
                raise ValueError(f"{stage}/{filename} size mismatch: "
                                 f"manifest={recorded['bytes']}, current={entry['bytes']}")
            current_hash = sha256(proof / filename)
            if current_hash != recorded["sha256"]:
                raise ValueError(f"{stage}/{filename} SHA-256 mismatch: "
                                 f"manifest={recorded['sha256']}, current={current_hash}")
            entry["sha256"] = current_hash
    return {"file": str(manifest_path), "sha256": sha256(manifest_path),
            "graph_packages_verified": True}


def peak_rss_bytes() -> int:
    """Process peak resident working set, not GPU or system memory."""
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes

        class ProcessMemoryCounters(ctypes.Structure):
            _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
                        ("PeakWorkingSetSize", ctypes.c_size_t),
                        ("WorkingSetSize", ctypes.c_size_t),
                        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                        ("PagefileUsage", ctypes.c_size_t),
                        ("PeakPagefileUsage", ctypes.c_size_t)]

        counters = ProcessMemoryCounters()
        counters.cb = ctypes.sizeof(counters)
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        psapi = ctypes.WinDLL("psapi", use_last_error=True)
        kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE,
                                                ctypes.POINTER(ProcessMemoryCounters),
                                                wintypes.DWORD]
        psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
        if not psapi.GetProcessMemoryInfo(kernel32.GetCurrentProcess(),
                                          ctypes.byref(counters), counters.cb):
            raise ctypes.WinError(ctypes.get_last_error())
        return int(counters.PeakWorkingSetSize)
    import resource

    value = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return int(value if sys.platform == "darwin" else value * 1024)


def provider_skip_reason(provider: str, available: list[str], system: str) -> str | None:
    if system not in PROVIDER_PLATFORMS[provider]:
        return f"{provider} probe is not applicable on {system}"
    if PROVIDER_NAMES[provider] not in available:
        return (f"{PROVIDER_NAMES[provider]} is absent from this ONNX Runtime build; "
                f"available={available}")
    return None


def discover_webgpu(ort):
    """Return built-in/plugin mode, plugin devices, and an unavailable reason."""
    if "WebGpuExecutionProvider" in ort.get_available_providers():
        return "built_in", None, None
    try:
        import onnxruntime_ep_webgpu as webgpu_ep
    except ImportError:
        return None, None, ("native WebGPU EP is absent; install a compatible "
                            "onnxruntime-ep-webgpu plugin with onnxruntime")
    if not all(hasattr(ort, name) for name in
               ("register_execution_provider_library", "get_ep_devices")):
        return None, None, "this ONNX Runtime build lacks the plugin EP registration API"
    try:
        ort.register_execution_provider_library("samts_webgpu", webgpu_ep.get_library_path())
        devices = [device for device in ort.get_ep_devices()
                   if device.ep_name == webgpu_ep.get_ep_name()]
    except Exception as error:
        return None, None, f"WebGPU plugin registration failed: {type(error).__name__}: {error}"
    if not devices:
        return None, None, "WebGPU plugin registered, but no WebGPU EP device was discovered"
    return "plugin", devices, None


def provider_configuration(provider: str, args: argparse.Namespace) -> dict:
    if provider == "coreml":
        return {"ModelFormat": args.coreml_model_format,
                "MLComputeUnits": args.coreml_compute_units,
                "RequireStaticInputShapes": str(args.coreml_static_shapes)}
    return {}


def session_providers(provider: str, args: argparse.Namespace, webgpu_mode=None):
    if provider == "webgpu" and webgpu_mode == "plugin":
        # The plugin EP is attached to SessionOptions through OrtEpDevice.
        return None
    requested = PROVIDER_NAMES[provider]
    if provider == "cpu":
        return [requested]
    configured = ((requested, provider_configuration(provider, args))
                  if provider == "coreml" else requested)
    return [configured, "CPUExecutionProvider"]


def session_options(ort, threads: int, provider: str, *, profiling: bool = False,
                    profile_prefix: str | None = None, webgpu_devices=None):
    options = ort.SessionOptions()
    options.intra_op_num_threads = threads
    options.inter_op_num_threads = 1
    if provider == "directml":
        # Required by the DirectML EP, even when CPU is the fallback EP.
        options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
        options.enable_mem_pattern = False
    if provider == "webgpu" and webgpu_devices is not None:
        options.add_provider_for_devices(webgpu_devices, {})
    if profiling:
        options.enable_profiling = True
        options.profile_file_prefix = profile_prefix
    return options


def array_differences(reference, actual) -> dict:
    import numpy as np

    if reference.shape != actual.shape:
        return {"error": "shape mismatch", "reference_shape": list(reference.shape),
                "actual_shape": list(actual.shape)}
    if not np.isfinite(actual).all():
        return {"error": "provider output contains nonfinite values"}
    delta = np.abs(reference.astype(np.float64) - actual.astype(np.float64))
    return {"shape": list(actual.shape), "max_abs": float(delta.max()),
            "mean_abs": float(delta.mean()),
            "rmse": float(np.sqrt(np.mean(delta * delta))),
            "changed_elements": int(np.count_nonzero(delta))}


def compare_with_reference(stage: str, output, proof: Path) -> dict:
    import numpy as np

    reference_file = {"encoder": "encoder_embedding_f32.npy",
                      "head": "head_logits_f32.npy",
                      "chain": "reference_logits_f32.npy"}[stage]
    reference = np.load(proof / reference_file)
    result = {"reference_file": reference_file,
              "output_difference": array_differences(reference, output)}
    if stage != "encoder" and "error" not in result["output_difference"]:
        result["canvas_threshold_sign_changes"] = int(np.count_nonzero(
            (reference > 0) != (output > 0)))
        # The source reference mask is saved after crop and nearest resize.
        # Recover the exact crop from the export manifest's input metadata.
        from PIL import Image

        manifest = json.loads((proof / "manifest.json").read_text())
        width, height = manifest["input"]["resized_wh"]
        original_wh = tuple(manifest["input"]["original_wh"])
        mask = Image.fromarray((output[0, 0, :height, :width] > 0).astype(np.uint8) * 255)
        restored = np.asarray(mask.resize(original_wh, Image.Resampling.NEAREST))
        reference_mask = np.load(proof / "reference_restored_mask_u8.npy")
        changed_pixels = int(np.count_nonzero(restored != reference_mask))
        result["restored_mask_changed_pixels_vs_full_pytorch"] = changed_pixels
        result["exact_restored_mask_parity_vs_full_pytorch"] = changed_pixels == 0
    return result


def profile_assignments(path: Path) -> dict:
    events = json.loads(path.read_text())
    counts = Counter()
    for event in events:
        if event.get("cat") != "Node":
            continue
        provider = event.get("args", {}).get("provider")
        if provider:
            counts[provider] += 1
    return {"node_events_by_provider": dict(sorted(counts.items())),
            "node_events_without_provider": sum(
                event.get("cat") == "Node" and not event.get("args", {}).get("provider")
                for event in events)}


def worker(args: argparse.Namespace) -> None:
    import numpy as np
    import onnxruntime as ort

    proof = args.proof.resolve()
    requested = PROVIDER_NAMES[args.provider]
    webgpu_mode, webgpu_devices = None, None
    if args.provider == "webgpu":
        webgpu_mode, webgpu_devices, unavailable = discover_webgpu(ort)
        if unavailable:
            raise RuntimeError(unavailable)
    providers = session_providers(args.provider, args, webgpu_mode)
    options = session_options(ort, args.threads, args.provider,
                              webgpu_devices=webgpu_devices)
    sessions = {}
    load_times = {}
    graph_names = ("encoder", "head") if args.stage == "chain" else (args.stage,)
    for graph_name in graph_names:
        started = time.perf_counter()
        session = ort.InferenceSession(str(proof / GRAPH_FILES[graph_name]),
                                       sess_options=options, providers=providers)
        load_times[graph_name] = (time.perf_counter() - started) * 1000
        sessions[graph_name] = session

    prepared = np.load(proof / "prepared_rgb_f32.npy") if args.stage != "head" else None
    embedding = np.load(proof / "encoder_embedding_f32.npy") if args.stage == "head" else None
    if prepared is not None and (prepared.dtype != np.float32 or
                                 prepared.shape != (1, 3, 1024, 1024)):
        raise ValueError(f"unexpected prepared input: {prepared.dtype} {prepared.shape}")
    if embedding is not None and embedding.dtype != np.float32:
        raise ValueError(f"unexpected embedding dtype: {embedding.dtype}")

    def infer(active: dict):
        value = embedding
        if args.stage in ("encoder", "chain"):
            encoder = active["encoder"]
            value = encoder.run(None, {encoder.get_inputs()[0].name: prepared})[0]
        if args.stage in ("head", "chain"):
            head = active["head"]
            value = head.run(None, {head.get_inputs()[0].name: value})[0]
        return value

    for _ in range(args.warmups):
        infer(sessions)
    latency_ms = []
    output = None
    for _ in range(args.repeats):
        started = time.perf_counter()
        output = infer(sessions)
        latency_ms.append((time.perf_counter() - started) * 1000)
    measured_peak_rss = peak_rss_bytes()
    active_providers = {name: session.get_providers() for name, session in sessions.items()}
    parity = compare_with_reference(args.stage, output, proof)
    profile_head_input = embedding
    if args.stage == "chain":
        encoder = sessions["encoder"]
        profile_head_input = encoder.run(None, {encoder.get_inputs()[0].name: prepared})[0]
    sessions.clear()
    gc.collect()

    # Profiling uses a separate session and run, so its instrumentation does
    # not enter the latency samples. Inspect actual node assignments because
    # get_providers() alone cannot reveal CoreML -> CPU node fallback.
    assignments = {}
    for graph_name in graph_names:
        try:
            with tempfile.TemporaryDirectory(prefix="samts-ort-profile-") as temporary:
                profiled_options = session_options(
                    ort, args.threads, args.provider, profiling=True,
                    profile_prefix=str(Path(temporary) / graph_name),
                    webgpu_devices=webgpu_devices)
                profiled = ort.InferenceSession(str(proof / GRAPH_FILES[graph_name]),
                                                sess_options=profiled_options,
                                                providers=providers)
                if graph_name == "encoder":
                    profiled.run(None, {profiled.get_inputs()[0].name: prepared})
                else:
                    profiled.run(None, {profiled.get_inputs()[0].name: profile_head_input})
                profile_path = Path(profiled.end_profiling())
                assignments[graph_name] = profile_assignments(profile_path)
                del profiled
        except Exception as error:
            assignments[graph_name] = {"error": f"{type(error).__name__}: {error}"}

    result = {
        "status": "completed",
        "provider_requested": requested,
        "provider_configuration": provider_configuration(args.provider, args),
        "webgpu_registration_mode": webgpu_mode,
        "providers_active_by_graph": active_providers,
        "stage": args.stage,
        "cold_session_load_ms_by_graph": load_times,
        "warm_latency_ms": latency_ms,
        "warm_latency_median_ms": float(np.median(latency_ms)),
        "peak_process_rss_bytes": measured_peak_rss,
        "profile_assignments": assignments,
        "pytorch_fixture_comparison": parity,
    }
    result["requested_provider_has_profiled_nodes_by_graph"] = {
        name: (counts.get("node_events_by_provider", {}).get(requested, 0) > 0
               if "error" not in counts else None)
        for name, counts in assignments.items()
    }
    result["requested_provider_has_profiled_nodes_in_every_graph"] = all(
        value is True for value in
        result["requested_provider_has_profiled_nodes_by_graph"].values())
    write_json(args.worker_result, result)


def run(args: argparse.Namespace) -> None:
    import onnxruntime as ort

    if not args.parity_confirmed:
        raise ValueError("Review CPU parity.json first, then pass --parity-confirmed")
    proof = args.proof.resolve()
    parity = read_parity(proof)
    available = ort.get_available_providers()
    requested = list(dict.fromkeys(args.provider or PROVIDER_NAMES))
    webgpu_mode, _, webgpu_unavailable = (discover_webgpu(ort) if "webgpu" in requested
                                          and sys.platform in PROVIDER_PLATFORMS["webgpu"]
                                          else (None, None, None))
    packages = graph_packages(proof)
    manifest_verification = verify_graph_packages(proof, packages)
    result = {
        "description": "Static batch-one 1024px FP32 ONNX Runtime mask-only inference",
        "environment": {"python": sys.version, "platform": platform.platform(),
                        "onnxruntime": ort.__version__, "available_providers": available,
                        "threads": args.threads},
        "parity_evidence": {"file": str(proof / "parity.json"), "cpu_comparison": parity,
                            "confirmed_by_invoker": True},
        "export_manifest": manifest_verification,
        "graph_packages": packages,
        "measurement": {"warmups": args.warmups, "repeats": args.repeats,
                        "timeout_seconds_per_stage": args.timeout_seconds,
                        "cold_definition": "first InferenceSession in a fresh worker process; OS file cache uncontrolled",
                        "peak_rss_definition": "worker process peak resident set/working set, including Python and ORT; excludes GPU/device memory; sampled before parity/profiling"},
        "runs": [],
    }
    with tempfile.TemporaryDirectory(prefix="samts-benchmark-") as temporary:
        for provider in requested:
            if provider == "webgpu" and webgpu_unavailable:
                reason = webgpu_unavailable
            elif provider == "webgpu" and webgpu_mode == "plugin":
                reason = None
            else:
                reason = provider_skip_reason(provider, available, sys.platform)
            if reason:
                for stage in (args.stage or STAGES):
                    result["runs"].append({"provider_requested": PROVIDER_NAMES[provider],
                                           "stage": stage, "status": "skipped",
                                           "reason": reason})
                write_json(args.output, result)
                continue
            for stage in (args.stage or STAGES):
                worker_result = Path(temporary) / f"{provider}-{stage}.json"
                command = [sys.executable, str(Path(__file__).resolve()),
                           "--worker", "--proof", str(proof), "--provider", provider,
                           "--stage", stage, "--threads", str(args.threads),
                           "--warmups", str(args.warmups), "--repeats", str(args.repeats),
                           "--coreml-model-format", args.coreml_model_format,
                           "--coreml-compute-units", args.coreml_compute_units,
                           "--coreml-static-shapes", str(args.coreml_static_shapes),
                           "--worker-result", str(worker_result)]
                try:
                    completed = subprocess.run(command, capture_output=True, text=True,
                                               timeout=args.timeout_seconds)
                    if completed.returncode == 0:
                        result["runs"].append(json.loads(worker_result.read_text()))
                    else:
                        result["runs"].append({"provider_requested": PROVIDER_NAMES[provider],
                                               "stage": stage,
                                               "status": "error",
                                               "error": f"worker exited with code {completed.returncode}",
                                               "stderr_tail": completed.stderr[-4000:],
                                               "stdout_tail": completed.stdout[-4000:],
                                               "exit_code": completed.returncode})
                except subprocess.TimeoutExpired as error:
                    stderr = error.stderr or b""
                    if isinstance(stderr, bytes):
                        stderr = stderr.decode("utf-8", errors="replace")
                    result["runs"].append({"provider_requested": PROVIDER_NAMES[provider],
                                           "stage": stage,
                                           "status": "error",
                                           "error": f"worker exceeded {args.timeout_seconds} seconds",
                                           "stderr_tail": stderr[-4000:],
                                           "timeout_seconds": args.timeout_seconds})
                write_json(args.output, result)
    print(json.dumps({"output": str(args.output), "runs": result["runs"]}, indent=2))


def main() -> None:
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--proof", type=Path, default=root / "artifacts/proof")
    parser.add_argument("--output", type=Path, default=root / "artifacts/proof/benchmark.json")
    parser.add_argument("--parity-confirmed", action="store_true",
                        help="assert that you reviewed CPU reference parity.json")
    parser.add_argument("--provider", choices=tuple(PROVIDER_NAMES), action="append",
                        help="repeat to select probes; default: all known providers, with unavailable probes recorded as skipped")
    parser.add_argument("--stage", choices=STAGES, action="append",
                        help="repeat to probe selected stages; default: encoder, head, chain")
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--timeout-seconds", type=int, default=900)
    parser.add_argument("--coreml-model-format", choices=("NeuralNetwork", "MLProgram"),
                        default="NeuralNetwork")
    parser.add_argument("--coreml-compute-units",
                        choices=("ALL", "CPUOnly", "CPUAndGPU", "CPUAndNeuralEngine"),
                        default="ALL")
    parser.add_argument("--coreml-static-shapes", choices=(0, 1), type=int, default=0)
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--worker-result", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.threads < 1 or args.warmups < 0 or args.repeats < 1 or args.timeout_seconds < 1:
        parser.error("threads/repeats/timeout must be positive and warmups nonnegative")
    if args.worker:
        if args.stage is None or len(args.stage) != 1 or args.worker_result is None or args.provider is None or len(args.provider) != 1:
            parser.error("internal worker needs one provider, stage, and result path")
        args.provider = args.provider[0]
        args.stage = args.stage[0]
        worker(args)
    else:
        args.output = args.output.resolve()
        args.output.parent.mkdir(parents=True, exist_ok=True)
        run(args)


if __name__ == "__main__":
    main()
