"""GPU worker runtime shared by the Modal and Beam workers.

One instance lives per GPU container. It loads the pinned pipeline from the weights
volume once (`copies` times when the container serves several renders at once, see
common/capacity.py) and then renders jobs. Every failure comes back as a typed result instead
of an exception, so the provider records the job as finished and the gateway can show
the user a stable error code. A container whose load failed tries again on its next
job, which is how it recovers once the weights volume is seeded.
"""

from __future__ import annotations

import json
import logging
import re
import threading
import time
from contextlib import contextmanager
from typing import Any, Callable, Dict, Iterator, List, Optional, Tuple

from deploy.cloud.common.capacity import exclusive_render
from deploy.cloud.common.contract import ContractValidationError, JobRequestMetadata, ServiceLimits
from deploy.cloud.common.flux import SdnqFluxRunner, WorkerRenderError, render_job, working_size
from deploy.cloud.common.jobs import WorkerOutcome, job_key, run_key
from deploy.cloud.common.manifest import MODEL_PROD_FLUX, production_limits, production_model
from deploy.cloud.common.redact import redact_text
from deploy.cloud.common.weights import WeightsError, require_snapshot

logger = logging.getLogger("deploy.cloud.worker")

MAX_MESSAGE_CHARS = 1024
_ERROR_CODE = re.compile(r"^[a-z][a-z0-9_]{0,63}$")


def failure(error_code: str, message: str) -> Dict[str, Any]:
    """The typed failure a worker returns; messages are redacted and bounded."""
    text = redact_text(str(message)) or error_code
    return {"ok": False, "error_code": error_code, "message": text[:MAX_MESSAGE_CHARS]}


def outcome_from_result(result: Any) -> WorkerOutcome:
    """Turn what a worker returned into a WorkerOutcome, trusting nothing about its shape."""
    if not isinstance(result, dict):
        return WorkerOutcome("failed", error_code="invalid_output", message="The GPU worker returned an unexpected value")
    if result.get("ok") is True:
        png, digest = result.get("png"), result.get("digest")
        if not isinstance(png, (bytes, bytearray)) or not isinstance(digest, str):
            return WorkerOutcome("failed", error_code="invalid_output", message="The GPU worker returned no image")
        return WorkerOutcome("completed", png=bytes(png), digest=digest)
    code = result.get("error_code")
    if not isinstance(code, str) or not _ERROR_CODE.match(code):
        code = "worker_failed"
    message = result.get("message")
    text = redact_text(message) if isinstance(message, str) and message else "The GPU worker failed"
    return WorkerOutcome("failed", error_code=code, message=text[:MAX_MESSAGE_CHARS])


NOT_READY_TAG = "[worker_not_ready:"


class WorkerNotReady(RuntimeError):
    """Raised by a GPU container's snapshot phase when its pipeline is not loaded.

    Modal takes the memory snapshot only after every `@modal.enter(snap=True)` method
    has returned, and an exception there fails the container instead
    (modal/_runtime/user_code_imports.py, `lifecycle_context` and
    `call_lifecycle_functions`). Raising is therefore how the snapshot phase refuses
    to freeze an unloaded pipeline into every later restore. Modal hands the
    exception to the call that started the container, so `error_code` travels with
    it (pickled) and in the text (for when it arrives only as a string).
    """

    def __init__(self, error_code: str, message: str):
        super().__init__(f"{NOT_READY_TAG}{error_code}] {message}")
        self.error_code = error_code
        self.detail = message

    def __reduce__(self) -> Any:
        return (type(self), (self.error_code, self.detail))


_NOT_READY = re.compile(re.escape(NOT_READY_TAG) + r"([a-z][a-z0-9_]{0,63})\]")


def not_ready_code(error: BaseException) -> Optional[str]:
    """The typed code of a WorkerNotReady, whether it arrived as itself or as text."""
    code = getattr(error, "error_code", None)
    if isinstance(error, WorkerNotReady) and isinstance(code, str) and _ERROR_CODE.match(code):
        return code
    match = _NOT_READY.search(str(error))
    return match.group(1) if match else None


