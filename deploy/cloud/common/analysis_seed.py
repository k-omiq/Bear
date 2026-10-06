"""Pinned, optional graph installation for cloud review-only analysis.

The CPU seed task acquires graphs. GPU workers only read verified files. A ready
marker is published last, so a failed download or Linux export never advertises a
capability. These pins match the desktop manifests in model_workflows.rs.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable, Iterable

from deploy.cloud.common.contract import ANALYSIS_RT, ANALYSIS_SAM, ANALYSIS_VERSION
from deploy.cloud.common.manifest import MODEL_PROD_FLUX
from deploy.cloud.common.weights import SEED_STATE_KEY, seed_state
from deploy.cloud.common.redact import redact_text

SAM_REVISION = "5dd97423e0fbf2404264979136d47e8101144046"
RT_REVISION = "16e8a622f91fabc6b5b65c96d32d1183f8843546"
RT_REPO = "ogkalu/comic-text-and-bubble-detector"
# The head export embeds a derived positional encoding. PyTorch 2.13.0
# produces different ONNX bytes on macOS arm64 and Linux x86_64, even though
# the checkpoint, graph structure, graph size, and fixed-input mask agree.
# Keep each observed package hash pinned; never accept an arbitrary export.
SAM_HEAD_SHA256_MAC = "a2c63ccf54e2e692a281cffd4dcda648f252ae6649dc7d23d0203e5868685281"
SAM_HEAD_SHA256_LINUX = "d9431cf1828bbbf70db26f438f5b5777783729dbf7cac5f20cd2b14057125bd2"


def sam_head_export_sha256(platform: str) -> str:
    if platform == "linux":
        return SAM_HEAD_SHA256_LINUX
    # The helper also imports this module on Windows before deploying the
    # Linux image. Windows does not run this exporter; the remote import
    # selects its own Linux pin.
    return SAM_HEAD_SHA256_MAC


GRAPH_FILES = {
    ANALYSIS_SAM: (
        ("koharu_samts_encoder.onnx", 1_335_305_985,
         "9b3a32f9018008cfd2c7a5b1a7eb6e20822ba43eab58918863f74ac62ecbafbe"),
        ("koharu_samts_text_head.onnx", 22_704_641,
         sam_head_export_sha256(sys.platform)),
    ),
    ANALYSIS_RT: (("detector.onnx", 168_481_531,
                   "065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195"),),
}
REVISIONS = {ANALYSIS_SAM: SAM_REVISION, ANALYSIS_RT: RT_REVISION}
DIRECTORIES = {ANALYSIS_SAM: "sam-ts-l", ANALYSIS_RT: "rtdetr-v2-full"}
READY_SCHEMA = 1


class AnalysisUnavailable(RuntimeError):
    """Selected graphs are absent or unverified; do not start an analysis GPU."""


class VerifiedGraphCache:
    """Remember a verified digest only while that exact volume file is present.

    The gateway checks marker contents and file identity on every request. A
    replacement, write, or removal invalidates the digest without streaming a
    1.3 GB encoder again for each tile. The lock also prevents simultaneous
    first requests from hashing the same graph in parallel.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._verified: dict[tuple[str, str], tuple[int, ...]] = {}

    @staticmethod
    def _identity(path: Path) -> tuple[int, ...]:
        stat = path.stat()
        return (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns)

    def matches(self, path: Path, sha256: str) -> bool:
        key = (str(path), sha256)
        with self._lock:
            try:
                before = self._identity(path)
                if self._verified.get(key) == before:
                    return True
                self._verified.pop(key, None)
                if _digest(path) != sha256 or self._identity(path) != before:
                    return False
            except OSError:
                return False
            self._verified[key] = before
            return True


def normalize_analysis_models(value: Any) -> tuple[str, ...]:
    """Accept only a unique subset of the two supported cloud graph capabilities."""
    if value is None:
        return ()
    if not isinstance(value, (list, tuple)) or any(not isinstance(item, str) for item in value):
        raise ValueError("analysis_models must be a list of capability names")
    if len(value) != len(set(value)) or any(item not in GRAPH_FILES for item in value):
        raise ValueError("analysis_models must be a unique subset of SAM-TS-L lettering mask and Ogkalu comic text & bubble detector (Full)")
    return tuple(key for key in (ANALYSIS_SAM, ANALYSIS_RT) if key in value)


