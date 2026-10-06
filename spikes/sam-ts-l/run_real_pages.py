"""Run the pinned mask-only ONNX graphs on local pages for reference comparison."""

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
                       peak_rss_bytes as peak_process_rss_bytes, profile_assignments,
                       provider_configuration, provider_skip_reason, session_options,
                       session_providers, sha256, verify_graph_packages)

IMAGE_SIZE = 1024


def prepare(image: Image.Image) -> tuple[np.ndarray, tuple[int, int], tuple[int, int]]:
    """Pinned image preparation using only Pillow and NumPy."""
    original_size = image.size
    rgb = image.convert("RGB")
    scale = IMAGE_SIZE / max(rgb.size)
    resized_size = tuple(max(1, round(axis * scale)) for axis in rgb.size)
    resized = rgb.resize(resized_size, Image.Resampling.BILINEAR)
    canvas = Image.new("RGB", (IMAGE_SIZE, IMAGE_SIZE), (128, 128, 128))
    canvas.paste(resized, (0, 0))
    array = np.asarray(canvas, dtype=np.float32).copy()
    return np.ascontiguousarray(array.transpose(2, 0, 1)), resized_size, original_size


def restore(logits: np.ndarray, resized_size: tuple[int, int],
            original_size: tuple[int, int]) -> np.ndarray:
    """Threshold, crop, and nearest resize in the pinned reference order."""
    width, height = resized_size
    mask = (logits[0, 0, :height, :width] > 0).astype(np.uint8) * 255
    return np.asarray(Image.fromarray(mask).resize(original_size, Image.Resampling.NEAREST),
                      dtype=np.uint8)


