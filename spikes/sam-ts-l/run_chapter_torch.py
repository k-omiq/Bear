#!/usr/bin/env python3
"""Run the pinned full SAM-TS-L mask model over a chapter with one model load.

Masks only: this does not produce text boxes, reading order, OCR, or inpainting.
Pages run sequentially. The source/preprocessing/restoration and strict model
verification are shared with the single-page PyTorch proof.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import time
from pathlib import Path

# Set before importing PyTorch or code that imports it.
os.environ["PYTORCH_ENABLE_MPS_FALLBACK"] = "0"

import numpy as np
import torch
from PIL import Image

from export_mask import CHECKPOINT_SHA256, load_model, prepare, restore, sha256
from probe_torch import (MpsSampler, backend_availability, mps_memory,
                         peak_process_rss_bytes, select_device, synchronize)

EXTENSIONS = {".jpg", ".jpeg", ".png", ".webp", ".tif", ".tiff"}


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--device", choices=("cpu", "mps", "cuda"), required=True)
    parser.add_argument("--cuda-index", type=int, default=0)
    parser.add_argument("--torch-threads", type=int, default=4)
    parser.add_argument("--limit", type=int, help="process only the first N pages for a smoke run")
    root = Path(__file__).resolve().parent
    parser.add_argument("--weights", type=Path, default=root / "artifacts/model.safetensors")
    parser.add_argument("--hi-sam-root", type=Path, default=root / "artifacts/Hi-SAM")
    args = parser.parse_args()
    if args.torch_threads < 1 or args.cuda_index < 0 or (args.limit is not None and args.limit < 1):
        parser.error("threads and limit must be positive; CUDA index must be nonnegative")

    source_dir = args.source_dir.resolve(strict=True)
    output_dir = args.output_dir.resolve()
    if not source_dir.is_dir():
        parser.error("--source-dir must be a directory")
    if output_dir == source_dir or source_dir in output_dir.parents:
        parser.error("output directory must be outside the chapter source directory")
    pages = sorted((p for p in source_dir.iterdir()
                    if p.is_file() and p.suffix.lower() in EXTENSIONS),
                   key=lambda p: (int(p.stem) if p.stem.isdigit() else float("inf"), p.name))
    if not pages:
        parser.error("no supported image pages found")
    by_stem: dict[str, Path] = {}
    for page in pages:
        previous = by_stem.get(page.stem)
        if previous is not None:
            parser.error(f"duplicate output stem {page.stem!r}: "
                         f"{previous.name} and {page.name}")
        by_stem[page.stem] = page
    selected = pages[:args.limit] if args.limit else pages
    availability = backend_availability()
    device = select_device(args.device, args.cuda_index, availability)
    torch.set_num_threads(args.torch_threads)
    output_dir.mkdir(parents=True, exist_ok=True)
    masks_dir = output_dir / "masks"
    masks_dir.mkdir(exist_ok=True)

    started = time.perf_counter()
    sampler = MpsSampler() if args.device == "mps" else None
    if sampler:
        sampler.start()
    summary = {
        "description": "Pinned full-checkpoint SAM-TS-L mask-only chapter inference",
        "status": "running",
        "source_dir": str(source_dir),
        "output_dir": str(output_dir),
        "discovered_pages": len(pages),
        "selected_pages": len(selected),
        "device": str(device),
        "mps_cpu_fallback_enabled": False,
        "model": {"checkpoint_sha256": CHECKPOINT_SHA256},
        "environment": {"platform": platform.platform(), "torch": torch.__version__,
                        "torch_threads": args.torch_threads},
        "measurement": {
            "page_total": "open source through saved mask and page JSON, excluding summary write",
            "inference": "synchronized model forward and logits transfer to CPU",
            "peak_rss": "process high-water mark, cumulative across chapter",
            "mps": "PyTorch Metal allocations sampled every 20 ms; sampled maximum may miss transient peak and overlaps unified-memory RSS",
        },
        "pages": [],
    }
    try:
        load_started = time.perf_counter()
        model, keys, elements, source = load_model(args.hi_sam_root, args.weights.resolve(strict=True))
        summary["model"].update({"source": source, "strict_loaded_keys": len(keys),
                                 "checkpoint_elements": elements})
        summary["timing_ms"] = {"model_load_cpu": (time.perf_counter() - load_started) * 1000}
        transfer_started = time.perf_counter()
        model.to(device).eval()
        synchronize(device)
        summary["timing_ms"]["model_transfer"] = (time.perf_counter() - transfer_started) * 1000
        summary["memory_after_model_transfer"] = {
            "peak_process_rss_bytes": peak_process_rss_bytes(),
            "mps": mps_memory() if sampler else None,
        }

        for index, image_path in enumerate(selected, 1):
            page_started = time.perf_counter()
            prepare_started = time.perf_counter()
            with Image.open(image_path) as image:
                prepared, resized_wh, original_wh = prepare(image)
            input_tensor = prepared.contiguous().to(device)
            synchronize(device)
            prepare_ms = (time.perf_counter() - prepare_started) * 1000

            infer_started = time.perf_counter()
            with torch.inference_mode():
                result = model([{"image": input_tensor, "original_size": (1024, 1024)}],
                               multimask_output=False)
            synchronize(device)
            logits = result[3].detach().cpu().numpy()
            synchronize(device)
            inference_ms = (time.perf_counter() - infer_started) * 1000
            del result, input_tensor, prepared
            if logits.shape != (1, 1, 1024, 1024) or not np.isfinite(logits).all():
                raise ValueError(f"unexpected or nonfinite logits for {image_path}")

            restore_started = time.perf_counter()
            mask = restore(logits, resized_wh, original_wh)
            mask_path = masks_dir / f"{image_path.stem}.png"
            Image.fromarray(mask).save(mask_path)
            restore_ms = (time.perf_counter() - restore_started) * 1000
            foreground_pixels = int(np.count_nonzero(mask))
            page = {
                "page": image_path.stem,
                "source": str(image_path),
                "source_sha256": sha256(image_path),
                "source_bytes": image_path.stat().st_size,
                "original_wh": list(original_wh),
                "resized_wh": list(resized_wh),
                "mask_png": str(mask_path),
                "mask_sha256": sha256(mask_path),
                "foreground_pixels": foreground_pixels,
                "foreground_fraction": foreground_pixels / mask.size,
                "timing_ms": {"prepare_and_transfer": prepare_ms, "inference": inference_ms,
                              "restore_and_save": restore_ms,
                              "page_total": (time.perf_counter() - page_started) * 1000},
                "peak_process_rss_bytes": peak_process_rss_bytes(),
                "mps_memory": mps_memory() if sampler else None,
            }
            write_json(output_dir / f"page-{image_path.stem}.json", page)
            summary["pages"].append(page)
            summary["completed_pages"] = len(summary["pages"])
            write_json(output_dir / "summary.json", summary)
            print(f"{index}/{len(selected)} {image_path.name}: "
                  f"{inference_ms / 1000:.2f}s inference, {foreground_pixels} mask pixels",
                  flush=True)
            del logits, mask
    finally:
        if sampler:
            sampler.stop()
        summary["timing_ms"] = summary.get("timing_ms", {})
        summary["timing_ms"]["whole_run"] = (time.perf_counter() - started) * 1000
        summary["peak_process_rss_bytes"] = peak_process_rss_bytes()
        summary["mps_sampled_max_bytes"] = sampler.sampled_max_bytes if sampler else None
        summary["mps_samples"] = sampler.samples if sampler else 0
        summary["status"] = "completed" if len(summary["pages"]) == len(selected) else "incomplete"
        write_json(output_dir / "summary.json", summary)


if __name__ == "__main__":
    main()
