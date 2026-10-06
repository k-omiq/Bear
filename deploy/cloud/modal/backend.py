"""Modal pieces of the dispatch backend.

`ModalDictStore` keeps the gateway's job documents in a modal.Dict, and
`ModalWorkerDispatcher` hands jobs to the GPU worker class through `spawn` and reads
their outcome back through `FunctionCall`. Nothing here imports modal: app.py passes
the SDK objects in, so the offline tests drive this code with fakes.
"""

from __future__ import annotations

import json
import logging
import threading
import time
from typing import Any, Callable, Dict, List, Optional, Tuple

from deploy.cloud.common.capacity import SYNC_GPU_WAIT_SECONDS
from deploy.cloud.common.contract import GPU_JOB_TERMINAL, JobRequestMetadata
from deploy.cloud.common.gpu import release_recent
from deploy.cloud.common.jobs import GpuJobConflict, WorkerOutcome, run_key
from deploy.cloud.common.redact import redact_text
from deploy.cloud.common.worker import (
    MAX_MESSAGE_CHARS,
    WorkerNotReady,
    WorkerRuntime,
    begin_run,
    not_ready_code,
    outcome_from_result,
)

logger = logging.getLogger("deploy.cloud.modal")


class ModalDictStore:
    """KeyValueStore on a modal.Dict (values are small plain dicts)."""

    def __init__(self, modal_dict: Any):
        self._dict = modal_dict

    def get(self, key: str) -> Optional[Dict[str, Any]]:
        value = self._dict.get(key, None)
        return dict(value) if isinstance(value, dict) else None

    def put(self, key: str, value: Dict[str, Any]) -> None:
        self._dict.put(key, dict(value))

    def put_if_absent(self, key: str, value: Dict[str, Any]) -> bool:
        return bool(self._dict.put(key, dict(value), skip_if_exists=True))

    def delete(self, key: str) -> None:
        self._dict.pop(key, None)


RENDER_STARTUP_FAILURE_KEY = "gpu:startup_failure:render"


class ModalWorkerDispatcher:
    """WorkerDispatcher over a Modal class method.

    `spawn` is `Worker().render.spawn`, `function_call_from_id` is
    `modal.FunctionCall.from_id` and `exceptions` is `modal.exception`.
    """

    def __init__(
        self,
        spawn: Callable[..., Any],
        function_call_from_id: Callable[[str], Any],
        exceptions: Any,
        store: Any,
    ):
        self._spawn = spawn
        self._from_id = function_call_from_id
        self._exc = exceptions
        self._store = store
        # Provider-side hiccups: the call's state is unknown, so the caller keeps the
        # last known status and asks again on the next poll.
        self._transient = tuple(
            getattr(exceptions, name)
            for name in ("ConnectionError", "ClientClosed", "ServiceError", "InternalError", "ResourceExhaustedError")
            if isinstance(getattr(exceptions, name, None), type)
        ) + (OSError,)

    def dispatch(self, handle: str, meta: JobRequestMetadata, image_png: bytes, hint_png: bytes) -> str:
        call = self._spawn(handle, json.dumps(meta.to_dict(), sort_keys=True), image_png, hint_png)
        return str(call.object_id)

    def poll(self, handle: str, native_id: str) -> WorkerOutcome:
        exc = self._exc
        try:
            result = self._from_id(native_id).get(timeout=0)
        except exc.OutputExpiredError:
            return WorkerOutcome("failed", error_code="result_expired", message="The provider no longer holds this job's result")
        except exc.FunctionTimeoutError:
            return WorkerOutcome("failed", error_code="worker_timeout", message="The GPU worker ran past its time limit")
        except TimeoutError:
            # The builtin TimeoutError (not modal's) means no output yet. It is also an
            # OSError, so it must be handled before the transient group.
            started = self._run_started(handle)
            if not started:
                failure = self._store.get(RENDER_STARTUP_FAILURE_KEY)
                job = self._store.get("job:" + handle)
                if failure and job and float(failure.get("at", 0)) >= float(job.get("created_at", 0)):
                    # Modal retries failed container initialization without giving the
                    # input an output. End this queued call rather than polling it forever.
                    self._from_id(native_id).cancel(terminate_containers=False)
                    return WorkerOutcome("failed", error_code=failure["error_code"], message=failure["message"])
            return WorkerOutcome("running" if started else "pending")
        except self._transient:
            raise
        except Exception as error:
            # Remote exceptions, crashed or cancelled inputs: the call is over and failed.
            # A container whose snapshot phase refused to start (WorkerNotReady, e.g.
            # weights not seeded yet) keeps its typed code.
            code = not_ready_code(error)
            if code is not None:
                text = redact_text(str(getattr(error, "detail", "") or error)) or code
                return WorkerOutcome("failed", error_code=code, message=text[:MAX_MESSAGE_CHARS])
            return WorkerOutcome("failed", error_code="worker_failed", message=f"GPU worker failed: {error}")
        return outcome_from_result(result)

    def _run_started(self, handle: str) -> bool:
        try:
            return self._store.get(run_key(handle)) is not None
        except Exception as error:
            logger.error("Run marker read failed for %s: %s", handle, redact_text(str(error)))
            return False

    def fetch_result(self, handle: str, native_id: str) -> Optional[bytes]:
        outcome = self.poll(handle, native_id)
        return outcome.png if outcome.state == "completed" else None

    def cancel(self, handle: str, native_id: str) -> None:
        # terminate_containers=False keeps the warm container and its loaded model;
        # Modal interrupts the running input instead.
        self._from_id(native_id).cancel(terminate_containers=False)

    def terminate(self, handle: str, native_id: str) -> None:
        # A GPU stop: the user wants the container gone, not kept warm.
        self._from_id(native_id).cancel(terminate_containers=True)


