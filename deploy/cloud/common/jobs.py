"""Job backends behind the /mc/v1 gateway.

`LocalJobBackend` runs a worker in the gateway process. The offline tests use it with
the fake worker.

`DispatchJobBackend` is the deployed shape. The CPU gateway keeps one small job
document per handle in the provider's durable key-value store (Modal Dict, Beam Map)
and hands the crop to a GPU worker through a provider dispatcher. Status and results
are read back from the provider, so a restarted gateway container still answers for
every job, and health or model-info requests never touch the GPU.
"""

from __future__ import annotations

import hashlib
import logging
import threading
import time
from collections import OrderedDict
from dataclasses import dataclass
from typing import Any, Callable, Dict, Optional, Protocol, Tuple

from deploy.cloud.common.contract import (
    ContractValidationError,
    JobExecutionStatus,
    JobRequestMetadata,
    RenderRecipe,
    ServiceLimits,
    TypedError,
    validate_worker_result,
)
from deploy.cloud.common.handle_mapping import InMemoryHandleRegistry, JobRecord
from deploy.cloud.common.redact import redact_text

logger = logging.getLogger("deploy.cloud.jobs")


def execute_analysis_job(worker: Any, metadata: Dict[str, Any], tile_png: bytes) -> Dict[str, Any]:
    """Dispatch one explicitly submitted analysis tile without FLUX job state."""
    return worker.analyze(metadata, tile_png)

TERMINAL_STATUSES = (
    JobExecutionStatus.COMPLETED,
    JobExecutionStatus.FAILED,
    JobExecutionStatus.CANCELLED,
)

JOB_KEY_PREFIX = "job:"
RUN_KEY_PREFIX = "run:"
# Handles of jobs not yet terminal, so a GPU stop can find every queued call.
ACTIVE_JOBS_KEY = "gpu:active_jobs"


class PreEnqueueError(Exception):
    """Submission refused before anything was queued; maps to a pre-enqueue rejection."""

    def __init__(self, error_code: str, message: str, http_status: int = 503, retryable: bool = True):
        super().__init__(message)
        self.error_code = error_code
        self.http_status = http_status
        self.retryable = retryable


class SubmissionUnknownError(Exception):
    """The provider may have accepted a spawn; retrying this attempt is unsafe."""


class StopUncertainError(Exception):
    """At least one provider call could not be confirmed terminated."""


class ResultUnavailableError(Exception):
    """A completed job's PNG could not be read back from the provider right now."""


class GpuJobConflict(Exception):
    """A GPU job handle already names a different request. Not expected: the handle
    hashes the request digest (contract.py `gpu_job_handle`), so this is a digest
    collision or a forged document, and the submit is refused."""


class GpuWaitExceeded(Exception):
    """A synchronous analysis or denoise request outwaited SYNC_GPU_WAIT_SECONDS.

    Its GPU call was asked to cancel; the gateway answers 504 rather than let the
    provider cut the request off (Modal redirects at 150 s)."""


def job_key(handle: str) -> str:
    return f"{JOB_KEY_PREFIX}{handle}"


def run_key(handle: str) -> str:
    return f"{RUN_KEY_PREFIX}{handle}"


def dispatch_handle(provider: str, job_id: str, attempt_id: str) -> str:
    """Deterministic handle, so a resubmitted attempt lands on the same job document."""
    digest = hashlib.sha256(f"{job_id}\x00{attempt_id}".encode("utf-8")).hexdigest()[:32]
    return f"handle-{provider}-{digest}"