def begin_run(store: Any, handle: str, clock: Callable[[], float] = time.time) -> Optional[Dict[str, Any]]:
    """Mark a job as started on the GPU, then honour a cancel that arrived first.

    Returns the typed "cancelled" failure when the job must not render, else None.
    The marker is written before the cancel flag is read: a gateway that finds no
    marker after setting the flag may report the job cancelled, because this read
    will then see the flag. Store access is best effort; a store hiccup must not fail
    a render the user is waiting for, it only delays the "running" status.
    """
    try:
        store.put(run_key(handle), {"started_at": clock()})
    except Exception as error:
        logger.error("Run marker write failed for %s: %s", handle, redact_text(str(error)))
    try:
        doc = store.get(job_key(handle))
    except Exception as error:
        logger.error("Job document read failed for %s: %s", handle, redact_text(str(error)))
        doc = None
    if doc is not None and doc.get("cancel_requested"):
        return failure("cancelled", "The job was cancelled before it started")
    return None


def runner_for(model_id: str) -> Callable[[str], Any]:
    """The runner class that renders a production model's recipe."""
    if production_model(model_id).runner == "qwen":
        from deploy.cloud.common.qwen import QwenEditRunner

        return QwenEditRunner
    return SdnqFluxRunner


class RunnerPool:
    """Loaded pipeline copies; each render borrows one and no two renders share one.

    diffusers pipelines keep per-call state (scheduler timesteps, progress, hooks), so
    a copy serves one render at a time. An exclusive borrow waits for every copy and
    holds them all, so a large render runs with nothing beside it on the GPU; while one
    waits, no new shared borrow starts, so it cannot be starved.
    """

    def __init__(self, runners: List[Any]):
        if not runners:
            raise ValueError("a runner pool needs at least one runner")
        self._runners = list(runners)
        self._free = list(runners)
        self._cond = threading.Condition()
        self._exclusive_waiting = 0

    @property
    def size(self) -> int:
        return len(self._runners)

    @property
    def runners(self) -> List[Any]:
        return list(self._runners)

    def add(self, runner: Any) -> None:
        with self._cond:
            self._runners.append(runner)
            self._free.append(runner)
            self._cond.notify_all()

    @contextmanager
    def borrow(self, exclusive: bool = False) -> Iterator[Any]:
        with self._cond:
            if exclusive:
                self._exclusive_waiting += 1
                try:
                    while len(self._free) < len(self._runners):
                        self._cond.wait()
                finally:
                    self._exclusive_waiting -= 1
                taken = list(self._free)
                self._free.clear()
            else:
                while not self._free or self._exclusive_waiting:
                    self._cond.wait()
                taken = [self._free.pop()]
        try:
            yield taken[0]
        finally:
            with self._cond:
                self._free.extend(taken)
                self._cond.notify_all()


def parse_metadata(metadata_json: str) -> JobRequestMetadata:
    try:
        payload = json.loads(metadata_json)
    except (TypeError, ValueError) as exc:
        raise ContractValidationError(f"Job metadata is not JSON: {exc}") from exc
    return JobRequestMetadata.from_dict(payload)