ANALYSIS_CALLS_KEY = "gpu:analysis_calls"
# Analysis tiles in flight at once, at most. The gateway takes 32 concurrent
# requests; anything past this bound is still cancelled by the container terminate
# the others trigger, since AnalysisGPU runs one container.
ANALYSIS_CALLS_LIMIT = 32
# The longest an AnalysisGPU call can live: its startup_timeout plus its timeout, both
# app.py ANALYSIS_TIMEOUT_SECONDS at their largest (600 s with page denoise on).
GPU_JOB_LONGEST_CALL_SECONDS = 2 * 600
# A record older than this is a call nobody will clear. A polled job (ModalGpuJobs)
# stays listed from its submit until a status or cancel request sees it end, so the
# expiry outlasts the longest call plus time queued for the container; a synchronous
# request clears its own record by SYNC_GPU_WAIT_SECONDS. Shorter would drop a slow
# denoise page from the list while it runs, out of reach of a stop and invisible to
# an idle release. A record left by a client that stopped polling holds an idle
# release back until it expires; the container still scales down on its own window.
ANALYSIS_CALL_EXPIRY_SECONDS = float(GPU_JOB_LONGEST_CALL_SECONDS + 600)

# The gateway serves concurrent requests on threads of one process
# (max_containers=1), so a process lock is enough to keep the read-modify-write of
# the call list from losing an entry.
_analysis_calls_lock = threading.Lock()


def _live_calls(store: Any, now: float) -> List[Dict[str, Any]]:
    doc = store.get(ANALYSIS_CALLS_KEY)
    calls = doc.get("calls") if isinstance(doc, dict) else None
    if not isinstance(calls, list):
        return []
    return [c for c in calls if isinstance(c, dict) and isinstance(c.get("id"), str) and c["id"]
            and isinstance(c.get("at"), (int, float)) and 0 <= now - c["at"] < ANALYSIS_CALL_EXPIRY_SECONDS]


def _write_calls(store: Any, calls: List[Dict[str, Any]]) -> None:
    if calls:
        store.put(ANALYSIS_CALLS_KEY, {"calls": calls[-ANALYSIS_CALLS_LIMIT:]})
    else:
        store.delete(ANALYSIS_CALLS_KEY)


def _track_call(store: Any, call_id: str, clock: Callable[[], float]) -> None:
    """List one analysis call in flight. Best effort; a store hiccup only means a stop
    cannot reach that call and an idle release does not wait for it."""
    try:
        with _analysis_calls_lock:
            _write_calls(store, _live_calls(store, clock()) + [{"id": call_id, "at": clock()}])
    except Exception as error:
        logger.error("Analysis call record failed: %s", redact_text(str(error)))


def _untrack_call(store: Any, call_id: str, clock: Callable[[], float]) -> None:
    try:
        with _analysis_calls_lock:
            _write_calls(store, [c for c in _live_calls(store, clock()) if c["id"] != call_id])
    except Exception as error:
        logger.error("Analysis call record clear failed: %s", redact_text(str(error)))


