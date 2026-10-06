"""Pinned weights snapshot on a provider volume.

A CPU job downloads the pinned snapshot once into the shared weights volume; GPU
workers only read it and never download. The marker file is written last, after the
file set and every size were checked, so its presence means the snapshot is complete.
Nothing here imports a provider SDK, torch or huggingface_hub at module load.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import threading
import time
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Union

from deploy.cloud.common.manifest import (
    ADAPTER_DIR,
    MODEL_PROD_FLUX,
    REVISION_PROD_FLUX,
    production_model,
)
from deploy.cloud.common.redact import redact_text

# The directory name carries the commit hash, so a future pin lands beside the old
# snapshot instead of mixing files into it.
SNAPSHOT_DIR_NAME: str = f"{MODEL_PROD_FLUX.replace('/', '--')}--{REVISION_PROD_FLUX}"
MARKER_FILE_NAME: str = f"{SNAPSHOT_DIR_NAME}.ready.json"
MARKER_SCHEMA: int = 1

# Key of the seeding status document in the installation's job store (Modal Dict,
# Beam Map). The provisioner reads it to report download progress.
SEED_STATE_KEY: str = "seed:state"
SEED_ATTEMPTS: int = 3
SEED_RETRY_DELAY_SECONDS: float = 10.0
PROGRESS_INTERVAL_SECONDS: float = 5.0

PathLike = Union[str, os.PathLike]
ProgressCallback = Callable[[int, int], Any]

logger = logging.getLogger("deploy.cloud.weights")


class WeightsError(RuntimeError):
    """Typed weights failure; `error_code` is a stable snake_case code."""

    error_code = "weights_error"


class WeightsMissingError(WeightsError):
    error_code = "weights_missing"


class WeightsCorruptError(WeightsError):
    error_code = "weights_corrupt"


def snapshot_name(model_id: str = MODEL_PROD_FLUX) -> str:
    model = production_model(model_id)
    return f"{model.model_id.replace('/', '--')}--{model.revision}"


def snapshot_dir(volume_root: PathLike, model_id: str = MODEL_PROD_FLUX) -> Path:
    return Path(volume_root) / snapshot_name(model_id)


def marker_path(volume_root: PathLike, model_id: str = MODEL_PROD_FLUX) -> Path:
    return Path(volume_root) / f"{snapshot_name(model_id)}.ready.json"


def check_snapshot_files(directory: PathLike, model_id: str = MODEL_PROD_FLUX) -> List[str]:
    """Return one line per problem (missing file, wrong size); empty when complete."""
    root = Path(directory)
    problems: List[str] = []
    total = 0
    model = production_model(model_id)
    for relative_path, expected_size in model.files:
        target = root / relative_path
        try:
            size = target.stat().st_size if target.is_file() else None
        except OSError:
            size = None
        if size is None:
            problems.append(f"missing: {relative_path}")
            continue
        if size != expected_size:
            problems.append(f"size mismatch: {relative_path} ({size} != {expected_size})")
            continue
        total += size
    if not problems and total != model.total_bytes:
        problems.append(f"total size mismatch ({total} != {model.total_bytes})")
    return problems


def snapshot_fingerprint(model_id: str = MODEL_PROD_FLUX) -> str:
    """Identify the pinned file set, including same-size adapter replacements."""
    model = production_model(model_id)
    identity = {
        "model_id": model.model_id,
        "model_revision": model.revision,
        "files": sorted(model.files),
        "adapters": sorted(
            (adapter.repo_id, adapter.revision, adapter.filename, adapter.size_bytes)
            for adapter in model.adapters
        ),
    }
    canonical = json.dumps(identity, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def expected_marker(model_id: str = MODEL_PROD_FLUX) -> Dict[str, Any]:
    model = production_model(model_id)
    return {
        "schema": MARKER_SCHEMA,
        "model_id": model.model_id,
        "model_revision": model.revision,
        "directory": snapshot_name(model_id),
        "file_count": len(model.files),
        "total_bytes": model.total_bytes,
        "snapshot_fingerprint": snapshot_fingerprint(model_id),
    }


def marker_matches(data: Any, model_id: str = MODEL_PROD_FLUX) -> bool:
    """Whether a parsed marker names exactly the pinned snapshot, adapters included.

    Legacy markers and markers for replaced or revised adapters do not match,
    even when the base revision, file count and total bytes stay the same.
    """
    if not isinstance(data, dict):
        return False
    return all(data.get(key) == value for key, value in expected_marker(model_id).items())


def read_marker(volume_root: PathLike, model_id: str = MODEL_PROD_FLUX) -> Optional[Dict[str, Any]]:
    """The marker when it exists and names exactly the pinned snapshot, else None."""
    try:
        raw = marker_path(volume_root, model_id).read_text(encoding="utf-8")
        data = json.loads(raw)
    except (OSError, ValueError):
        return None
    return data if marker_matches(data, model_id) else None


def is_seeded(volume_root: PathLike, model_id: str = MODEL_PROD_FLUX) -> bool:
    return read_marker(volume_root, model_id) is not None


def _default_snapshot_download(**kwargs: Any) -> Any:
    # Imported here so the gateway and the worker image never need huggingface_hub.
    from huggingface_hub import snapshot_download

    return snapshot_download(**kwargs)


def _bytes_under(directory: Path) -> int:
    total = 0
    for dirpath, _dirnames, filenames in os.walk(directory):
        for name in filenames:
            try:
                total += os.path.getsize(os.path.join(dirpath, name))
            except OSError:
                pass
    return total


def _safe_progress(progress: ProgressCallback, done: int, total: int) -> None:
    # Progress is informational; a failing reporter must never fail the download.
    try:
        progress(min(done, total), total)
    except Exception as exc:
        logger.warning("Seed progress report failed: %s", redact_text(str(exc)))


def _report_until(stop: threading.Event, directory: Path, progress: ProgressCallback, interval: float, total: int) -> None:
    while not stop.wait(interval):
        _safe_progress(progress, _bytes_under(directory), total)


def seed_snapshot(
    volume_root: PathLike,
    snapshot_download: Optional[Callable[..., Any]] = None,
    commit: Optional[Callable[[], Any]] = None,
    progress: Optional[ProgressCallback] = None,
    progress_interval: float = PROGRESS_INTERVAL_SECONDS,
    model_id: str = MODEL_PROD_FLUX,
) -> Dict[str, Any]:
    """Download the pinned snapshot into the volume once; idempotent.

    `commit` persists the volume (Modal needs an explicit commit). A second call on a
    seeded volume does no network work. `progress(done, total)` receives the bytes on
    disk every `progress_interval` seconds until the snapshot is verified and committed,
    so a caller that publishes it keeps a heartbeat going for the whole job.
    """
    root = Path(volume_root)
    model = production_model(model_id)
    if is_seeded(root, model_id):
        return {"status": "present", **expected_marker(model_id)}

    target = snapshot_dir(root, model_id)
    target.mkdir(parents=True, exist_ok=True)
    download = snapshot_download or _default_snapshot_download
    stop = threading.Event()
    reporter: Optional[threading.Thread] = None
    if progress is not None:
        _safe_progress(progress, _bytes_under(target), model.total_bytes)
        reporter = threading.Thread(
            target=_report_until, args=(stop, target, progress, progress_interval, model.total_bytes), daemon=True
        )
        reporter.start()
    try:
        download(
            repo_id=model.model_id,
            revision=model.revision,
            local_dir=str(target),
        )
        for adapter in model.adapters:
            download(
                repo_id=adapter.repo_id,
                revision=adapter.revision,
                local_dir=str(target / ADAPTER_DIR),
                allow_patterns=[adapter.filename],
            )
        problems = check_snapshot_files(target, model_id)
        if problems:
            raise WeightsCorruptError(
                "Downloaded snapshot does not match the pinned file list: " + "; ".join(problems[:5])
            )

        marker = marker_path(root, model_id)
        temp = marker.with_name(marker.name + ".tmp")
        temp.write_text(json.dumps(expected_marker(model_id), sort_keys=True), encoding="utf-8")
        os.replace(temp, marker)
        if commit is not None:
            commit()
    finally:
        stop.set()
        if reporter is not None:
            reporter.join(timeout=progress_interval + 1.0)
    if progress is not None:
        _safe_progress(progress, model.total_bytes, model.total_bytes)
    return {"status": "seeded", **expected_marker(model_id)}


def seed_state(
    state: str,
    bytes_done: int = 0,
    error_code: Optional[str] = None,
    message: Optional[str] = None,
    now: Optional[float] = None,
    model_id: str = MODEL_PROD_FLUX,
) -> Dict[str, Any]:
    """The seeding status document: state is running, done or failed.

    updated_at is the heartbeat. A running job rewrites the document every
    PROGRESS_INTERVAL_SECONDS, pausing only SEED_RETRY_DELAY_SECONDS between attempts,
    so the provisioner can tell a live job from one that stopped without a result.
    """
    model = production_model(model_id)
    return {
        "state": state,
        "bytes_done": int(bytes_done),
        "bytes_total": model.total_bytes,
        "model_revision": model.revision,
        "snapshot_fingerprint": snapshot_fingerprint(model_id),
        "error_code": error_code,
        "message": message,
        "updated_at": time.time() if now is None else now,
    }


def run_seed(
    volume_root: PathLike,
    store: Any,
    commit: Optional[Callable[[], Any]] = None,
    snapshot_download: Optional[Callable[..., Any]] = None,
    attempts: int = SEED_ATTEMPTS,
    retry_delay_seconds: float = SEED_RETRY_DELAY_SECONDS,
    progress_interval: float = PROGRESS_INTERVAL_SECONDS,
    sleep: Callable[[float], Any] = time.sleep,
    model_id: str = MODEL_PROD_FLUX,
) -> Dict[str, Any]:
    """Body of the provider seeding job: seed, publish progress, never raise.

    The status document in `store` (a KeyValueStore) is what the provisioner polls.
    A failure is returned as {"status": "failed", "error_code", "message"} so the
    provider does not retry blindly; the provisioner reports it and a later resume
    starts a new seeding job, which continues the partial download.
    """

    def publish(doc: Dict[str, Any]) -> None:
        try:
            store.put(SEED_STATE_KEY, doc)
        except Exception as exc:
            logger.warning("Seed state write failed: %s", redact_text(str(exc)))

    def on_progress(done: int, _total: int) -> None:
        publish(seed_state("running", done, model_id=model_id))

    last_error: Optional[BaseException] = None
    for attempt in range(max(1, attempts)):
        try:
            result = seed_snapshot(
                volume_root,
                snapshot_download=snapshot_download,
                commit=commit,
                progress=on_progress,
                progress_interval=progress_interval,
                model_id=model_id,
            )
        except WeightsError as exc:
            last_error = exc
        except Exception as exc:
            # Network and hub errors; snapshot_download resumes partial files next time.
            last_error = exc
        else:
            publish(seed_state("done", production_model(model_id).total_bytes, model_id=model_id))
            return result
        logger.warning("Seeding attempt %d failed: %s", attempt + 1, redact_text(str(last_error)))
        if attempt + 1 < max(1, attempts):
            sleep(retry_delay_seconds)

    code = last_error.error_code if isinstance(last_error, WeightsError) else "weights_download_failed"
    message = redact_text(f"Could not download the model weights: {last_error}")[:1024]
    publish(seed_state("failed", 0, error_code=code, message=message, model_id=model_id))
    return {"status": "failed", "error_code": code, "message": message}


def require_snapshot(
    volume_root: PathLike,
    reload: Optional[Callable[[], Any]] = None,
    attempts: int = 1,
    delay_seconds: float = 0.0,
    sleep: Callable[[float], Any] = time.sleep,
    model_id: str = MODEL_PROD_FLUX,
) -> Path:
    """Path of the seeded snapshot, or WeightsMissingError.

    `reload` refreshes the mount view between attempts (Modal Volume.reload). Beam
    volume writes can take up to a minute to show up in another container, so the
    Beam worker passes several attempts.
    """
    for attempt in range(max(1, attempts)):
        if is_seeded(volume_root, model_id):
            return snapshot_dir(volume_root, model_id)
        if attempt + 1 < max(1, attempts):
            if reload is not None:
                try:
                    reload()
                except Exception:
                    # A failed refresh is the same as "not visible yet"; the next check decides.
                    pass
            if delay_seconds > 0:
                sleep(delay_seconds)
    raise WeightsMissingError(
        "Model weights are not on the volume yet. Run the provisioner again to seed them."
    )