class WorkerRuntime:
    def __init__(
        self,
        weights_root: str,
        reload: Optional[Callable[[], Any]] = None,
        runner_factory: Optional[Callable[[str], Any]] = None,
        limits: Optional[ServiceLimits] = None,
        marker_attempts: int = 1,
        marker_delay_seconds: float = 0.0,
        sleep: Callable[[float], Any] = time.sleep,
        analysis_worker: Optional[Any] = None,
        model_id: str = MODEL_PROD_FLUX,
        copies: int = 1,
    ):
        self.weights_root = weights_root
        self.reload = reload
        self.runner_factory = runner_factory or runner_for(model_id)
        self.limits = limits or production_limits()
        self.marker_attempts = marker_attempts
        self.marker_delay_seconds = marker_delay_seconds
        self.sleep = sleep
        self.runner: Any = None
        self.analysis_worker = analysis_worker
        self.model_id = model_id
        self.load_error: Optional[Tuple[str, str]] = None
        self.copies = max(1, int(copies))
        self.pool: Optional[RunnerPool] = None
        self._snapshot: Any = None
        # Concurrent renders on a container whose enter did not load must not each
        # load a pipeline of their own.
        self._load_lock = threading.Lock()

    @property
    def loaded(self) -> bool:
        return self.runner is not None

    def load_for_snapshot(self) -> None:
        """Body of a snapshot phase: load and warm the pipeline, or raise WorkerNotReady.

        Never returns with the pipeline unloaded, so a memory snapshot taken after it
        always holds a loaded, warmed pipeline. A warmup failure raises as well: a
        CUDA context left broken by it must not be frozen into every restore.
        """
        if not self.load():
            code, message = self.load_error or ("model_load_failed", "Model not loaded")
            raise WorkerNotReady(code, message)
        pool = self.pool
        for index, runner in enumerate(pool.runners if pool is not None else [self.runner]):
            warmup = getattr(runner, "warmup", None)
            if warmup is None:
                continue
            try:
                warmup()
            except WorkerRenderError as exc:
                raise WorkerNotReady(exc.error_code, f"Warmup failed (copy {index + 1}): {exc}") from exc
            except Exception as exc:
                raise WorkerNotReady("model_load_failed", f"Warmup failed (copy {index + 1}): {exc}") from exc

    def load(self) -> bool:
        """Load the pipeline copies; on failure remember the typed reason and return False.

        The first copy is required. Each further copy is best effort: one that fails
        to load (out of memory, most likely) is logged and the container serves with
        the copies it has, one render per copy, instead of failing.
        """
        if self.runner is not None:
            return True
        with self._load_lock:
            if self.runner is not None:
                return True
            if not self._load_first():
                return False
            self._load_extra_copies()
            return True

    def _load_extra_copies(self) -> None:
        pool = self.pool
        while pool is not None and pool.size < self.copies:
            try:
                runner = self.runner_factory(str(self._snapshot))
                runner.load()
            except Exception as exc:
                logger.error("Pipeline copy %d of %d did not load; serving with %d: %s",
                             pool.size + 1, self.copies, pool.size, redact_text(str(exc)))
                return
            pool.add(runner)

    def _load_first(self) -> bool:
        try:
            snapshot = require_snapshot(
                self.weights_root,
                reload=self.reload,
                attempts=self.marker_attempts,
                delay_seconds=self.marker_delay_seconds,
                sleep=self.sleep,
                model_id=self.model_id,
            )
            runner = self.runner_factory(str(snapshot))
            runner.load()
            self._snapshot = snapshot
        except WeightsError as exc:
            self.load_error = (exc.error_code, str(exc))
        except WorkerRenderError as exc:
            self.load_error = (exc.error_code, str(exc))
        except Exception as exc:
            self.load_error = ("model_load_failed", f"Failed to load the model: {exc}")
        else:
            self.pool = RunnerPool([runner])
            self.runner = runner
            self.load_error = None
            return True
        logger.error("Worker load failed: %s", redact_text(self.load_error[1]))
        return False

    def render(self, metadata_json: str, image_png: bytes, hint_png: bytes) -> Dict[str, Any]:
        """Render one job. Returns {"ok": True, "png", "digest"} or a typed failure."""
        try:
            meta = parse_metadata(metadata_json)
        except ContractValidationError as exc:
            return failure("invalid_request", str(exc))
        if not self.load():
            code, message = self.load_error or ("model_load_failed", "Model not loaded")
            return failure(code, message)
        pool = self.pool or RunnerPool([self.runner])
        exclusive = pool.size > 1 and exclusive_render(*working_size(meta.width, meta.height))
        try:
            rendered = self._render_on(pool, exclusive, meta, image_png, hint_png)
        except WorkerRenderError as exc:
            if exc.error_code != "out_of_memory" or exclusive or pool.size < 2:
                return failure(exc.error_code, str(exc))
            # Out of memory beside another render: run it again alone. The seed is
            # fixed, so the retry gives the bytes the first try would have.
            logger.error("Render ran out of memory beside another; retrying alone")
            try:
                rendered = self._render_on(pool, True, meta, image_png, hint_png)
            except WorkerRenderError as retry:
                return failure(retry.error_code, str(retry))
            except Exception as retry:
                logger.error("Unexpected render failure: %s", redact_text(str(retry)))
                return failure("inference_failed", f"Unexpected render failure: {retry}")
        except Exception as exc:
            logger.error("Unexpected render failure: %s", redact_text(str(exc)))
            return failure("inference_failed", f"Unexpected render failure: {exc}")
        return {"ok": True, "png": rendered.png, "digest": rendered.digest}

    def _render_on(self, pool: RunnerPool, exclusive: bool, meta: JobRequestMetadata,
                   image_png: bytes, hint_png: bytes) -> Any:
        with pool.borrow(exclusive=exclusive) as runner:
            return render_job(runner, meta, bytes(image_png), bytes(hint_png), self.limits)

    def analyze(self, metadata: Dict[str, Any], tile_png: bytes) -> Dict[str, Any]:
        if self.analysis_worker is None:
            raise ContractValidationError("analysis capability unavailable")
        return self.analysis_worker.analyze(metadata, tile_png)