def run_tracked_analysis(
    spawn: Callable[..., Any],
    store: Any,
    metadata: Dict[str, Any],
    tile_png: bytes,
    clock: Callable[[], float] = time.time,
    admission: Optional[Callable[[], Any]] = None,
    wait_seconds: float = SYNC_GPU_WAIT_SECONDS,
) -> Dict[str, Any]:
    """Run one analysis tile (or denoise page) through `spawn` and wait, with its call
    id in the store. The synchronous routes only; an app that knows the job routes
    submits and polls through `ModalGpuJobs` instead.

    Every call in flight is listed, so a GPU stop can cancel all of them from another
    request, not only the last one recorded. The wait ends after `wait_seconds`, under
    Modal's 150 s redirect: the call is then cancelled (its container kept) and
    `GpuWaitExceeded` raised, since the app that sent it can no longer read an answer.
    """
    from contextlib import nullcontext
    from deploy.cloud.common.jobs import GpuWaitExceeded
    with admission() if admission is not None else nullcontext():
        call = spawn(metadata, tile_png)
        call_id = str(call.object_id)
        _track_call(store, call_id, clock)
    # Whether the call is over (answered, failed or cancelled), so its record goes. A
    # call whose cancel failed stays listed for a stop to reach.
    settled = True
    try:
        try:
            return call.get(timeout=wait_seconds)
        except TimeoutError as exc:
            # The builtin TimeoutError: no output yet (modal's own timeout errors are
            # not subclasses of it).
            try:
                call.cancel(terminate_containers=False)
            except Exception as error:
                settled = False
                logger.error("Analysis call cancel after the synchronous wait failed: %s", redact_text(str(error)))
            raise GpuWaitExceeded(f"No answer within {wait_seconds:g} s") from exc
    finally:
        if settled:
            _untrack_call(store, call_id, clock)


def count_tracked_analysis(store: Any, clock: Callable[[], float] = time.time) -> int:
    """How many analysis tiles are in flight, for an idle-only GPU release."""
    with _analysis_calls_lock:
        return len(_live_calls(store, clock()))


def cancel_tracked_analysis(
    store: Any,
    function_call_from_id: Callable[[str], Any],
    clock: Callable[[], float] = time.time,
) -> int:
    """Cancel every analysis tile in flight, with its container. Returns how many."""
    with _analysis_calls_lock:
        calls = _live_calls(store, clock())
        cancelled = 0
        remaining = []
        for entry in calls:
            try:
                function_call_from_id(entry["id"]).cancel(terminate_containers=True)
                cancelled += 1
            except Exception as error:
                # Kept: a later stop can try it again.
                remaining.append(entry)
                logger.error("Analysis call cancel failed: %s", redact_text(str(error)))
        _write_calls(store, remaining)
        if remaining:
            from deploy.cloud.common.jobs import StopUncertainError
            raise StopUncertainError("One or more analysis calls could not be confirmed stopped")
    return cancelled


# ---------------------------------------------------------------------------
# Submit-then-poll analysis tiles and denoise pages
# ---------------------------------------------------------------------------

GPU_JOB_KEY_PREFIX = "gpujob:"
# A job document with no call id this long after its claim lost its spawn: the
# gateway died between the claim and recording the call.
GPU_JOB_DISPATCH_GRACE_SECONDS = 120.0

# Read-modify-write of job documents, across the gateway's request threads (one
# process, max_containers=1), so a cancel and a status that both see a call end
# cannot write two different outcomes.
_gpu_jobs_lock = threading.Lock()


def gpu_job_key(handle: str) -> str:
    return f"{GPU_JOB_KEY_PREFIX}{handle}"


def _job_error(code: str, message: str) -> Dict[str, str]:
    return {"error_code": code, "message": ((redact_text(message) or code)[:MAX_MESSAGE_CHARS])}


def analysis_failure(error: BaseException) -> Tuple[str, str]:
    """A failed analysis call as the typed code its job reports."""
    from deploy.cloud.common.analysis_seed import AnalysisUnavailable
    if isinstance(error, AnalysisUnavailable):
        return "capability_unavailable", "Selected analysis graphs are unavailable"
    return "inference_failed", "Analysis inference failed"


