"""Install the optional local FLUX helper and a pinned 4B checkpoint.

Invoked by the desktop with a system Python. It never reads provider credentials,
and writes only under the app's sidecar directory. JSON progress lines on stdout
are safe to forward to the UI; package-manager output stays out of that stream.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
from pathlib import Path
import shutil
import subprocess
import sys


CHECKPOINTS = {
    "mflux": ("mflux-community/flux2-klein-4b-mflux-q4",
              "77090341902cb5f9217f05c664ff604236ca18cc", "flux2-klein-4b-mflux-q4"),
    "sdnq": ("Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic",
             "45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd", "flux2-klein-4b-sdnq"),
}


def report(step: str, state: str) -> None:
    print(json.dumps({"step": step, "state": state}, separators=(",", ":")), flush=True)


def run(command: list[str], *, timeout: int = 1800) -> None:
    result = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, timeout=timeout, check=False,
                            env=safe_environment())
    if result.returncode:
        raise RuntimeError(f"{command[1] if len(command) > 1 else 'command'} failed (exit {result.returncode})")


def safe_environment() -> dict[str, str]:
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("PIP_", "HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "UV_"))
           and key not in ("PYTHONPATH", "PYTHONHOME")}
    env["PIP_CONFIG_FILE"] = os.devnull
    env["HF_HUB_DISABLE_IMPLICIT_TOKEN"] = "1"
    env["HF_HUB_DISABLE_TELEMETRY"] = "1"
    env["PYTHONNOUSERSITE"] = "1"
    return env


def python_in(venv: Path) -> Path:
    return venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python3")


def resolve_backend(requested: str, accelerator: str) -> tuple[str, str]:
    if requested == "auto":
        requested = "mflux" if sys.platform == "darwin" and platform.machine() == "arm64" else "sdnq"
    if requested == "mflux":
        if sys.platform != "darwin" or platform.machine() != "arm64":
            raise ValueError("MLX requires an Apple Silicon Mac")
        return requested, "mlx"
    if requested != "sdnq":
        raise ValueError("Unsupported FLUX backend")
    if accelerator == "auto":
        if sys.platform == "darwin":
            accelerator = "mps"
        elif shutil.which("nvidia-smi"):
            accelerator = "cuda"
        elif shutil.which("sycl-ls"):
            accelerator = "xpu"
        else:
            raise ValueError("No supported GPU found. Select CUDA or Intel XPU after installing its driver")
    if accelerator not in ("cuda", "xpu", "mps") or (accelerator == "mps" and sys.platform != "darwin"):
        raise ValueError("Unsupported accelerator for SDNQ")
    return requested, accelerator


def install(root: Path, source: Path, backend: str, accelerator: str) -> dict[str, str]:
    if sys.version_info < (3, 10) or sys.version_info >= (3, 13):
        raise ValueError("Python 3.10, 3.11 or 3.12 is required")
    backend, accelerator = resolve_backend(backend, accelerator)
    if not (source / "pyproject.toml").is_file():
        raise ValueError("The app's FLUX helper source is missing")

    root.mkdir(parents=True, exist_ok=True)
    venv = root / ".venv"
    pending = root / ".venv-installing"
    if not (venv / "pyvenv.cfg").is_file():
        if pending.exists():
            shutil.rmtree(pending)
        report("environment", "start")
        run([sys.executable, "-m", "venv", str(pending)], timeout=120)
        active = pending
    else:
        active = venv
    python = python_in(active)
    if not python.is_file():
        raise RuntimeError("Virtual environment has no Python interpreter")
    report("dependencies", "start")
    run([str(python), "-m", "pip", "install", "huggingface-hub==1.24.0"])
    if backend == "mflux":
        run([str(python), "-m", "pip", "install", "-r", str(source / "requirements.txt")])
    else:
        if accelerator in ("cuda", "xpu"):
            wheel_index = "https://download.pytorch.org/whl/cu129" if accelerator == "cuda" else "https://download.pytorch.org/whl/xpu"
            # PyTorch's cu129 index has no 2.13 Windows wheel. The cp311
            # Windows CUDA wheel is published for 2.9.0; Linux and XPU have
            # 2.13.0. Pin per platform rather than silently getting CPU torch.
            torch_version = "2.9.0" if sys.platform == "win32" and accelerator == "cuda" else "2.13.0"
            run([str(python), "-m", "pip", "install", f"torch=={torch_version}", "--index-url", wheel_index])
        run([str(python), "-m", "pip", "install", "-r", str(source / "requirements-sdnq.txt")])
    run([str(python), "-m", "pip", "install", "--no-deps", str(source)])
    import_check = "import manga_cleaner_sidecar, mflux, mlx" if backend == "mflux" else "import manga_cleaner_sidecar, torch, diffusers, sdnq"
    run([str(python), "-c", import_check], timeout=120)
    report("dependencies", "done")

    repo, revision, directory = CHECKPOINTS[backend]
    weights = root / "weights" / directory
    weights.mkdir(parents=True, exist_ok=True)
    marker = weights / ".manga-cleaner-ready.json"
    try:
        seeded = json.loads(marker.read_text(encoding="utf-8")).get("revision") == revision
    except (OSError, ValueError):
        seeded = False
    if not seeded:
        report("weights", "start")
        code = ("import sys; from huggingface_hub import snapshot_download; "
                "snapshot_download(repo_id=sys.argv[1], revision=sys.argv[2], local_dir=sys.argv[3])")
        run([str(python), "-c", code, repo, revision, str(weights)], timeout=7200)
        expected = "model_index.json" if backend == "sdnq" else "transformer/model.safetensors.index.json"
        if not (weights / expected).is_file():
            raise RuntimeError("Downloaded FLUX checkpoint is incomplete")
        temp = marker.with_suffix(".tmp")
        temp.write_text(json.dumps({"repo": repo, "revision": revision}), encoding="utf-8")
        os.replace(temp, marker)
        report("weights", "done")
    if active == pending:
        if venv.exists():
            shutil.rmtree(venv)
        os.replace(pending, venv)
    report("ready", "done")
    return {"backend": backend, "accelerator": accelerator, "model": "flux2-klein-4b", "root": str(root)}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--backend", choices=("auto", "mflux", "sdnq"), default="auto")
    parser.add_argument("--accelerator", choices=("auto", "cuda", "xpu", "mps"), default="auto")
    args = parser.parse_args()
    try:
        result = install(args.root, args.source, args.backend, args.accelerator)
    except Exception as exc:
        report("error", type(exc).__name__)
        print(json.dumps({"error": str(exc)[:500]}), flush=True)
        return 1
    print(json.dumps({"result": result}), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
