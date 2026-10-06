"""Beam pieces of the dispatch backend.

Job documents, job inputs and results live in one beam.Map per installation. The
gateway enqueues a job on the GPU task queue over HTTP with the workspace token and
learns the outcome from the Map, where the worker writes it. Nothing here imports
beam: app.py passes the Map and the network callables in, so the offline tests drive
this code with fakes.

Why a Map rather than the weights volume or beam.Output: a Map write is visible to
the other container at once and is kept for up to 7 days, while a volume write can
take a minute to reach another container and an Output URL expires after an hour.
A Map value holds at most 1 MiB, so PNG bytes are stored in chunks.
"""

from __future__ import annotations

import hashlib
import json
import logging
import threading
import time
import urllib.request
from typing import Any, Callable, Dict, Optional

from deploy.cloud.common.contract import JobRequestMetadata
from deploy.cloud.common.deployment import JOB_TIMEOUT_SECONDS
from deploy.cloud.common.jobs import WorkerOutcome, job_key, run_key
from deploy.cloud.common.redact import redact_text
from deploy.cloud.common.worker import begin_run, failure, outcome_from_result

logger = logging.getLogger("deploy.cloud.beam")

# Chunk size stays well below the 1 MiB Map value limit after pickling.
CHUNK_BYTES = 768 * 1024
# Beam's maximum time to live for a Map key.
MAP_TTL_SECONDS = 604_800
# Beam fails a task that no container picked up within 20 minutes.
UNSTARTED_TIMEOUT_SECONDS = 20 * 60
# Slack on top of provider deadlines before the gateway gives up on a silent job.
STALE_GRACE_SECONDS = 120
ENQUEUE_TIMEOUT_SECONDS = 15.0
OUTCOME_WRITE_ATTEMPTS = 3

INPUT_KEY_PREFIX = "in:"
OUTPUT_KEY_PREFIX = "out:"


def input_key(handle: str, part: str = "") -> str:
    return f"{INPUT_KEY_PREFIX}{handle}" + (f":{part}" if part else "")


def output_key(handle: str, part: str = "") -> str:
    return f"{OUTPUT_KEY_PREFIX}{handle}" + (f":{part}" if part else "")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class BeamMapStore:
    """KeyValueStore on a beam.Map, plus chunked byte storage."""

    def __init__(self, beam_map: Any, ttl: int = MAP_TTL_SECONDS):
        self._map = beam_map
        self._ttl = ttl
        self._lock = threading.Lock()

    def get(self, key: str) -> Optional[Dict[str, Any]]:
        value = self._map.get(key)
        return dict(value) if isinstance(value, dict) else None

    def put(self, key: str, value: Dict[str, Any]) -> None:
        self._map.set(key, dict(value), ttl=self._ttl)

    def put_if_absent(self, key: str, value: Dict[str, Any]) -> bool:
        # A Map has no conditional write. Every writer of job documents is the gateway,
        # which runs as one container with one process (max_containers=1, workers=1),
        # so this lock serialises them.
        with self._lock:
            if self._map.get(key) is not None:
                return False
            self._map.set(key, dict(value), ttl=self._ttl)
            return True

    def delete(self, key: str) -> None:
        try:
            del self._map[key]
        except KeyError:
            pass

    def put_blob(self, key: str, data: bytes) -> Dict[str, Any]:
        """Store bytes under key:0, key:1, ... and return what get_blob needs."""
        data = bytes(data)
        chunks = [data[i : i + CHUNK_BYTES] for i in range(0, len(data), CHUNK_BYTES)] or [b""]
        for index, chunk in enumerate(chunks):
            self._map.set(f"{key}:{index}", chunk, ttl=self._ttl)
        return {"chunks": len(chunks), "bytes": len(data), "sha256": _sha256(data)}

    def get_blob(self, key: str, info: Any) -> Optional[bytes]:
        """The stored bytes, or None when a chunk is missing or the digest differs."""
        try:
            count, size, digest = int(info["chunks"]), int(info["bytes"]), str(info["sha256"])
        except (KeyError, TypeError, ValueError):
            return None
        parts = []
        for index in range(count):
            chunk = self._map.get(f"{key}:{index}")
            if not isinstance(chunk, (bytes, bytearray)):
                return None
            parts.append(bytes(chunk))
        data = b"".join(parts)
        if len(data) != size or _sha256(data) != digest:
            return None
        return data

    def delete_blob(self, key: str, info: Any) -> None:
        try:
            count = int(info["chunks"])
        except (KeyError, TypeError, ValueError):
            return
        for index in range(count):
            self.delete(f"{key}:{index}")