def denoise_failure(error: BaseException) -> Tuple[str, str]:
    """A failed denoise call as the typed code its job reports, as the synchronous
    route maps the same errors."""
    from deploy.cloud.common.denoise import DenoiseError
    if isinstance(error, DenoiseError):
        if error.error_code == "weights_missing":
            return "capability_unavailable", str(error)
        if error.error_code == "unsupported_mode":
            return "unsupported_page", str(error)
    return "inference_failed", "Denoise inference failed"


def gpu_job_view(doc: Dict[str, Any]) -> Dict[str, Any]:
    return {"handle": doc["handle"], "request_digest": doc["request_digest"],
            "status": doc["status"], "error": doc.get("error")}


class ModalGpuJobs:
    """Analysis tiles and denoise pages as polled jobs on AnalysisGPU.

    A job is one spawned call plus one small document in the job Dict under
    `gpujob:<handle>`. Every method returns in the time of a few Dict reads and one
    spawn, never waiting on the GPU, so no gateway request comes near Modal's 150 s
    redirect however long the call queues or runs.

    - The handle is `gpu_job_handle(kind, request_digest, attempt_id)`. A submit whose
      handle already has a document spawns nothing and answers that job, so an app
      that repeats a submit after a lost answer never pays for the page twice.
    - The document is claimed (`put_if_absent`) inside the GPU admission, before the
      spawn, and the call id is recorded right after it. The call is listed under
      `ANALYSIS_CALLS_KEY` like a synchronous tile, so a GPU stop cancels it and an
      idle release waits for it, until a status or cancel request sees it end.
    - Status asks the call with `get(timeout=0)`. The output stays with Modal, so a
      repeated status of a completed job answers the result again.
    - A terminal document (completed, failed, cancelled) never changes again.
    """

    def __init__(
        self,
        store: Any,
        function_call_from_id: Callable[[str], Any],
        exceptions: Any,
        clock: Callable[[], float] = time.time,
    ):
        self._store = store
        self._from_id = function_call_from_id
        self._exc = exceptions
        self._clock = clock
        self._transient = tuple(
            getattr(exceptions, name)
            for name in ("ConnectionError", "ClientClosed", "ServiceError", "InternalError", "ResourceExhaustedError")
            if isinstance(getattr(exceptions, name, None), type)
        ) + (OSError,)

    # -- documents -------------------------------------------------------------

    def _read(self, kind: str, handle: str) -> Optional[Dict[str, Any]]:
        doc = self._store.get(gpu_job_key(handle))
        if not isinstance(doc, dict) or doc.get("kind") != kind or doc.get("handle") != handle:
            return None
        return doc

    def _update(self, handle: str, mutate: Callable[[Dict[str, Any]], None]) -> Optional[Dict[str, Any]]:
        """Re-read and mutate one document. A terminal document is final."""
        with _gpu_jobs_lock:
            doc = self._store.get(gpu_job_key(handle))
            if not isinstance(doc, dict):
                return None
            if doc["status"] in GPU_JOB_TERMINAL:
                return doc
            mutate(doc)
            doc["updated_at"] = self._clock()
            self._store.put(gpu_job_key(handle), doc)
            return doc

    def _end(self, handle: str, status: str, error: Optional[Dict[str, str]] = None) -> Optional[Dict[str, Any]]:
        def end(doc: Dict[str, Any]) -> None:
            doc["status"] = status
            doc["error"] = error
        return self._update(handle, end)

    @staticmethod
    def _same(existing: Dict[str, Any], kind: str, metadata: Dict[str, Any]) -> Dict[str, Any]:
        if existing.get("kind") != kind or existing.get("request_digest") != metadata["request_digest"]:
            raise GpuJobConflict("GPU job handle already names another request")
        return gpu_job_view(existing)

    # -- operations ------------------------------------------------------------

    def submit(
        self,
        kind: str,
        spawn: Callable[..., Any],
        metadata: Dict[str, Any],
        payload_png: bytes,
        attempt_id: str,
        admission: Optional[Callable[[], Any]] = None,
    ) -> Dict[str, Any]:
        """Spawn one call for this attempt, once. Raises what `admission` raises (a GPU
        just stopped) before anything is claimed."""
        from contextlib import nullcontext
        from deploy.cloud.common.contract import gpu_job_handle
        handle = gpu_job_handle(kind, metadata["request_digest"], attempt_id)
        key = gpu_job_key(handle)
        existing = self._store.get(key)
        if isinstance(existing, dict):
            return self._same(existing, kind, metadata)
        with admission() if admission is not None else nullcontext():
            now = self._clock()
            doc: Dict[str, Any] = {
                "v": 1, "kind": kind, "handle": handle, "request_digest": metadata["request_digest"],
                "call_id": None, "status": "pending", "error": None, "cancel_requested": False,
                "created_at": now, "updated_at": now,
            }
            if not self._store.put_if_absent(key, doc):
                existing = self._store.get(key)
                if not isinstance(existing, dict):
                    raise RuntimeError("GPU job store changed during submit")
                return self._same(existing, kind, metadata)
            try:
                call = spawn(metadata, payload_png)
            except Exception as error:
                # Modal may or may not have taken the call. The claim stays, so this
                # attempt never spawns twice; the job reports the failure.
                logger.error("GPU job spawn failed for %s: %s", handle, redact_text(str(error)))
                self._end(handle, "failed", _job_error("dispatch_failed", "The GPU call could not be confirmed queued"))
                raise
            call_id = str(call.object_id)
            _track_call(self._store, call_id, self._clock)

        def spawned(d: Dict[str, Any]) -> None:
            d["call_id"] = call_id
            d["status"] = "running"
        stored = self._update(handle, spawned) or {**doc, "call_id": call_id, "status": "running"}
        if stored.get("cancel_requested"):
            # A cancel arrived while the spawn ran; it could not reach the call then.
            return self._cancel_call(kind, handle, call_id) or gpu_job_view(stored)
        return gpu_job_view(stored)

    def status(
        self, kind: str, handle: str, failure: Callable[[BaseException], Tuple[str, str]],
    ) -> Optional[Dict[str, Any]]:
        """The job's state, with `result` (the worker's own dict) once completed.
        None: no such job of this kind."""
        doc = self._read(kind, handle)
        if doc is None:
            return None
        if doc["status"] in ("failed", "cancelled"):
            return gpu_job_view(doc)
        call_id = doc.get("call_id")
        if not call_id:
            if self._clock() - float(doc.get("created_at") or 0.0) > GPU_JOB_DISPATCH_GRACE_SECONDS:
                doc = self._end(handle, "failed", _job_error("dispatch_lost", "The job never reached the GPU queue")) or doc
            return gpu_job_view(doc)
        exc = self._exc
        try:
            result = self._from_id(call_id).get(timeout=0)
        except TimeoutError:
            # The builtin TimeoutError: no output yet. It is also an OSError, so it is
            # handled before the transient group.
            return gpu_job_view(doc)
        except exc.OutputExpiredError:
            return self._ended(handle, call_id, doc, "failed",
                               _job_error("result_expired", "The provider no longer holds this job's result"))
        except exc.FunctionTimeoutError:
            return self._ended(handle, call_id, doc, "failed",
                               _job_error("worker_timeout", "The GPU call ran past its time limit"))
        except self._transient as error:
            # Provider hiccup: the last known state; the next poll asks again.
            logger.error("GPU job poll failed for %s: %s", handle, redact_text(str(error)))
            return gpu_job_view(doc)
        except Exception as error:
            if doc.get("cancel_requested"):
                return self._ended(handle, call_id, doc, "cancelled", None)
            code, message = failure(error)
            logger.error("GPU job %s failed: %s", handle, redact_text(str(error)))
            return self._ended(handle, call_id, doc, "failed", _job_error(code, message))
        if doc["status"] != "completed":
            doc = self._end(handle, "completed") or doc
            _untrack_call(self._store, call_id, self._clock)
        if doc["status"] != "completed":
            # Cancelled or failed by another request while this one read the output.
            return gpu_job_view(doc)
        return {**gpu_job_view(doc), "result": result}

    def _ended(self, handle: str, call_id: str, doc: Dict[str, Any], status: str,
               error: Optional[Dict[str, str]]) -> Dict[str, Any]:
        if doc["status"] == "completed":
            # Answered before; its output is gone now. Say so without rewriting it.
            return {**gpu_job_view(doc), "status": "failed", "error": error or _job_error("result_expired", "Result gone")}
        stored = self._end(handle, status, error) or doc
        _untrack_call(self._store, call_id, self._clock)
        return gpu_job_view(stored)

    def cancel(self, kind: str, handle: str) -> Optional[Dict[str, Any]]:
        """Cancel the job's call, keeping the container and its loaded models. None: no
        such job. A call whose cancel fails stays `running` and listed for a stop."""
        doc = self._read(kind, handle)
        if doc is None:
            return None
        if doc["status"] in GPU_JOB_TERMINAL:
            return gpu_job_view(doc)
        doc = self._update(handle, lambda d: d.__setitem__("cancel_requested", True)) or doc
        call_id = doc.get("call_id")
        if doc["status"] in GPU_JOB_TERMINAL or not call_id:
            # Not spawned yet: the submit cancels it once it has the call id.
            return gpu_job_view(doc)
        return self._cancel_call(kind, handle, call_id) or gpu_job_view(doc)

    def _cancel_call(self, kind: str, handle: str, call_id: str) -> Optional[Dict[str, Any]]:
        try:
            self._from_id(call_id).cancel(terminate_containers=False)
        except Exception as error:
            logger.error("GPU job cancel failed for %s: %s", handle, redact_text(str(error)))
            return None
        stored = self._end(handle, "cancelled")
        _untrack_call(self._store, call_id, self._clock)
        return gpu_job_view(stored) if stored is not None else None



