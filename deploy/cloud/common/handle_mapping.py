"""In-Memory Native-Handle Mapping and Job Registry for Offline Testing.

Provides an ephemeral, in-memory thread-safe registry mapping application-level
handles to provider-native handles (e.g. Modal FunctionCall IDs, Beam Task IDs)
while tracking execution states, timestamps, and securely retaining results.

NOTE: InMemoryHandleRegistry is strictly ephemeral/in-memory for local testing.
Production cloud deployments require an external durable backend (e.g. Redis,
PostgreSQL, DynamoDB, or durable object store index).
"""

from __future__ import annotations

import dataclasses
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional

from deploy.cloud.common.contract import (
    ContractValidationError,
    JobExecutionStatus,
    JobRequestMetadata,
    RenderRecipe,
    TypedError,
)
from deploy.cloud.common.redact import redact_text


@dataclass
class JobRecord:
    app_handle: str
    native_handle: str
    provider: str
    job_id: str
    attempt_id: str
    request_digest: str
    recipe: RenderRecipe
    width: int
    height: int
    seed: int
    steps: int
    guidance_scaled: int
    status: JobExecutionStatus = JobExecutionStatus.PENDING
    cancel_requested: bool = False
    created_at: float = field(default_factory=time.time)
    updated_at: float = field(default_factory=time.time)
    started_at: Optional[float] = None
    completed_at: Optional[float] = None
    reported_cost_usd: Optional[float] = None
    result_digest: Optional[str] = None
    result_bytes: Optional[int] = None
    result_data: Optional[bytes] = None
    internal_storage_ref: Optional[str] = None  # Internal presigned link / store ref (never exposed in API)
    error: Optional[TypedError] = None

    def to_dict(self, include_result_bytes: bool = False) -> Dict[str, Any]:
        """Convert record to dict, strictly omitting raw result bytes and internal storage URLs."""
        d = {
            "app_handle": self.app_handle,
            "native_handle": self.native_handle,
            "provider": self.provider,
            "job_id": self.job_id,
            "attempt_id": self.attempt_id,
            "request_digest": self.request_digest,
            "recipe": self.recipe.to_dict(),
            "width": self.width,
            "height": self.height,
            "seed": self.seed,
            "steps": self.steps,
            "guidance_scaled": self.guidance_scaled,
            "status": self.status.value,
            "cancel_requested": self.cancel_requested,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "started_at": self.started_at,
            "completed_at": self.completed_at,
            "reported_cost_usd": self.reported_cost_usd,
            "result_digest": self.result_digest,
            "result_bytes": self.result_bytes,
            "error": self.error.to_dict() if self.error else None,
        }
        if include_result_bytes and self.result_data:
            d["result_data_len"] = len(self.result_data)
        return d