def execute_worker_job(
    handle_registry: InMemoryHandleRegistry,
    worker: Any,
    record: JobRecord,
    img_bytes: bytes,
    hint_bytes: bytes,
    limits: ServiceLimits,
    internal_storage_ref: Optional[str] = None,
    auto_execute: bool = True,
) -> bool:
    """Run the worker in-process, validate its output and persist result or typed failure."""
    if not auto_execute or worker is None:
        return False

    # Atomically claim PENDING -> RUNNING; refuses cancelled or terminal jobs.
    if not handle_registry.claim_execution(record.app_handle):
        return False
    cost = None
    try:
        result_bytes, result_digest, cost = worker.infer(record, img_bytes, hint_bytes)
        validate_worker_result(
            result_bytes=result_bytes,
            result_digest=result_digest,
            expected_width=record.width,
            expected_height=record.height,
            limits=limits,
            reported_cost_usd=cost,
        )
        handle_registry.store_result(
            app_handle=record.app_handle,
            result_data=result_bytes,
            result_digest=result_digest,
            reported_cost_usd=cost,
            internal_storage_ref=internal_storage_ref,
        )
        return True
    except Exception as exc:
        logger.error("Worker inference or result validation failed: %s", redact_text(str(exc)))
        handle_registry.fail_job(
            app_handle=record.app_handle,
            error_code="execution_failed",
            message=str(exc),
            cost=cost,
        )
        return False


class LocalJobBackend:
    """Registry plus in-process worker; the gateway's behaviour before dispatch existed."""

    def __init__(
        self,
        provider: str,
        registry: InMemoryHandleRegistry,
        worker: Any,
        limits: ServiceLimits,
        auto_execute: bool = True,
    ):
        self.provider = provider
        self.registry = registry
        self.worker = worker
        self.limits = limits
        self.auto_execute = auto_execute

    def run(self, record: JobRecord, img_bytes: bytes, hint_bytes: bytes, internal_storage_ref: Optional[str] = None) -> bool:
        return execute_worker_job(
            handle_registry=self.registry,
            worker=self.worker,
            record=record,
            img_bytes=img_bytes,
            hint_bytes=hint_bytes,
            limits=self.limits,
            internal_storage_ref=internal_storage_ref,
            auto_execute=self.auto_execute,
        )

    def submit(self, meta: JobRequestMetadata, image_png: bytes, hint_png: bytes) -> JobRecord:
        record = self.registry.register_job(
            job_meta=meta,
            native_handle=f"native-{self.provider}-{meta.job_id}-{meta.attempt_id}",
            provider=self.provider,
        )
        self.run(record, image_png, hint_png)
        return record

    def get(self, handle: str) -> Optional[JobRecord]:
        return self.registry.get_by_app_handle(handle)

    def result_png(self, handle: str) -> Tuple[Optional[JobRecord], Optional[bytes]]:
        record = self.registry.get_by_app_handle(handle)
        if record is None or record.status != JobExecutionStatus.COMPLETED or record.result_data is None:
            return record, None
        return record, record.result_data

    def cancel(self, handle: str) -> Tuple[Optional[JobRecord], bool]:
        record = self.registry.get_by_app_handle(handle)
        if record is None:
            return None, False
        if record.status in TERMINAL_STATUSES:
            return record, False
        updated = self.registry.request_cancel(record.app_handle)
        if updated is None:
            return self.registry.get_by_app_handle(handle) or record, False
        return updated, True

    def warmup(self) -> str:
        return str(self.worker.warmup().get("worker_state", "warm"))


# ---------------------------------------------------------------------------
# Dispatch backend
# ---------------------------------------------------------------------------


class KeyValueStore(Protocol):
    def get(self, key: str) -> Optional[Dict[str, Any]]: ...

    def put(self, key: str, value: Dict[str, Any]) -> None: ...

    def put_if_absent(self, key: str, value: Dict[str, Any]) -> bool: ...

    def delete(self, key: str) -> None: ...