def prepare_render_container(
    store: Any,
    new_heartbeat: Callable[[], Any],
    new_runtime: Callable[[], WorkerRuntime],
    clock: Callable[[], float] = time.time,
) -> WorkerRuntime:
    """Body of Worker's first `@modal.enter`: a loaded, warmed runtime or a raise.

    With the Worker snapshot off (app.py WORKER_SNAPSHOT) this runs on every start.
    With it on, Modal runs this once per memory snapshot and snapshots only after it returns; an
    exception fails the container and no snapshot is taken (SDK 1.5.5,
    modal/_runtime/user_code_imports.py: `call_lifecycle_functions` raises through
    `handle_task_lifecycle_exception`, which runs before `maybe_snapshot`). So it
    never returns with the pipeline unloaded, and nothing here may be per start.

    - A container a stop's release call started (release marker recent) raises before
      loading anything, as the old enter skipped the load.
    - While it loads, a throwaway "starting" heartbeat is shown, so the long first
      load is visible. The real heartbeat, with its own container id, is written by
      `start_gpu_container` after the restore; a failed load removes the throwaway,
      because no exit hook runs for a container whose enter failed.
    """
    if release_recent(store, clock(), "render"):
        raise WorkerNotReady("released", "Started by a GPU stop; the model was not loaded")
    loading = new_heartbeat()
    loading.starting()
    try:
        runtime = new_runtime()
        runtime.load_for_snapshot()
    except BaseException as error:
        if isinstance(error, Exception):
            try:
                store.put(RENDER_STARTUP_FAILURE_KEY, {
                    "at": clock(), "error_code": not_ready_code(error) or "model_load_failed",
                    "message": redact_text(str(error))[:MAX_MESSAGE_CHARS],
                })
            except Exception as store_error:
                logger.error("Worker startup failure could not be recorded: %s", redact_text(str(store_error)))
        loading.clear()
        raise
    try:
        store.delete(RENDER_STARTUP_FAILURE_KEY)
    except Exception as error:
        logger.error("Old startup failure could not be cleared: %s", redact_text(str(error)))
    return runtime


