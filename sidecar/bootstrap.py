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
import tempfile


MARKER = ".manga-cleaner-ready.json"
#: How much of a failed step's stderr its error carries: it has to fit the 500
#: characters `main` reports, with the words in front of it.
ERROR_TAIL_BYTES = 420
#: How long one pip step may take. Windows' CUDA torch alone is 3.06 GB, and pip
#: starts it again from nothing after a timeout.
PIP_TIMEOUT_SECONDS = 7200
#: The GPU torch and the torchvision built for it, asked for exactly. Given a
#: version range, pip takes any pair that resolves: on an index with no matching
#: torchvision it picks 0.1.6, and the next step then replaces both with PyPI's
#: newest, which on Windows has no GPU support.
TORCH_VERSION = "2.13.0"
TORCHVISION_VERSION = "0.28.0"
#: What starting the helper imports. The package alone imports next to nothing,
#: so a check of it passes for a helper that cannot start.
ENTRY_POINT = "manga_cleaner_sidecar.__main__"
#: Room kept free beyond a download: the Hub's own cache grows beside it, and a
#: full disk breaks more than the download.
DISK_MARGIN_BYTES = 2_000_000_000
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


def run(command: list[str], *, timeout: int = PIP_TIMEOUT_SECONDS, label: str | None = None) -> None:
    """Run one step. Its output stays off stdout, and the end of its stderr goes
    into the error, in a file because a download's progress bars can be long."""
    if label is None:
        label = command[2] if command[1:2] == ["-m"] and len(command) > 2 else "setup step"
    with tempfile.TemporaryFile() as stderr:
        result = subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=stderr, timeout=timeout, check=False,
                                env=safe_environment())
        if result.returncode:
            size = stderr.seek(0, os.SEEK_END)
            stderr.seek(max(0, size - ERROR_TAIL_BYTES))
            tail = stderr.read().decode("utf-8", "replace").strip().rsplit("\r", 1)[-1]
            raise RuntimeError(f"{label} failed (exit {result.returncode}): {tail}")


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


def installed_accelerator(root: Path) -> str | None:
    """The GPU the torch already in this helper was built for, or None.

    Read from the wheel's folder name (`torch-2.13.0+xpu.dist-info`), so a repair
    under Automatic keeps an Intel XPU that was picked by hand rather than looking
    on PATH, where `sycl-ls` often is not.
    """
    venv = root if (root / "pyvenv.cfg").is_file() else root / ".venv"
    for info in venv.glob("**/site-packages/torch-*.dist-info"):
        label = info.name.removesuffix(".dist-info").partition("+")[2]
        if label.startswith("cu"):
            return "cuda"
        if label == "xpu":
            return "xpu"
    return None


def resolve_backend(requested: str, accelerator: str, installed: str | None = None) -> tuple[str, str]:
    if requested == "auto":
        requested = "mflux" if sys.platform == "darwin" and platform.machine() == "arm64" else "sdnq"
    if requested == "mflux":
        if sys.platform != "darwin" or platform.machine() != "arm64":
            raise ValueError("MLX requires an Apple Silicon Mac")
        return requested, "mlx"
    if requested != "sdnq":
        raise ValueError("Unsupported backend")
    if accelerator == "auto":
        if installed and sys.platform != "darwin":
            accelerator = installed
        elif sys.platform == "darwin":
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


def torch_build(accelerator: str) -> str:
    """The PyTorch index, and the version label its wheels carry, for a GPU.
    Windows' cu129 index stops at torch 2.9.0 with no torchvision for it."""
    if accelerator == "xpu":
        return "xpu"
    return "cu130" if sys.platform == "win32" else "cu129"


def valid_checkpoint(checkpoint: dict) -> dict:
    """The checkpoint, or a refusal for one that would write outside `weights/`."""
    names = [checkpoint.get("dir"), *(a.get("file") for a in checkpoint.get("adapters", []))]
    if not all(isinstance(name, str) and name and name == Path(name).name and not name.startswith(".") for name in names):
        raise ValueError("The checkpoint names a path outside the weights folder")
    return checkpoint


def check_disk_room(weights_root: Path, pending: Path, checkpoint: dict) -> None:
    """Refuse a download that cannot fit, before it fills the disk. What a retry
    has already fetched counts."""
    total = checkpoint.get("bytes")
    if not total:
        return
    held = sum(path.stat().st_size for path in pending.rglob("*") if path.is_file()) if pending.is_dir() else 0
    needed = max(0, total - held) + DISK_MARGIN_BYTES
    free = shutil.disk_usage(weights_root).free
    if free < needed:
        raise RuntimeError(f"Not enough disk space for this model: it needs {needed / 1e9:.1f} GB free, "
                           f"and {free / 1e9:.1f} GB is free")


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
    check_disk_room(weights_root, pending, checkpoint)
    pending.mkdir(exist_ok=True)
    code = ("import sys; from huggingface_hub import snapshot_download; "
            "snapshot_download(repo_id=sys.argv[1], revision=sys.argv[2], local_dir=sys.argv[3])")
    run([str(python), "-c", code, checkpoint["repo"], checkpoint["revision"], str(pending)], timeout=7200,
        label="model download")
    for adapter in checkpoint.get("adapters", []):
        code = ("import sys; from huggingface_hub import hf_hub_download; "
                "hf_hub_download(repo_id=sys.argv[1], revision=sys.argv[2], filename=sys.argv[3], local_dir=sys.argv[4])")
        run([str(python), "-c", code, adapter["repo"], adapter["revision"], adapter["file"], str(pending / "adapters")],
            timeout=7200, label="model download")
    if not (pending / checkpoint["expected"]).is_file():
        raise RuntimeError("Downloaded model is incomplete")
    (pending / MARKER).write_text(json.dumps({"repo": checkpoint["repo"], "revision": checkpoint["revision"]}), encoding="utf-8")
    if weights.is_dir():
        shutil.rmtree(weights)
    os.replace(pending, weights)
    report("weights", "done")


def install(root: Path, source: Path, backend: str, accelerator: str,
            model: str = DEFAULT_MODEL, checkpoint: dict | None = None) -> dict[str, str]:
    # Linux has no torchvision 0.28 for Python 3.10.
    if sys.version_info < (3, 11) or sys.version_info >= (3, 13):
        raise ValueError("Python 3.11 or 3.12 is required")
    backend, accelerator = resolve_backend(backend, accelerator, installed_accelerator(root))
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
        import_check = f"import {ENTRY_POINT}, mflux, mlx"
    else:
        import_check = f"import {ENTRY_POINT}, torch, diffusers, sdnq"
        if accelerator in ("cuda", "xpu"):
            build = torch_build(accelerator)
            run([str(python), "-m", "pip", "install", f"torch=={TORCH_VERSION}+{build}",
                 f"torchvision=={TORCHVISION_VERSION}+{build}", "--index-url", f"https://download.pytorch.org/whl/{build}"])
            # The next step may not swap this for a build with no GPU support.
            import_check += f"; assert torch.__version__.endswith('+{build}'), 'torch is not the {build} build: ' + torch.__version__"
        run([str(python), "-m", "pip", "install", "-r", str(source / "requirements-sdnq.txt")])
    run([str(python), "-m", "pip", "install", "--no-deps", str(source)])
    run([str(python), "-c", import_check], timeout=120, label="import check")
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
