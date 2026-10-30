"""Install the optional local model helper and one pinned checkpoint.

Invoked by the desktop with a system Python. It never reads provider credentials,
and writes only under the app's sidecar directory. JSON progress lines on stdout
are safe to forward to the UI; package-manager output stays out of that stream.

Which repository to fetch comes from the app's one table of models
(crates/cleaner-core/src/sidecar/models.rs), as `--checkpoint`. A download lands in
a hidden folder and moves to its name only when it is complete, so the app and the
backends, which skip hidden folders, never see half of one, and a retry resumes it.
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


MARKER = ".manga-cleaner-ready.json"
#: Klein 4B on Hugging Face at the pin, for a call that names no model.
DEFAULT_MODEL = "flux2-klein-4b"
DEFAULT_CHECKPOINTS = {
    "mflux": {"repo": "mflux-community/flux2-klein-4b-mflux-q4",
              "revision": "77090341902cb5f9217f05c664ff604236ca18cc",
              "dir": "flux2-klein-4b-mflux-q4",
              "expected": "transformer/model.safetensors.index.json", "adapters": []},
    "sdnq": {"repo": "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic",
             "revision": "45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd",
             "dir": "flux2-klein-4b-sdnq",
             "expected": "model_index.json", "adapters": []},
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
        raise ValueError("Unsupported backend")
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


def valid_checkpoint(checkpoint: dict) -> dict:
    """The checkpoint, or a refusal for one that would write outside `weights/`."""
    names = [checkpoint.get("dir"), *(a.get("file") for a in checkpoint.get("adapters", []))]
    if not all(isinstance(name, str) and name and name == Path(name).name and not name.startswith(".") for name in names):
        raise ValueError("The checkpoint names a path outside the weights folder")
    return checkpoint


def fetch_weights(python: Path, weights_root: Path, checkpoint: dict) -> None:
    """The pinned snapshot and its adapters, moved to their name once complete."""
    weights = weights_root / checkpoint["dir"]
    marker = weights / MARKER
    try:
        seeded = json.loads(marker.read_text(encoding="utf-8")).get("revision") == checkpoint["revision"]
    except (OSError, ValueError):
        seeded = False
    if seeded:
        return
    report("weights", "start")
    weights_root.mkdir(parents=True, exist_ok=True)
    pending = weights_root / f".installing-{checkpoint['dir']}"
    # A folder with no marker is an older install's partial download, or one for
    # another revision: carry on from what it holds.
    if weights.is_dir() and not pending.exists():
        os.replace(weights, pending)
    pending.mkdir(exist_ok=True)
    code = ("import sys; from huggingface_hub import snapshot_download; "
            "snapshot_download(repo_id=sys.argv[1], revision=sys.argv[2], local_dir=sys.argv[3])")
    run([str(python), "-c", code, checkpoint["repo"], checkpoint["revision"], str(pending)], timeout=7200)
    for adapter in checkpoint.get("adapters", []):
        code = ("import sys; from huggingface_hub import hf_hub_download; "
                "hf_hub_download(repo_id=sys.argv[1], revision=sys.argv[2], filename=sys.argv[3], local_dir=sys.argv[4])")
        run([str(python), "-c", code, adapter["repo"], adapter["revision"], adapter["file"], str(pending / "adapters")],
            timeout=7200)
    if not (pending / checkpoint["expected"]).is_file():
        raise RuntimeError("Downloaded model is incomplete")
    (pending / MARKER).write_text(json.dumps({"repo": checkpoint["repo"], "revision": checkpoint["revision"]}), encoding="utf-8")
    if weights.is_dir():
        shutil.rmtree(weights)
    os.replace(pending, weights)
    report("weights", "done")


def install(root: Path, source: Path, backend: str, accelerator: str,
            model: str = DEFAULT_MODEL, checkpoint: dict | None = None) -> dict[str, str]:
    if sys.version_info < (3, 10) or sys.version_info >= (3, 13):
        raise ValueError("Python 3.10, 3.11 or 3.12 is required")
    backend, accelerator = resolve_backend(backend, accelerator)
    checkpoint = valid_checkpoint(checkpoint or DEFAULT_CHECKPOINTS[backend])
    if not (source / "pyproject.toml").is_file():
        raise ValueError("The app's model helper source is missing")

    root.mkdir(parents=True, exist_ok=True)
    # An environment the root itself is, or the one this file makes under it.
    venv = root if (root / "pyvenv.cfg").is_file() else root / ".venv"
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
    run([str(python), "-m", "pip", "install", "huggingface-hub==1.33.0"])
    if backend == "mflux":
        run([str(python), "-m", "pip", "install", "-r", str(source / "requirements.txt")])
    else:
        if accelerator in ("cuda", "xpu"):
            wheel_index = "https://download.pytorch.org/whl/cu129" if accelerator == "cuda" else "https://download.pytorch.org/whl/xpu"
            # PyTorch's cu129 index has no 2.13 Windows wheel. The cp311
            # Windows CUDA wheel is published for 2.9.0; Linux and XPU have
            # 2.13.0. Pin per platform rather than silently getting CPU torch.
            torch_version = "2.9.0" if sys.platform == "win32" and accelerator == "cuda" else "2.13.0"
            run([str(python), "-m", "pip", "install", f"torch=={torch_version}", "torchvision", "--index-url", wheel_index])
        run([str(python), "-m", "pip", "install", "-r", str(source / "requirements-sdnq.txt")])
    run([str(python), "-m", "pip", "install", "--no-deps", str(source)])
    import_check = "import manga_cleaner_sidecar, mflux, mlx" if backend == "mflux" else "import manga_cleaner_sidecar, torch, diffusers, sdnq"
    run([str(python), "-c", import_check], timeout=120)
    report("dependencies", "done")

    fetch_weights(python, root / "weights", checkpoint)
    if active == pending:
        if venv.exists():
            shutil.rmtree(venv)
        os.replace(pending, venv)
    report("ready", "done")
    return {"backend": backend, "accelerator": accelerator, "model": model, "root": str(root)}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--backend", choices=("auto", "mflux", "sdnq"), default="auto")
    parser.add_argument("--accelerator", choices=("auto", "cuda", "xpu", "mps"), default="auto")
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--checkpoint", type=json.loads, default=None)
    args = parser.parse_args()
    try:
        result = install(args.root, args.source, args.backend, args.accelerator, args.model, args.checkpoint)
    except Exception as exc:
        report("error", type(exc).__name__)
        print(json.dumps({"error": str(exc)[:500]}), flush=True)
        return 1
    print(json.dumps({"result": result}), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