def start_gpu_container(
    heartbeat: Any,
    runtime: Optional[WorkerRuntime] = None,
    clock: Callable[[], float] = time.time,
) -> bool:
    """Body of a GPU class's `@modal.enter(snap=False)`: runs on every container start.

    That includes each restore from a memory snapshot, which is why the heartbeat
    (with its container id and `started_at`) is created and written here and never in
    the snapshot phase. A container that starts right after a stop was most likely
    started by the stop's own release call: it stays out of the GPU status and, if
    its model is not loaded, does not load it (a render that does land here still
    loads on demand in WorkerRuntime.render). Returns False for such a container.
    """
    if release_recent(heartbeat.store, clock(), heartbeat.role):
        return False
    if runtime is not None and not runtime.loaded:
        heartbeat.starting()
        runtime.load()
    heartbeat.idle()
    return True


def worker_render(
    runtime: WorkerRuntime,
    store: Any,
    handle: str,
    metadata_json: str,
    image_png: bytes,
    hint_png: bytes,
    clock: Callable[[], float] = time.time,
) -> Dict[str, Any]:
    """Body of the GPU worker method: mark the run, honour a cancel, render."""
    cancelled = begin_run(store, handle, clock)
    if cancelled is not None:
        return cancelled
    return runtime.render(metadata_json, image_png, hint_png)