class InMemoryHandleRegistry:
    """Thread-safe in-memory registry for mapping application handles to native provider handles."""

    def __init__(self):
        self._lock = threading.Lock()
        self._by_app_handle: Dict[str, JobRecord] = {}
        self._by_native_handle: Dict[str, str] = {}  # native_handle -> app_handle
        self._by_job_attempt: Dict[str, str] = {}   # f"{job_id}:{attempt_id}" -> app_handle

    def register_job(
        self,
        job_meta: JobRequestMetadata,
        native_handle: str,
        app_handle: Optional[str] = None,
        provider: str = "modal",
    ) -> JobRecord:
        """Register a new job and establish bidirectional handle mapping."""
        with self._lock:
            key = f"{job_meta.job_id}:{job_meta.attempt_id}"
            if key in self._by_job_attempt:
                existing_app_handle = self._by_job_attempt[key]
                existing_record = self._by_app_handle[existing_app_handle]
                if existing_record.request_digest != job_meta.request_digest:
                    raise ContractValidationError(
                        f"Attempt '{job_meta.attempt_id}' already registered with a different request digest"
                    )
                return existing_record

            if not app_handle:
                app_handle = f"handle-{provider}-{uuid.uuid4().hex}"

            record = JobRecord(
                app_handle=app_handle,
                native_handle=native_handle,
                provider=provider,
                job_id=job_meta.job_id,
                attempt_id=job_meta.attempt_id,
                request_digest=job_meta.request_digest,
                recipe=job_meta.recipe,
                width=job_meta.width,
                height=job_meta.height,
                seed=job_meta.seed,
                steps=job_meta.steps,
                guidance_scaled=job_meta.guidance_scaled,
                status=JobExecutionStatus.PENDING,
            )

            self._by_app_handle[app_handle] = record
            self._by_native_handle[native_handle] = app_handle
            self._by_job_attempt[key] = app_handle
            return record

    def get_by_app_handle(self, app_handle: str) -> Optional[JobRecord]:
        """Lookup a job record by application-visible handle."""
        with self._lock:
            return self._by_app_handle.get(app_handle)

    def get_by_native_handle(self, native_handle: str) -> Optional[JobRecord]:
        """Lookup a job record by provider native handle."""
        with self._lock:
            app_h = self._by_native_handle.get(native_handle)
            if app_h:
                return self._by_app_handle.get(app_h)
            return None

    def get_by_job_attempt(self, job_id: str, attempt_id: str) -> Optional[JobRecord]:
        """Lookup a job record by logical job_id and attempt_id."""
        with self._lock:
            key = f"{job_id}:{attempt_id}"
            app_h = self._by_job_attempt.get(key)
            if app_h:
                return self._by_app_handle.get(app_h)
            return None

    def update_status(
        self,
        app_handle: str,
        status: JobExecutionStatus,
        cost: Optional[float] = None,
        error: Optional[TypedError] = None,
    ) -> Optional[JobRecord]:
        """Update job status and timestamps."""
        with self._lock:
            rec = self._by_app_handle.get(app_handle)
            if not rec:
                return None

            now = time.time()
            rec.status = status
            rec.updated_at = now
            if status == JobExecutionStatus.RUNNING and rec.started_at is None:
                rec.started_at = now
            elif status in (JobExecutionStatus.COMPLETED, JobExecutionStatus.FAILED, JobExecutionStatus.CANCELLED):
                rec.completed_at = now

            if cost is not None:
                rec.reported_cost_usd = cost
            if error is not None:
                rec.error = error
            return rec

    def claim_execution(self, app_handle: str) -> bool:
        """Atomically claim job for execution and transition PENDING -> RUNNING under lock.

        Refuses transition and returns False if the job is not found, cancel was requested,
        or status is not PENDING (e.g. already RUNNING, COMPLETED, FAILED, or CANCELLED).
        Returns True if the claim succeeded and caller has exclusive execution rights.
        """
        with self._lock:
            rec = self._by_app_handle.get(app_handle)
            if not rec:
                return False
            if rec.cancel_requested:
                return False
            if rec.status != JobExecutionStatus.PENDING:
                return False

            now = time.time()
            rec.status = JobExecutionStatus.RUNNING
            rec.updated_at = now
            if rec.started_at is None:
                rec.started_at = now
            return True

    claim_for_execution = claim_execution

    def request_cancel(self, app_handle: str) -> Optional[JobRecord]:
        """Flag job for cancellation without prematurely marking it terminal. Does not mutate terminal jobs."""
        with self._lock:
            rec = self._by_app_handle.get(app_handle)
            if not rec:
                return None
            if rec.status in (JobExecutionStatus.COMPLETED, JobExecutionStatus.FAILED, JobExecutionStatus.CANCELLED):
                return None

            rec.cancel_requested = True
            rec.updated_at = time.time()
            return rec

    def store_result(
        self,
        app_handle: str,
        result_data: bytes,
        result_digest: str,
        reported_cost_usd: Optional[float] = None,
        internal_storage_ref: Optional[str] = None,
    ) -> Optional[JobRecord]:
        """Store validated result buffer and complete job."""
        with self._lock:
            rec = self._by_app_handle.get(app_handle)
            if not rec:
                return None

            now = time.time()
            rec.status = JobExecutionStatus.COMPLETED
            rec.result_data = result_data
            rec.result_digest = result_digest
            rec.result_bytes = len(result_data)
            rec.reported_cost_usd = reported_cost_usd
            rec.internal_storage_ref = internal_storage_ref
            rec.completed_at = now
            rec.updated_at = now
            return rec

    def fail_job(
        self,
        app_handle: str,
        error_code: str,
        message: str,
        cost: Optional[float] = None,
    ) -> Optional[JobRecord]:
        """Mark job as failed with a sanitized typed error."""
        with self._lock:
            rec = self._by_app_handle.get(app_handle)
            if not rec:
                return None

            now = time.time()
            rec.status = JobExecutionStatus.FAILED
            rec.error = TypedError(
                error_code=error_code,
                message=redact_text(message)[:1024],
            )
            rec.reported_cost_usd = cost
            rec.completed_at = now
            rec.updated_at = now
            return rec

    def list_jobs(self) -> List[JobRecord]:
        """List all tracked job records."""
        with self._lock:
            return list(self._by_app_handle.values())

    def export_snapshot(self) -> List[Dict[str, Any]]:
        """Export serialized records for persistence/durability testing."""
        with self._lock:
            return [r.to_dict(include_result_bytes=False) for r in self._by_app_handle.values()]


# Backward-compatible alias for existing code
DurableHandleRegistry = InMemoryHandleRegistry