def _digest(path: Path) -> str:
    sha = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(4 * 1024 * 1024), b""):
            sha.update(chunk)
    return sha.hexdigest()


def _marker(capability: str) -> dict[str, Any]:
    return {"schema": READY_SCHEMA, "capability": capability,
            "model_revision": REVISIONS[capability],
            "graphs": [{"name": name, "bytes": size, "sha256": sha}
                       for name, size, sha in GRAPH_FILES[capability]]}


def _directory(root: Path, capability: str) -> Path:
    return root / "analysis" / DIRECTORIES[capability]


def installed_graphs(root: str | os.PathLike[str], selected: Iterable[str], *,
                     verify_hash: bool = False,
                     cache: VerifiedGraphCache | None = None) -> tuple[dict[str, list[str]], dict[str, str]]:
    """Return only complete selected capabilities; never infer readiness from options."""
    graphs: dict[str, list[str]] = {}
    revisions: dict[str, str] = {}
    for capability in normalize_analysis_models(tuple(selected)):
        directory = _directory(Path(root), capability)
        try:
            marker_path = directory / "ready.json"
            marker_identity = VerifiedGraphCache._identity(marker_path)
            marker = json.loads(marker_path.read_text(encoding="utf-8"))
            if marker != _marker(capability):
                continue
            paths = [directory / name for name, _, _ in GRAPH_FILES[capability]]
            if any(not path.is_file() or path.stat().st_size != size
                   for path, (_, size, _) in zip(paths, GRAPH_FILES[capability])):
                continue
            if verify_hash and any(not (cache.matches(path, sha) if cache else _digest(path) == sha)
                                   for path, (_, _, sha) in zip(paths, GRAPH_FILES[capability])):
                continue
            if VerifiedGraphCache._identity(marker_path) != marker_identity:
                continue
        except (OSError, ValueError, TypeError):
            continue
        graphs[capability] = [str(path) for path in paths]
        revisions[capability] = REVISIONS[capability]
    return graphs, revisions


def analysis_capabilities(root: str | os.PathLike[str], selected: Iterable[str],
                          reload: Callable[[], Any] | None = None, *,
                          cache: VerifiedGraphCache | None = None) -> dict[str, Any]:
    """No GPU startup: advertise only graph sets with a completed seed marker."""
    if reload is not None:
        reload()
    selected = normalize_analysis_models(tuple(selected))
    graphs, revisions = installed_graphs(root, selected, verify_hash=True, cache=cache)
    return {
        "protocol_version": ANALYSIS_VERSION,
        "capabilities": [
            {"capability": capability,
             "graph_sha256s": [sha for _, _, sha in GRAPH_FILES[capability]],
             "model_revision": revisions[capability]}
            for capability in selected if capability in graphs
        ],
        "limits": {"max_tile_side": 1024, "max_tile_pixels": 1048576,
                   "max_png_bytes": 4194304, "max_components": 4096, "max_boxes": 4096},
    }


def _publish(root: Path, capability: str, source: Path) -> None:
    directory = _directory(root, capability)
    directory.mkdir(parents=True, exist_ok=True)
    for name, size, sha in GRAPH_FILES[capability]:
        path = source / name
        if not path.is_file() or path.stat().st_size != size or _digest(path) != sha:
            raise RuntimeError(f"Pinned {capability} graph did not match its size and SHA-256: {name}")
    # The marker is invalidated before replacement, and published only after every
    # graph has been copied and reverified on the mounted volume.
    (directory / "ready.json").unlink(missing_ok=True)
    for name, _, _ in GRAPH_FILES[capability]:
        shutil.copyfile(source / name, directory / f"{name}.partial")
        os.replace(directory / f"{name}.partial", directory / name)
    for name, size, sha in GRAPH_FILES[capability]:
        path = directory / name
        if path.stat().st_size != size or _digest(path) != sha:
            raise RuntimeError(f"Published {capability} graph failed verification: {name}")
    temp = directory / "ready.json.partial"
    temp.write_text(json.dumps(_marker(capability), sort_keys=True), encoding="utf-8")
    os.replace(temp, directory / "ready.json")