def make_http_enqueue(
    worker_url: str,
    token: Callable[[], str],
    urlopen: Callable[..., Any] = urllib.request.urlopen,
    timeout: float = ENQUEUE_TIMEOUT_SECONDS,
) -> Callable[[str], str]:
    """Enqueue one job on the deployed GPU task queue; returns Beam's task id.

    The task queue is deployed with authorized=True, so the call carries the
    workspace token from the installation's secret. The JSON body becomes the
    task's keyword arguments.
    """

    def enqueue(handle: str) -> str:
        if not worker_url:
            raise RuntimeError("The GPU worker URL is not configured")
        secret = token()
        if not secret:
            raise RuntimeError("The workspace token secret is not available to the gateway")
        request = urllib.request.Request(
            worker_url,
            data=json.dumps({"handle": handle}).encode("utf-8"),
            method="POST",
            headers={"Authorization": f"Bearer {secret}", "Content-Type": "application/json"},
        )
        with urlopen(request, timeout=timeout) as response:
            body = json.loads(response.read(65536).decode("utf-8"))
        task_id = body.get("task_id") if isinstance(body, dict) else None
        if not isinstance(task_id, str) or not task_id:
            raise RuntimeError("The GPU task queue did not return a task id")
        return task_id

    return enqueue


class BeamWorkerDispatcher:
    """WorkerDispatcher over the Beam GPU task queue and the installation Map.

    `enqueue(handle) -> task_id` posts to the task queue; `stop_task(task_id)` asks
    Beam to cancel a task and may be None. Beam reports a task's end only to callers
    holding the workspace token, so the outcome comes from the Map instead: the worker
    writes it there, and a job that stays silent past the provider deadlines fails.
    """

    def __init__(
        self,
        store: BeamMapStore,
        enqueue: Callable[[str], str],
        stop_task: Optional[Callable[[str], Any]] = None,
        clock: Callable[[], float] = time.time,
        job_timeout_seconds: int = JOB_TIMEOUT_SECONDS,
    ):
        self._store = store
        self._enqueue = enqueue
        self._stop_task = stop_task
        self._clock = clock
        self._job_timeout = job_timeout_seconds

    def dispatch(self, handle: str, meta: JobRequestMetadata, image_png: bytes, hint_png: bytes) -> str:
        manifest = {
            "metadata_json": json.dumps(meta.to_dict(), sort_keys=True),
            "image": self._store.put_blob(input_key(handle, "image"), image_png),
            "hint": self._store.put_blob(input_key(handle, "hint"), hint_png),
        }
        self._store.put(input_key(handle), manifest)
        try:
            return self._enqueue(handle)
        except Exception:
            discard_inputs(self._store, handle, manifest)
            raise

    def poll(self, handle: str, native_id: str) -> WorkerOutcome:
        outcome = self._store.get(output_key(handle))
        if outcome is not None:
            return self._read_outcome(handle, outcome)
        now = self._clock()
        run = self._store.get(run_key(handle))
        if run is not None:
            started = float(run.get("started_at") or 0.0)
            if now - started > self._job_timeout + STALE_GRACE_SECONDS:
                return WorkerOutcome(
                    "failed", error_code="worker_timeout", message="The GPU worker stopped before the job finished"
                )
            return WorkerOutcome("running")
        doc = self._store.get(job_key(handle)) or {}
        if doc.get("cancel_requested"):
            # No run marker yet, so the worker has not checked the flag; it writes its
            # marker before reading the flag and will refuse the job when it starts.
            return WorkerOutcome("cancelled")
        created = float(doc.get("created_at") or now)
        if now - created > UNSTARTED_TIMEOUT_SECONDS + STALE_GRACE_SECONDS:
            return WorkerOutcome(
                "failed", error_code="worker_unavailable", message="No GPU worker picked up the job in time"
            )
        return WorkerOutcome("pending")

    def _read_outcome(self, handle: str, outcome: Dict[str, Any]) -> WorkerOutcome:
        if outcome.get("ok") is True:
            png = self._store.get_blob(output_key(handle, "png"), outcome.get("png"))
            if png is None:
                return WorkerOutcome("failed", error_code="result_expired", message="The stored result is incomplete")
            return outcome_from_result({"ok": True, "png": png, "digest": outcome.get("digest")})
        return outcome_from_result(outcome)

    def fetch_result(self, handle: str, native_id: str) -> Optional[bytes]:
        outcome = self._store.get(output_key(handle))
        if outcome is None or outcome.get("ok") is not True:
            return None
        return self._store.get_blob(output_key(handle, "png"), outcome.get("png"))

    def cancel(self, handle: str, native_id: str) -> None:
        # The cancel flag in the job document already stops a job that has not
        # started; stopping the task also frees a queued slot or a running GPU.
        if self._stop_task is not None and native_id:
            self._stop_task(native_id)


