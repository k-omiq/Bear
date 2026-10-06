#!/usr/bin/env python3
"""Select a pinned SAM-TS-L mask backend for one local image.

This is a standalone proof tool. PyTorch and ONNX Runtime may live in separate
Python environments; pass their executables with --torch-python and
--ort-python. A requested backend is never replaced with another one.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


BACKENDS = {
    "torch-cpu": ("torch", "cpu", ("darwin", "linux", "win32")),
    "torch-mps": ("torch", "mps", ("darwin",)),
    "torch-cuda": ("torch", "cuda", ("linux", "win32")),
    "ort-cpu": ("ort", "cpu", ("darwin", "linux", "win32")),
    "ort-coreml": ("ort", "coreml", ("darwin",)),
    "ort-cuda": ("ort", "cuda", ("linux", "win32")),
    "ort-directml": ("ort", "directml", ("win32",)),
    "ort-webgpu": ("ort", "webgpu", ("darwin", "linux", "win32")),
    "ort-migraphx": ("ort", "migraphx", ("linux",)),
}


def probe(script: Path, python: Path) -> tuple[dict | None, str | None]:
    try:
        completed = subprocess.run(
            [str(python), str(script), "--list-backends"],
            capture_output=True, text=True, timeout=30, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return None, str(error)
    if completed.returncode:
        return None, (completed.stderr or completed.stdout).strip()[-1000:]
    try:
        return json.loads(completed.stdout), None
    except json.JSONDecodeError:
        return None, "backend probe returned invalid JSON"


def main() -> None:
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list-backends", action="store_true")
    parser.add_argument("--backend", choices=tuple(BACKENDS))
    parser.add_argument("--input", type=Path)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--reference-page-dir", type=Path,
                        help="optional saved FP32 PyTorch reference for parity")
    parser.add_argument("--torch-python", type=Path, default=Path(sys.executable))
    parser.add_argument("--ort-python", type=Path, default=Path(sys.executable))
    parser.add_argument("--weights", type=Path,
                        help="full pinned SafeTensors file for PyTorch")
    parser.add_argument("--hi-sam-root", type=Path,
                        help="pinned Hi-SAM source for PyTorch")
    parser.add_argument("--graph-dir", type=Path,
                        help="pinned encoder/head ONNX package directory")
    parser.add_argument("--threads", type=int, default=4)
    parser.add_argument("--cuda-index", type=int, default=0,
                        help="PyTorch CUDA device index")
    parser.add_argument("--timeout-seconds", type=int, default=180,
                        help="maximum time for inference and profiling (default: 180)")
    args = parser.parse_args()

    if args.list_backends:
        torch_status, torch_error = probe(root / "probe_torch.py", args.torch_python)
        ort_status, ort_error = probe(root / "infer_ort.py", args.ort_python)
        rows = []
        for name, (engine, device, platforms) in BACKENDS.items():
            applicable = sys.platform in platforms
            status, error = ((torch_status, torch_error) if engine == "torch"
                             else (ort_status, ort_error))
            reported = (status or {}).get("backends", {}).get(device, {})
            installed = (reported.get("installed", reported.get("available"))
                         if applicable and not error else False)
            device_available = (reported.get("available")
                                if applicable and not error else False)
            if not applicable:
                availability_status = "unsupported_platform"
            elif error:
                availability_status = "probe_failed"
            elif device_available is True:
                availability_status = "available"
            elif installed and device_available is None:
                availability_status = "installed_device_unverified"
            else:
                availability_status = "unavailable"
            rows.append({
                "id": name, "engine": engine, "device": device,
                "platforms": list(platforms), "applicable_here": applicable,
                "runtime_installed_here": bool(installed),
                "device_available_here": device_available,
                "availability_status": availability_status,
                "reason": (f"unsupported on {sys.platform}" if not applicable else
                           error or reported.get("reason") or
                           ("device availability has not been verified"
                            if availability_status == "installed_device_unverified" else
                            None if device_available is True else "backend not reported")),
                "qualification": ("Mac M5 page-07–09 parity measured" if name in
                                  ("torch-cpu", "torch-mps", "ort-cpu", "ort-webgpu")
                                  else "not qualified for this mask graph"),
            })
        print(json.dumps({"platform": sys.platform, "backends": rows}, indent=2))
        return

    if not args.backend or not args.input or not args.output_dir:
        parser.error("--backend, --input and --output-dir are required for inference")
    if args.threads < 1 or args.cuda_index < 0 or args.timeout_seconds < 1:
        parser.error("--threads and --timeout-seconds must be positive; --cuda-index nonnegative")
    engine, device, platforms = BACKENDS[args.backend]
    if sys.platform not in platforms:
        parser.error(f"{args.backend} is unavailable on {sys.platform}")
    source = args.input.resolve(strict=True)
    destination = args.output_dir.resolve()

    if engine == "torch":
        command = [str(args.torch_python), str(root / "probe_torch.py"),
                   "--device", device, "--input", str(source),
                   "--output", str(destination / "run.json"),
                   "--output-mask", str(destination / "mask.png"),
                   "--torch-threads", str(args.threads),
                   "--cuda-index", str(args.cuda_index)]
        if args.reference_page_dir:
            command += ["--page-dir", str(args.reference_page_dir.resolve(strict=True))]
        if args.weights:
            command += ["--weights", str(args.weights.resolve(strict=True))]
        if args.hi_sam_root:
            command += ["--hi-sam-root", str(args.hi_sam_root.resolve(strict=True))]
    else:
        command = [str(args.ort_python), str(root / "infer_ort.py"),
                   "--provider", device, "--input", str(source),
                   "--output-dir", str(destination), "--threads", str(args.threads)]
        if args.reference_page_dir:
            command += ["--reference-dir", str(args.reference_page_dir.resolve(strict=True))]
        if args.graph_dir:
            command += ["--graph-dir", str(args.graph_dir.resolve(strict=True))]

    try:
        completed = subprocess.run(command, check=False, timeout=args.timeout_seconds)
    except subprocess.TimeoutExpired:
        parser.exit(124, f"{args.backend} exceeded {args.timeout_seconds} seconds\n")
    raise SystemExit(completed.returncode)


if __name__ == "__main__":
    main()