def differences(reference: np.ndarray, actual: np.ndarray) -> dict:
    if reference.shape != actual.shape:
        return {"shape_a": list(reference.shape), "shape_b": list(actual.shape),
                "error": "shape mismatch"}
    if not np.isfinite(reference).all() or not np.isfinite(actual).all():
        raise ValueError("reference or provider output contains nonfinite values")
    delta = np.abs(reference.astype(np.float64) - actual.astype(np.float64))
    return {"shape": list(reference.shape), "max_abs": float(delta.max()),
            "mean_abs": float(delta.mean()),
            "rmse": float(np.sqrt(np.mean(delta * delta))),
            "changed_elements": int(np.count_nonzero(delta))}


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def main() -> None:
    process_started = time.perf_counter()
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--pages", nargs="+", default=["07", "08", "09"])
    parser.add_argument("--graph-dir", type=Path, default=root / "artifacts/proof")
    parser.add_argument("--provider", choices=tuple(PROVIDER_NAMES), default="cpu")
    parser.add_argument("--coreml-model-format", choices=("NeuralNetwork", "MLProgram"),
                        default="NeuralNetwork")
    parser.add_argument("--coreml-compute-units",
                        choices=("ALL", "CPUOnly", "CPUAndGPU", "CPUAndNeuralEngine"),
                        default="ALL")
    parser.add_argument("--coreml-static-shapes", choices=(0, 1), type=int, default=0)
    parser.add_argument("--reference-dir", type=Path,
                        help="directory containing saved PyTorch page references")
    parser.add_argument("--output-dir", type=Path,
                        help="generated ONNX results; non-CPU providers use separate directories")
    args = parser.parse_args()
    default_reference = root / "artifacts/examples/apocalypse-109"
    args.reference_dir = (args.reference_dir or
                          (args.output_dir if args.provider == "cpu" else None) or
                          default_reference).resolve()
    args.output_dir = (args.output_dir or
                       (default_reference if args.provider == "cpu" else
                        default_reference.with_name(
                            f"{default_reference.name}-{args.provider}"))).resolve()

    graph_dir = args.graph_dir.resolve()
    preflight_started = time.perf_counter()
    packages = graph_packages(graph_dir)
    manifest_verification = verify_graph_packages(graph_dir, packages)
    preflight_ms = (time.perf_counter() - preflight_started) * 1000
    webgpu_mode, webgpu_devices = None, None
    requested_provider = PROVIDER_NAMES[args.provider]
    available = ort.get_available_providers()
    if args.provider == "webgpu":
        # Plugin WebGPU need not appear in get_available_providers() before
        # registration, so check platform separately, then discover devices.
        unavailable = provider_skip_reason("webgpu", [requested_provider], sys.platform)
        if unavailable:
            raise RuntimeError(unavailable)
        webgpu_mode, webgpu_devices, unavailable = discover_webgpu(ort)
        if unavailable:
            raise RuntimeError(unavailable)
    else:
        unavailable = provider_skip_reason(args.provider, available, sys.platform)
        if unavailable:
            raise RuntimeError(unavailable)
    if args.provider == "cpu":
        # Preserve the previous CPU session configuration and output paths.
        options = ort.SessionOptions()
        options.intra_op_num_threads = 4
        providers = ["CPUExecutionProvider"]
    else:
        options = session_options(ort, 4, args.provider,
                                  webgpu_devices=webgpu_devices)
        providers = session_providers(args.provider, args, webgpu_mode)
    encoder_load_started = time.perf_counter()
    encoder = ort.InferenceSession(str(graph_dir / "koharu_samts_encoder.onnx"),
                                   sess_options=options, providers=providers)
    encoder_load_ms = (time.perf_counter() - encoder_load_started) * 1000
    head_load_started = time.perf_counter()
    head = ort.InferenceSession(str(graph_dir / "koharu_samts_text_head.onnx"),
                                sess_options=options, providers=providers)
    head_load_ms = (time.perf_counter() - head_load_started) * 1000

    summary = {
        "description": f"Real-page mask-only ONNX Runtime {args.provider} inference and parity",
        "provider": {
            "requested": requested_provider,
            "provider_options": provider_configuration(args.provider, args),
            "registration_mode": webgpu_mode,
            "plugin_device_count": len(webgpu_devices) if webgpu_devices is not None else None,
            "session_provider_specification": providers,
            "plugin_provider_options": {} if webgpu_mode == "plugin" else None,
            "session_execution_mode": str(options.execution_mode),
            "session_memory_pattern_enabled": options.enable_mem_pattern,
        },
        "reference_dir": str(args.reference_dir),
        "output_dir": str(args.output_dir),
        "environment": {
            "platform": platform.platform(),
            "python": sys.version,
            "onnxruntime": ort.__version__,
            "available_providers": ort.get_available_providers(),
            "active_providers_by_graph": {
                "encoder": encoder.get_providers(),
                "head": head.get_providers(),
            },
            "intra_op_num_threads": options.intra_op_num_threads,
            "inter_op_num_threads": options.inter_op_num_threads,
        },
        "graph_packages": packages,
        "graph_package_total_bytes": sum(package["total_bytes"] for package in packages.values()),
        "export_manifest": manifest_verification,
        "timing_ms": {
            "manifest_and_graph_hash_preflight": preflight_ms,
            "cold_encoder_session_load": encoder_load_ms,
            "cold_head_session_load": head_load_ms,
        },
        "measurement": {
            "cold_definition": "first InferenceSession in this process; OS file cache uncontrolled",
            "page_total_definition": "source open through parity and inference artifact writes for one page; excludes performance JSON write",
            "end_to_end_definition": "main() entry through all page processing, before separate profiling, aggregate JSON write, and stdout",
            "peak_rss_definition": "process peak resident/working set, including Python and ORT; excludes GPU/device memory; per-page values are cumulative and measured before any separate profiling run",
        },
        "pages": {},
    }
    for page in args.pages:
        page_started = time.perf_counter()
        source = args.source_dir / f"{page}.jpg"
        reference = args.reference_dir / f"page-{page}"
        output = args.output_dir / f"page-{page}"
        output.mkdir(parents=True, exist_ok=True)
        prepare_started = time.perf_counter()
        with Image.open(source) as image:
            prepared, resized_size, original_size = prepare(image)
        batch = prepared[np.newaxis, ...]
        prepare_ms = (time.perf_counter() - prepare_started) * 1000
        saved = np.load(reference / "prepared_rgb_f32.npy")
        if not np.array_equal(batch, saved):
            raise ValueError(f"prepared input changed for {source}")
        encoder_started = time.perf_counter()
        embedding = encoder.run(None, {"prepared_rgb": batch})[0]
        encoder_ms = (time.perf_counter() - encoder_started) * 1000
        if not np.isfinite(embedding).all():
            raise ValueError(f"nonfinite encoder embedding for {source}")
        head_started = time.perf_counter()
        logits = head.run(None, {"embedding": embedding})[0]
        head_ms = (time.perf_counter() - head_started) * 1000
        if not np.isfinite(logits).all():
            raise ValueError(f"nonfinite head logits for {source}")
        restore_started = time.perf_counter()
        mask = restore(logits, resized_size, original_size)
        restore_ms = (time.perf_counter() - restore_started) * 1000
        reference_logits = np.load(reference / "reference_logits_f32.npy")
        reference_mask = np.load(reference / "reference_restored_mask_u8.npy")
        np.save(output / "onnx_encoder_embedding_f32.npy", embedding)
        np.save(output / "onnx_head_logits_f32.npy", logits)
        np.save(output / "onnx_restored_mask_u8.npy", mask)
        Image.fromarray(mask).save(output / "onnx_restored_mask.png")
        result = {
            "source": str(source.resolve()),
            "source_sha256": sha256(source),
            "original_wh": list(original_size),
            "resized_wh": list(resized_size),
            "prepared_input_changed_values": 0,
            "logits": differences(reference_logits, logits),
            "changed_canvas_signs": int(np.count_nonzero((reference_logits > 0) != (logits > 0))),
            "changed_restored_pixels": int(np.count_nonzero(reference_mask != mask)),
            "exact_restored_mask_parity": bool(np.array_equal(reference_mask, mask)),
            "reference_mask_pixels": int(np.count_nonzero(reference_mask)),
            "onnx_mask_pixels": int(np.count_nonzero(mask)),
        }
        write_json(output / "onnx_parity.json", result)
        page_metrics = {
            "page": page,
            "provider_requested": PROVIDER_NAMES[args.provider],
            "source": str(source.resolve()),
            "input_sizes_bytes": {
                "source_jpeg": source.stat().st_size,
                "prepared_rgb_f32": batch.nbytes,
                "encoder_embedding_f32": embedding.nbytes,
                "head_logits_f32": logits.nbytes,
                "restored_mask_u8": mask.nbytes,
            },
            "timing_ms": {
                "prepare": prepare_ms,
                "encoder_run": encoder_ms,
                "head_run": head_ms,
                "restore": restore_ms,
            },
        }
        page_metrics["timing_ms"]["page_total"] = (time.perf_counter() - page_started) * 1000
        page_metrics["peak_process_rss_bytes"] = peak_process_rss_bytes()
        write_json(output / "onnx_performance.json", page_metrics)
        summary["pages"][page] = {"parity": result, "performance": page_metrics}
    summary["timing_ms"]["end_to_end"] = (time.perf_counter() - process_started) * 1000
    summary["peak_process_rss_bytes"] = peak_process_rss_bytes()
    if args.provider != "cpu":
        # Profile once per graph after timed pages, so instrumentation does
        # not affect their latency or cumulative process-RSS readings.
        del encoder, head
        gc.collect()
        profile_started = time.perf_counter()
        first_page = args.pages[0]
        profile_inputs = {
            "encoder": np.load(args.reference_dir / f"page-{first_page}" /
                               "prepared_rgb_f32.npy"),
            "head": np.load(args.output_dir / f"page-{first_page}" /
                            "onnx_encoder_embedding_f32.npy"),
        }
        assignments = {}
        for graph_name in GRAPH_FILES:
            try:
                with tempfile.TemporaryDirectory(prefix="samts-real-page-profile-") as temporary:
                    profile_options = session_options(
                        ort, 4, args.provider, profiling=True,
                        profile_prefix=str(Path(temporary) / graph_name),
                        webgpu_devices=webgpu_devices)
                    session = ort.InferenceSession(
                        str(graph_dir / GRAPH_FILES[graph_name]),
                        sess_options=profile_options, providers=providers)
                    session.run(None, {session.get_inputs()[0].name:
                                       profile_inputs[graph_name]})
                    assignments[graph_name] = profile_assignments(
                        Path(session.end_profiling()))
                    del session
            except Exception as error:
                assignments[graph_name] = {"error": f"{type(error).__name__}: {error}"}
        summary["profile_assignments"] = assignments
        summary["requested_provider_has_profiled_nodes_by_graph"] = {
            name: (result.get("node_events_by_provider", {}).get(
                requested_provider, 0) > 0 if "error" not in result else None)
            for name, result in assignments.items()
        }
        summary["profile_timing_ms"] = (time.perf_counter() - profile_started) * 1000
        summary["peak_process_rss_bytes_after_profile"] = peak_process_rss_bytes()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    write_json(args.output_dir / "onnx_run_summary.json", summary)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