def seed_analysis_graphs(root: str | os.PathLike[str], selected: Iterable[str],
                         source_root: str | os.PathLike[str], *,
                         hf_download: Callable[..., str] | None = None,
                         run: Callable[..., Any] = subprocess.run,
                         commit: Callable[[], Any] | None = None) -> dict[str, Any]:
    """Acquire selected graphs on CPU, using the desktop's pinned SAM exporter."""
    selected = normalize_analysis_models(tuple(selected))
    volume = Path(root)
    source = Path(source_root)
    installed, _ = installed_graphs(volume, selected, verify_hash=True)
    if len(installed) == len(selected):
        return {"status": "present", "capabilities": list(selected)}
    if hf_download is None:
        from huggingface_hub import hf_hub_download
        hf_download = hf_hub_download
    for capability in selected:
        if capability in installed:
            continue
        stage = volume / "analysis" / ".staging" / DIRECTORIES[capability]
        stage.mkdir(parents=True, exist_ok=True)
        if capability == ANALYSIS_RT:
            target = stage / "detector.onnx"
            name, size, sha = GRAPH_FILES[capability][0]
            if not target.is_file() or target.stat().st_size != size or _digest(target) != sha:
                # Hugging Face's local_dir copy can resolve Modal's volume
                # mount to its backing path and then copy the file onto itself.
                # Download outside the volume and publish the verified copy.
                with tempfile.TemporaryDirectory(prefix="mc-rt-download-") as download_dir:
                    downloaded = Path(hf_download(repo_id=RT_REPO, filename=name,
                                                  revision=RT_REVISION, local_dir=download_dir))
                    if not target.exists() or not os.path.samefile(downloaded, target):
                        shutil.copyfile(downloaded, target)
            _publish(volume, capability, stage)
        else:
            bootstrap = source / "bootstrap.py"
            exporter = source / "export_mask.py"
            fixture = source / "synthetic_page.png"
            if not all(path.is_file() for path in (bootstrap, exporter, fixture)):
                raise RuntimeError("Pinned SAM exporter resources are absent from the seed image")
            run([sys.executable, str(bootstrap), "--root", str(stage),
                 "--source", str(source)], check=True, timeout=3600)
            _publish(volume, capability, stage / "proof")
        if commit is not None:
            commit()
        # The published graphs are the durable artifact. Checkpoint, source ZIP,
        # and export intermediates need not occupy the billed volume afterward.
        shutil.rmtree(stage)
        if commit is not None:
            commit()
    return {"status": "seeded", "capabilities": list(selected)}


def run_analysis_seed(root: str | os.PathLike[str], selected: Iterable[str],
                      source_root: str | os.PathLike[str], store: Any, *,
                      model_id: str = MODEL_PROD_FLUX,
                      commit: Callable[[], Any] | None = None,
                      seed: Callable[..., dict[str, Any]] = seed_analysis_graphs) -> dict[str, Any]:
    """Keep the provisioner's seed heartbeat alive through graph acquisition."""
    selected = normalize_analysis_models(tuple(selected))
    if not selected:
        return {"status": "present", "capabilities": []}
    stop = threading.Event()
    def heartbeat() -> None:
        while not stop.is_set():
            try:
                store.put(SEED_STATE_KEY, seed_state("running", 0, model_id=model_id))
            except Exception:
                pass
            stop.wait(5)
    reporter = threading.Thread(target=heartbeat, daemon=True)
    reporter.start()
    try:
        return seed(root, selected, source_root, commit=commit)
    except Exception as exc:
        message = redact_text(f"Cloud analysis graph installation failed: {exc}")[:512]
        try:
            store.put(SEED_STATE_KEY, seed_state("failed", 0, error_code="analysis_seed_failed",
                                                  message=message, model_id=model_id))
        except Exception:
            pass
        return {"status": "failed", "error_code": "analysis_seed_failed", "message": message}
    finally:
        stop.set()
        reporter.join(timeout=1)