def discard_inputs(store: BeamMapStore, handle: str, manifest: Optional[Dict[str, Any]]) -> None:
    """Best-effort removal of a job's input bytes; the Map TTL removes leftovers."""
    try:
        if manifest:
            store.delete_blob(input_key(handle, "image"), manifest.get("image"))
            store.delete_blob(input_key(handle, "hint"), manifest.get("hint"))
        store.delete(input_key(handle))
    except Exception as error:
        logger.warning("Could not remove inputs of %s: %s", handle, redact_text(str(error)))


def _render_inputs(runtime: Any, store: BeamMapStore, handle: str, manifest: Optional[Dict[str, Any]]) -> Dict[str, Any]:
    if not isinstance(manifest, dict) or not isinstance(manifest.get("metadata_json"), str):
        return failure("input_missing", "The job inputs are no longer stored")
    image = store.get_blob(input_key(handle, "image"), manifest.get("image"))
    hint = store.get_blob(input_key(handle, "hint"), manifest.get("hint"))
    if image is None or hint is None:
        return failure("input_missing", "The job inputs are incomplete")
    return runtime.render(manifest["metadata_json"], image, hint)


def _write_outcome(store: BeamMapStore, handle: str, result: Dict[str, Any], sleep: Callable[[float], Any]) -> None:
    for attempt in range(OUTCOME_WRITE_ATTEMPTS):
        try:
            if result.get("ok") is True:
                info = store.put_blob(output_key(handle, "png"), result["png"])
                store.put(output_key(handle), {"ok": True, "digest": result["digest"], "png": info})
            else:
                store.put(output_key(handle), dict(result))
            return
        except Exception as error:
            logger.error("Outcome write failed for %s: %s", handle, redact_text(str(error)))
            if attempt + 1 < OUTCOME_WRITE_ATTEMPTS:
                sleep(2.0)
    raise RuntimeError("Could not store the job outcome")


def beam_worker_render(
    runtime: Any,
    store: BeamMapStore,
    handle: str,
    clock: Callable[[], float] = time.time,
    sleep: Callable[[float], Any] = time.sleep,
) -> Dict[str, Any]:
    """Body of the GPU task: read the inputs, render, write the outcome to the Map.

    Returns a small summary without image bytes; Beam stores task results as JSON.
    """
    result = begin_run(store, handle, clock)
    manifest: Optional[Dict[str, Any]] = None
    try:
        manifest = store.get(input_key(handle))
        if result is None:
            result = _render_inputs(runtime, store, handle, manifest)
    except Exception as error:
        logger.error("Input read failed for %s: %s", handle, redact_text(str(error)))
        result = failure("input_unavailable", f"Could not read the job inputs: {error}")
    _write_outcome(store, handle, result, sleep)
    discard_inputs(store, handle, manifest)
    if result.get("ok") is True:
        return {"ok": True, "digest": result["digest"]}
    return {"ok": False, "error_code": result.get("error_code"), "message": result.get("message")}