class MemoryStore:
    """In-process KeyValueStore for tests and the local end-to-end harness."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._data: Dict[str, Dict[str, Any]] = {}

    def get(self, key: str) -> Optional[Dict[str, Any]]:
        with self._lock:
            value = self._data.get(key)
            return dict(value) if isinstance(value, dict) else value

    def put(self, key: str, value: Dict[str, Any]) -> None:
        with self._lock:
            self._data[key] = dict(value)

    def put_if_absent(self, key: str, value: Dict[str, Any]) -> bool:
        with self._lock:
            if key in self._data:
                return False
            self._data[key] = dict(value)
            return True

    def delete(self, key: str) -> None:
        with self._lock:
            self._data.pop(key, None)

    def keys(self):
        with self._lock:
            return list(self._data.keys())


@dataclass(frozen=True)
class WorkerOutcome:
    """What the provider says about one dispatched job."""

    state: str  # pending | running | completed | failed | cancelled
    png: Optional[bytes] = None
    digest: Optional[str] = None
    error_code: Optional[str] = None
    message: Optional[str] = None


class WorkerDispatcher(Protocol):
    def dispatch(self, handle: str, meta: JobRequestMetadata, image_png: bytes, hint_png: bytes) -> str: ...

    def poll(self, handle: str, native_id: str) -> WorkerOutcome: ...

    def fetch_result(self, handle: str, native_id: str) -> Optional[bytes]: ...

    def cancel(self, handle: str, native_id: str) -> None: ...


def _record_from_doc(provider: str, doc: Dict[str, Any]) -> JobRecord:
    error = doc.get("error")
    return JobRecord(
        app_handle=doc["handle"],
        native_handle=doc.get("native_id") or "",
        provider=provider,
        job_id=doc["job_id"],
        attempt_id=doc["attempt_id"],
        request_digest=doc["request_digest"],
        recipe=RenderRecipe.from_dict(doc["recipe"]),
        width=doc["width"],
        height=doc["height"],
        seed=doc["seed"],
        steps=doc["steps"],
        guidance_scaled=doc["guidance_scaled"],
        status=JobExecutionStatus(doc["status"]),
        cancel_requested=bool(doc.get("cancel_requested")),
        created_at=doc.get("created_at", 0.0),
        updated_at=doc.get("updated_at", 0.0),
        started_at=doc.get("started_at"),
        completed_at=doc.get("completed_at"),
        reported_cost_usd=doc.get("reported_cost_usd"),
        result_digest=doc.get("result_digest"),
        result_bytes=doc.get("result_bytes"),
        error=TypedError(error_code=error["error_code"], message=error["message"]) if error else None,
    )


def _typed_error(code: str, message: str) -> Dict[str, str]:
    return {"error_code": code, "message": (redact_text(message) or code)[:1024]}


class DispatchJobBackend:
    """Durable job documents plus a provider dispatcher."""

    # A document with no native id this long after creation lost its dispatch
    # (gateway died between storing the claim and recording the provider id).
    DISPATCH_GRACE_SECONDS = 120.0
    RESULT_CACHE_ENTRIES = 8

    def __init__(
        self,
        provider: str,
        store: KeyValueStore,
        dispatcher: WorkerDispatcher,
        limits: ServiceLimits,
        clock: Callable[[], float] = time.time,
        preflight: Optional[Callable[[], None]] = None,
    ):
        self.preflight = preflight
        self.provider = provider
        self.store = store
        self.dispatcher = dispatcher
        self.limits = limits
        self.clock = clock
        self._lock = threading.Lock()
        # Separate from _lock: a completed outcome is cached while _update holds _lock.
        self._results_lock = threading.Lock()
        self._results: "OrderedDict[str, bytes]" = OrderedDict()

    # -- helpers ---------------------------------------------------------------

    def _remember_png(self, handle: str, png: bytes) -> None:
        with self._results_lock:
            self._results[handle] = png
            self._results.move_to_end(handle)
            while len(self._results) > self.RESULT_CACHE_ENTRIES:
                self._results.popitem(last=False)

    def _cached_png(self, handle: str) -> Optional[bytes]:
        with self._results_lock:
            return self._results.get(handle)

    def _update(self, handle: str, mutate: Callable[[Dict[str, Any]], bool]) -> Optional[Dict[str, Any]]:
        """Re-read, mutate and write one document under the process lock.

        Terminal documents are never changed again, and a cancel flag set by another
        request between our read and write survives because we mutate the fresh copy.
        """
        with self._lock:
            doc = self.store.get(job_key(handle))
            if doc is None:
                return None
            if JobExecutionStatus(doc["status"]) in TERMINAL_STATUSES:
                return doc
            if mutate(doc):
                doc["updated_at"] = self.clock()
                self.store.put(job_key(handle), doc)
                if JobExecutionStatus(doc["status"]) in TERMINAL_STATUSES:
                    self._track_active(handle, active=False)
            return doc

    def _active_handles(self) -> list:
        doc = self.store.get(ACTIVE_JOBS_KEY) or {}
        handles = doc.get("handles")
        return [h for h in handles if isinstance(h, str)] if isinstance(handles, list) else []

    def active_count(self) -> int:
        """How many render jobs are still pending or running: the handles in the active
        index whose document is not terminal. A job turns terminal when it is polled, so
        one whose caller never polled again still counts until a GPU stop settles it.
        Store failures propagate; the caller decides what an unknown count means."""
        with self._lock:
            handles = self._active_handles()
        count = 0
        for handle in handles:
            doc = self.store.get(job_key(handle))
            if doc is not None and JobExecutionStatus(doc["status"]) not in TERMINAL_STATUSES:
                count += 1
        return count

    def _track_active(self, handle: str, active: bool, required: bool = False) -> None:
        """Add or drop one handle. A new job must be indexed before dispatch."""
        try:
            handles = [h for h in self._active_handles() if h != handle]
            if active:
                handles.append(handle)
            if handles:
                self.store.put(ACTIVE_JOBS_KEY, {"handles": handles})
            else:
                self.store.delete(ACTIVE_JOBS_KEY)
        except Exception as exc:
            logger.error("Active job index update failed for %s: %s", handle, redact_text(str(exc)))
            if required:
                raise

    # -- submit ----------------------------------------------------------------

    def submit(self, meta: JobRequestMetadata, image_png: bytes, hint_png: bytes) -> JobRecord:
        handle = dispatch_handle(self.provider, meta.job_id, meta.attempt_id)
        # A replay returns its durable job even if installation readiness changed.
        # New work must be ready before a claim or GPU call exists.
        if self.preflight is not None:
            try:
                existing = self.store.get(job_key(handle))
            except Exception as exc:
                raise PreEnqueueError("service_unavailable_pre_queue", "Job store unavailable") from exc
            if existing is None:
                self.preflight()
        now = self.clock()
        doc: Dict[str, Any] = {
            "v": 1,
            "handle": handle,
            "job_id": meta.job_id,
            "attempt_id": meta.attempt_id,
            "request_digest": meta.request_digest,
            "recipe": meta.recipe.to_dict(),
            "width": meta.width,
            "height": meta.height,
            "seed": meta.seed,
            "steps": meta.steps,
            "guidance_scaled": meta.guidance_scaled,
            "status": JobExecutionStatus.PENDING.value,
            "native_id": None,
            "cancel_requested": False,
            "created_at": now,
            "updated_at": now,
            "started_at": None,
            "completed_at": None,
            "reported_cost_usd": None,
            "result_digest": None,
            "result_bytes": None,
            "error": None,
        }
        try:
            claimed = self.store.put_if_absent(job_key(handle), doc)
        except Exception as exc:
            raise PreEnqueueError("service_unavailable_pre_queue", f"Job store unavailable: {exc}") from exc

        if not claimed:
            existing = self.store.get(job_key(handle))
            if existing is None:
                raise PreEnqueueError("service_unavailable_pre_queue", "Job store changed during submit")
            if existing["request_digest"] != meta.request_digest:
                raise ContractValidationError(
                    f"Attempt '{meta.attempt_id}' already registered with a different request digest"
                )
            return _record_from_doc(self.provider, existing)

        try:
            with self._lock:
                self._track_active(handle, active=True, required=True)
        except Exception as exc:
            try:
                self.store.delete(job_key(handle))
            except Exception as cleanup_error:
                raise SubmissionUnknownError("Job claim could not be cleaned after index failure") from cleanup_error
            raise PreEnqueueError("service_unavailable_pre_queue", "Job index unavailable") from exc
        try:
            native_id = self.dispatcher.dispatch(handle, meta, image_png, hint_png)
        except Exception as exc:
            # A failed reply from spawn does not say whether Modal accepted the call.
            # Keep the durable claim, so the same attempt cannot dispatch twice.
            logger.error("GPU submission outcome unknown for %s: %s", handle, redact_text(str(exc)))
            self._update(handle, lambda d: d.__setitem__("dispatch_unknown", True) or True)
            raise SubmissionUnknownError("GPU submission outcome unknown") from exc

        def record_native(d: Dict[str, Any]) -> bool:
            d["native_id"] = native_id
            return True

        stored = self._update(handle, record_native)
        if stored is not None and stored.get("cancel_requested"):
            self._cancel_native(handle, native_id)
        return _record_from_doc(self.provider, stored or {**doc, "native_id": native_id})

    # -- status ----------------------------------------------------------------

    def get(self, handle: str) -> Optional[JobRecord]:
        doc = self.store.get(job_key(handle))
        if doc is None:
            return None
        if JobExecutionStatus(doc["status"]) in TERMINAL_STATUSES:
            return _record_from_doc(self.provider, doc)

        native_id = doc.get("native_id")
        if not native_id:
            if not doc.get("dispatch_unknown") and self.clock() - float(doc.get("created_at") or 0.0) > self.DISPATCH_GRACE_SECONDS:
                doc = self._update(handle, lambda d: self._apply_failure(d, "dispatch_lost", "The job never reached the GPU queue")) or doc
            return _record_from_doc(self.provider, doc)

        try:
            outcome = self.dispatcher.poll(handle, native_id)
        except Exception as exc:
            # Provider hiccup: report the last known state; the next poll asks again.
            logger.error("Status poll failed for %s: %s", handle, redact_text(str(exc)))
            return _record_from_doc(self.provider, doc)

        updated = self._update(handle, lambda d: self._apply_outcome(d, outcome))
        return _record_from_doc(self.provider, updated or doc)

    def _apply_failure(self, doc: Dict[str, Any], code: str, message: str) -> bool:
        now = self.clock()
        doc["status"] = JobExecutionStatus.FAILED.value
        doc["error"] = _typed_error(code, message)
        doc["completed_at"] = now
        return True

    def _apply_outcome(self, doc: Dict[str, Any], outcome: WorkerOutcome) -> bool:
        now = self.clock()
        state = outcome.state
        if state == "running":
            if doc["status"] == JobExecutionStatus.RUNNING.value:
                return False
            doc["status"] = JobExecutionStatus.RUNNING.value
            doc["started_at"] = doc.get("started_at") or now
            return True
        if state == "completed":
            png = outcome.png or b""
            try:
                validate_worker_result(png, outcome.digest or "", doc["width"], doc["height"], self.limits)
            except ContractValidationError as exc:
                return self._apply_failure(doc, "invalid_output", str(exc))
            self._remember_png(doc["handle"], png)
            doc["status"] = JobExecutionStatus.COMPLETED.value
            doc["result_digest"] = outcome.digest
            doc["result_bytes"] = len(png)
            doc["completed_at"] = now
            doc["started_at"] = doc.get("started_at") or now
            return True
        if state == "cancelled" or (state == "failed" and doc.get("cancel_requested")):
            doc["status"] = JobExecutionStatus.CANCELLED.value
            doc["completed_at"] = now
            return True
        if state == "failed":
            return self._apply_failure(doc, outcome.error_code or "worker_failed", outcome.message or "Worker failed")
        return False

    # -- result ----------------------------------------------------------------

    def result_png(self, handle: str) -> Tuple[Optional[JobRecord], Optional[bytes]]:
        record = self.get(handle)
        if record is None or record.status != JobExecutionStatus.COMPLETED:
            return record, None
        png = self._cached_png(handle)
        if png is None:
            try:
                png = self.dispatcher.fetch_result(handle, record.native_handle)
            except Exception as exc:
                raise ResultUnavailableError(redact_text(str(exc))) from exc
            if png is None:
                raise ResultUnavailableError("The provider no longer holds this result")
        if hashlib.sha256(png).hexdigest() != record.result_digest:
            raise ResultUnavailableError("Result bytes no longer match the recorded digest")
        self._remember_png(handle, png)
        return record, png

    # -- cancel ----------------------------------------------------------------

    def cancel(self, handle: str) -> Tuple[Optional[JobRecord], bool]:
        with self._lock:
            doc = self.store.get(job_key(handle))
            if doc is None:
                return None, False
            if JobExecutionStatus(doc["status"]) in TERMINAL_STATUSES:
                return _record_from_doc(self.provider, doc), False
            doc["cancel_requested"] = True
            doc["updated_at"] = self.clock()
            self.store.put(job_key(handle), doc)
        if doc.get("native_id"):
            self._cancel_native(handle, doc["native_id"])
        return _record_from_doc(self.provider, doc), True

    def _cancel_native(self, handle: str, native_id: str) -> None:
        try:
            self.dispatcher.cancel(handle, native_id)
        except Exception as exc:
            # The flag stays set; the worker checks it before rendering and the next
            # status poll reports cancelled once the provider agrees.
            logger.error("Provider cancel failed for %s: %s", handle, redact_text(str(exc)))

    def stop_active(self) -> int:
        """End every job still running on the GPU as cancelled and terminate its call.

        The provider is asked first: a job the GPU already finished, but which the app
        has not polled yet, keeps its completed (or failed) outcome rather than being
        thrown away. Only a job still pending or running is cancelled, and its
        document turns terminal only after its provider call is confirmed terminated.
        A call whose state or termination is unknown stays tracked for a later stop.
        Returns how many jobs this ended, or raises when any stop is uncertain.
        """
        with self._lock:
            try:
                handles = self._active_handles()
            except Exception as exc:
                logger.error("Active job index read failed: %s", redact_text(str(exc)))
                raise StopUncertainError("Render job index could not be read") from exc
        ended = 0
        uncertain = False
        for handle in handles:
            try:
                current = self.store.get(job_key(handle))
            except Exception as exc:
                logger.error("Could not read %s during GPU stop: %s", handle, redact_text(str(exc)))
                uncertain = True
                continue
            if current is None:
                with self._lock:
                    self._track_active(handle, active=False)
                continue
            if JobExecutionStatus(current["status"]) in TERMINAL_STATUSES:
                with self._lock:
                    self._track_active(handle, active=False)
                continue
            native_id = current.get("native_id")
            if native_id:
                try:
                    outcome: Optional[WorkerOutcome] = self.dispatcher.poll(handle, native_id)
                except Exception as exc:
                    logger.error("Status poll failed for %s during GPU stop: %s", handle, redact_text(str(exc)))
                    outcome = None
                if outcome is not None and outcome.state in ("completed", "failed", "cancelled"):
                    finished = outcome
                    try:
                        self._update(handle, lambda d: self._apply_outcome(d, finished))
                    except Exception as exc:
                        logger.error("Could not record %s during GPU stop: %s", handle, redact_text(str(exc)))
                    continue

            try:
                if native_id:
                    self._terminate_native(handle, native_id)
                else:
                    # A spawn with no recorded native id may already be running.
                    uncertain = True
                    continue
                def cancel_now(d: Dict[str, Any]) -> bool:
                    d["cancel_requested"] = True
                    d["status"] = JobExecutionStatus.CANCELLED.value
                    d["completed_at"] = self.clock()
                    return True
                doc = self._update(handle, cancel_now)
            except Exception as exc:
                logger.error("Could not cancel %s during GPU stop: %s", handle, redact_text(str(exc)))
                uncertain = True
                continue
            if doc is None:
                with self._lock:
                    self._track_active(handle, active=False)
                continue
            if JobExecutionStatus(doc["status"]) == JobExecutionStatus.CANCELLED:
                ended += 1
        if uncertain:
            raise StopUncertainError("One or more render calls could not be confirmed stopped")
        return ended

    def _terminate_native(self, handle: str, native_id: str) -> None:
        terminate = getattr(self.dispatcher, "terminate", None)
        if callable(terminate):
            terminate(handle, native_id)
        else:
            self.dispatcher.cancel(handle, native_id)

    def warmup(self) -> str:
        # Warm-up never starts a GPU: the worker scales from zero on the first job.
        return "on_demand"
