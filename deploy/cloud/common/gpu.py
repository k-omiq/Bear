"""Whether a cloud GPU container is up, and stopping it, without waking a GPU.

The provider scales GPU containers to zero after the idle window, and nothing in its
public API tells a CPU gateway cheaply whether one is warm. So each GPU container
publishes a small **heartbeat** document in the job store it already writes to:

    gpu:render / gpu:analysis -> {state, gpu, started_at, last_active_at,
                                  idle_seconds, timeout_seconds, container}

written when the container starts, at the start and end of every call, and removed
when it exits. The heartbeat is advisory: a container can die without running its
exit hook, so a reader treats a document as gone once its own clock is past the time
the container would have scaled down (`read_heartbeat`). Health and model-info never
read it; only `GET /mc/v1/gpu` does, and that route never starts a GPU either.

Stopping (`GpuControl.stop`, one role or both) ends what is up, as the user asked:

1. every render job still running on the GPU is marked cancelled in the store and its
   provider call is cancelled with its container terminated, so the app sees the job
   end as cancelled instead of waiting on it. A job the GPU already finished keeps its
   result (`DispatchJobBackend.stop_active` asks the provider first);
2. every analysis tile in flight is cancelled the same way;
3. a warm container that is not running anything is asked to leave through a small
   `release` call, which stops the container fetching further inputs.

A heartbeat is removed here only for a container this stop terminated. A released
container removes its own when it leaves, so until then the status still says it is
up, which is true: it bills until it has gone.

A release can race the provider's own scale-down: the container may already be gone
when the call is spawned, and the call then starts a fresh one. The stop therefore
leaves a per-role `gpu:release_requested_at:<role>` marker first, and a GPU container of that
role that starts within `RELEASE_WINDOW_SECONDS` of it skips its model load, so a stray
cold start exits after the release instead of loading weights. The next real
submission for that role clears its marker (`GpuControl.before_dispatch`).

An **idle-only** stop (`GpuControl.stop(role, idle_only=True)`) is the app's own
end-of-batch call: an Auto clean run that sends its pages' detection to the analysis
GPU asks for it once its last page has been analyzed, so the GPU does not bill its
idle window while the computer cleans, and a batch cloud clean asks for the render
GPU's once its last region is back. It cancels nothing, writes no release marker (so
no refusal window for the next tile and no skipped model load), and sends a role's
`release` only when that role's heartbeat says idle and no work of that role is
tracked in flight: for analysis no tile, for render no job pending or running in the
backend's active index (another device's render queued behind an idle heartbeat). Anything else is left exactly as it is. Without the marker
a release that races the provider's own scale-down can cold-start a container that
loads its model and then leaves; the idle heartbeat makes that window a few seconds
at most, and it costs one short start, never a stuck GPU.

Heartbeat writes are advisory and best effort. Marker reads and writes fail closed:
an unknown stop or dispatch admission cannot be reported as a confirmed stop.
"""

from __future__ import annotations

import logging
import math
import re
import threading
import time
import uuid
from contextlib import contextmanager
from typing import Any, Callable, Dict, List, Mapping, Optional

from deploy.cloud.common.contract import GPU_ROLES, GPU_STATES
from deploy.cloud.common.redact import redact_text

logger = logging.getLogger("deploy.cloud.gpu")

RELEASE_KEY = "gpu:release_requested_at"

# A heartbeat is trusted this long past the moment its container should have gone.
STALE_GRACE_SECONDS = 30.0
# A GPU container starting this soon after a stop skips its model load.
RELEASE_WINDOW_SECONDS = 60.0
# Analysis tiles are refused this long after a stop, so a batch the user stopped
# between two tiles does not start the GPU again with its next tile.
ANALYSIS_REFUSE_SECONDS = 15.0

_GPU_NAME = re.compile(r"^[A-Za-z0-9_-]{1,32}$")
_MAX_WINDOW_SECONDS = 24 * 3600


def heartbeat_key(role: str) -> str:
    return f"gpu:{role}"


def release_key(role: str) -> str:
    return f"{RELEASE_KEY}:{role}"


def _finite(value: Any) -> Optional[float]:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    return number if math.isfinite(number) and number >= 0 else None


def _seconds(value: Any) -> Optional[int]:
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    return value if 0 < value <= _MAX_WINDOW_SECONDS else None


class GpuHeartbeat:
    """The heartbeat one GPU container publishes. Never raises."""

    def __init__(
        self,
        store: Any,
        role: str,
        gpu: str,
        idle_seconds: int,
        startup_timeout_seconds: int,
        call_timeout_seconds: int,
        clock: Callable[[], float] = time.time,
    ):
        if role not in GPU_ROLES:
            raise ValueError(f"unknown GPU role '{role}'")
        self.store = store
        self.role = role
        self.gpu = gpu
        self.idle_seconds = int(idle_seconds)
        self.startup_timeout_seconds = int(startup_timeout_seconds)
        self.call_timeout_seconds = int(call_timeout_seconds)
        self.clock = clock
        # Distinguishes this container's document from a successor's, so an exiting
        # container never deletes the heartbeat of the one that replaced it.
        self.container = uuid.uuid4().hex
        self.started_at: Optional[float] = None
        # Calls running now. A container serves several inputs at once
        # (@modal.concurrent), so it is idle only when the last of them ends.
        self._calls = 0
        self._calls_lock = threading.Lock()

    def _write(self, state: str) -> None:
        now = self.clock()
        if self.started_at is None:
            self.started_at = now
        timeout = self.startup_timeout_seconds if state == "starting" else self.call_timeout_seconds
        doc = {
            "v": 1,
            "role": self.role,
            "state": state,
            "gpu": self.gpu,
            "started_at": self.started_at,
            "last_active_at": now,
            "idle_seconds": self.idle_seconds,
            "timeout_seconds": timeout,
            "container": self.container,
        }
        try:
            self.store.put(heartbeat_key(self.role), doc)
        except Exception as error:
            logger.error("GPU heartbeat write failed (%s): %s", self.role, redact_text(str(error)))

    def starting(self) -> None:
        self._write("starting")

    def idle(self) -> None:
        self._write("idle")

    def busy(self) -> None:
        self._write("busy")

    @contextmanager
    def serving(self):
        """Around one call: busy while any call runs, idle once the last one ends.

        The count and the write happen under one lock, so a call that ends never
        writes "idle" over the "busy" of a call that started meanwhile, and an
        idle-only stop never sees a busy container as idle.
        """
        with self._calls_lock:
            self._calls += 1
            self._write("busy")
        try:
            yield
        finally:
            with self._calls_lock:
                self._calls -= 1
                self._write("busy" if self._calls else "idle")

    @property
    def calls(self) -> int:
        return self._calls

    def clear(self) -> None:
        try:
            doc = self.store.get(heartbeat_key(self.role))
            if doc is not None and doc.get("container") == self.container:
                self.store.delete(heartbeat_key(self.role))
        except Exception as error:
            logger.error("GPU heartbeat clear failed (%s): %s", self.role, redact_text(str(error)))


def read_heartbeat(store: Any, role: str, now: float) -> Optional[Dict[str, Any]]:
    """The heartbeat of `role` as the wire shows it, or None when absent, stale or malformed."""
    doc = store.get(heartbeat_key(role))
    if not isinstance(doc, dict):
        return None
    state = doc.get("state")
    gpu = doc.get("gpu")
    started_at = _finite(doc.get("started_at"))
    last_active_at = _finite(doc.get("last_active_at"))
    idle_seconds = _seconds(doc.get("idle_seconds"))
    timeout_seconds = _seconds(doc.get("timeout_seconds"))
    if (state not in GPU_STATES or not isinstance(gpu, str) or not _GPU_NAME.match(gpu)
            or started_at is None or last_active_at is None or idle_seconds is None
            or timeout_seconds is None or last_active_at < started_at):
        return None
    # A starting or busy container is doing something that may take up to its
    # timeout, then idles for the scale-down window like any other.
    busy_for = 0 if state == "idle" else timeout_seconds
    if now > last_active_at + busy_for + idle_seconds + STALE_GRACE_SECONDS:
        return None
    return {
        "role": role,
        "gpu": gpu,
        "state": state,
        "started_at": started_at,
        "last_active_at": last_active_at,
        "idle_seconds": idle_seconds,
        "scaledown_estimate_at": last_active_at + idle_seconds if state == "idle" else None,
    }


def release_requested_at(store: Any, role: str) -> Optional[float]:
    doc = store.get(release_key(role))
    return _finite(doc.get("at")) if isinstance(doc, dict) else None


def release_recent(store: Any, now: float, role: str, window: float = RELEASE_WINDOW_SECONDS) -> bool:
    """Whether a stop asked `role`'s GPU container to leave within `window` seconds of `now`."""
    at = release_requested_at(store, role)
    return at is not None and 0 <= now - at < window


def release_container(heartbeat: Optional[GpuHeartbeat], stop_fetching_inputs: Callable[[], Any]) -> Dict[str, Any]:
    """Body of a GPU class's `release` method: stop inputs; exit hook clears heartbeat."""
    stop_fetching_inputs()
    return {"ok": True}


class GpuControl:
    """Status and stop for one deployment's GPU containers.

    `backend` is the gateway's job backend; its `stop_active()` cancels the render jobs
    still running and returns how many it ended. `release` maps a role to a callable
    that spawns that class's `release` method. `cancel_analysis` cancels every analysis
    call in flight and returns how many; `analysis_in_flight` only counts them, for an
    idle-only stop. Everything provider-specific arrives as a callable, so the offline
    tests drive this class with fakes.
    """

    def __init__(
        self,
        provider: str,
        store: Any,
        gpu: str,
        backend: Optional[Any] = None,
        release: Optional[Mapping[str, Callable[[], Any]]] = None,
        cancel_analysis: Optional[Callable[[], int]] = None,
        analysis_in_flight: Optional[Callable[[], int]] = None,
        list_prices: Optional[Mapping[str, float]] = None,
        clock: Callable[[], float] = time.time,
    ):
        self.provider = provider
        self.store = store
        self.gpu = gpu
        self.backend = backend
        self.release = dict(release or {})
        self.cancel_analysis = cancel_analysis
        self.analysis_in_flight = analysis_in_flight
        self.list_prices = dict(list_prices or {})
        self.clock = clock
        self._admission_lock = threading.RLock()

    def _prices(self) -> Dict[str, float]:
        price = self.list_prices.get(self.gpu)
        return {self.gpu: float(price)} if _finite(price) is not None else {}

    def status(self) -> Dict[str, Any]:
        now = self.clock()
        try:
            containers = [view for role in GPU_ROLES if (view := read_heartbeat(self.store, role, now)) is not None]
        except Exception as error:
            logger.error("GPU status store read failed: %s", redact_text(str(error)))
            containers = []
            supported = False
        else:
            supported = True
        return {
            "provider": self.provider,
            "supported": supported,
            "now": now,
            "containers": containers,
            "list_price_usd_per_hour": self._prices(),
        }

    def stop(self, role: Optional[str] = None, idle_only: bool = False) -> Dict[str, Any]:
        """Stop `role`'s container, or both when None. Idempotent.

        `idle_only` releases only what is idle and touches nothing else; see the
        module docs.
        """
        with self._admission_lock:
            return self._stop_locked(role, idle_only)

    def _stop_locked(self, role: Optional[str], idle_only: bool) -> Dict[str, Any]:
        if role is not None and role not in GPU_ROLES:
            raise ValueError(f"unknown GPU role '{role}'")
        roles = GPU_ROLES if role is None else (role,)
        if idle_only:
            return self._release_idle(roles)
        now = self.clock()
        from deploy.cloud.common.jobs import StopUncertainError
        try:
            for each in roles:
                self.store.put(release_key(each), {"at": now})
        except Exception as error:
            raise StopUncertainError("GPU stop marker could not be recorded") from error

        terminated: List[str] = []
        cancelled_jobs = 0
        if "render" in roles and self.backend is not None and hasattr(self.backend, "stop_active"):
            try:
                cancelled_jobs = int(self.backend.stop_active())
            except Exception as error:
                logger.error("Stopping render jobs failed: %s", redact_text(str(error)))
                raise StopUncertainError("Render job stop could not be confirmed") from error
            if cancelled_jobs:
                terminated.append("render")
        if "analysis" in roles and self.cancel_analysis is not None:
            try:
                if int(self.cancel_analysis()) > 0:
                    terminated.append("analysis")
            except Exception as error:
                logger.error("Stopping analysis calls failed: %s", redact_text(str(error)))
                raise StopUncertainError("Analysis call stop could not be confirmed") from error

        stopped = set(terminated)
        for each in roles:
            try:
                view = read_heartbeat(self.store, each, now)
            except Exception as error:
                raise StopUncertainError("GPU heartbeat could not be read") from error
            if view is None:
                continue
            if view["state"] == "busy" and each in terminated:
                # Its running call was cancelled with its container, so it is going
                # now and cannot remove its own heartbeat. A release would only start
                # a new container to deliver itself.
                try:
                    self.store.delete(heartbeat_key(each))
                except Exception as error:
                    logger.error("GPU heartbeat clear failed (%s): %s", each, redact_text(str(error)))
                continue
            spawn = self.release.get(each)
            if spawn is None:
                continue
            try:
                spawn()
            except Exception as error:
                logger.error("GPU release spawn failed (%s): %s", each, redact_text(str(error)))
                continue
            # Asked to leave; its heartbeat stays until it has, since it bills until then.
            stopped.add(each)
        return {
            "provider": self.provider,
            "stopped": [each for each in GPU_ROLES if each in stopped],
            "cancelled_jobs": cancelled_jobs,
        }

    def _release_idle(self, roles: tuple) -> Dict[str, Any]:
        now = self.clock()
        stopped: List[str] = []
        for each in roles:
            try:
                view = read_heartbeat(self.store, each, now)
            except Exception as error:
                logger.error("GPU idle release heartbeat read failed: %s", redact_text(str(error)))
                continue
            if view is None or view["state"] != "idle":
                continue
            if each == "analysis" and self._analysis_busy():
                continue
            if each == "render" and self._render_busy():
                continue
            spawn = self.release.get(each)
            if spawn is None:
                continue
            try:
                spawn()
            except Exception as error:
                logger.error("GPU idle release spawn failed (%s): %s", each, redact_text(str(error)))
                continue
            stopped.append(each)
        return {"provider": self.provider, "stopped": stopped, "cancelled_jobs": 0}

    def _analysis_busy(self) -> bool:
        """Whether an analysis tile is tracked in flight. A count that cannot be read
        is treated as busy: an idle release is an optimisation, never worth a tile."""
        if self.analysis_in_flight is None:
            return False
        try:
            return int(self.analysis_in_flight()) > 0
        except Exception as error:
            logger.error("Counting analysis calls failed: %s", redact_text(str(error)))
            return True

    def _render_busy(self) -> bool:
        """Whether a render job is pending or running. A count that cannot be read is
        treated as busy, as for analysis; a backend that keeps no index has none."""
        if self.backend is None or not hasattr(self.backend, "active_count"):
            return False
        try:
            return int(self.backend.active_count()) > 0
        except Exception as error:
            logger.error("Counting render jobs failed: %s", redact_text(str(error)))
            return True

    def before_dispatch(self, role: str) -> bool:
        """Called before a new GPU call. False refuses it (an analysis just stopped).

        Otherwise clears the release marker, so the container this call starts loads
        its model as usual.
        """
        with self._admission_lock:
            at = release_requested_at(self.store, role)
            if at is None:
                return True
            if 0 <= self.clock() - at < ANALYSIS_REFUSE_SECONDS:
                return False
            self.store.delete(release_key(role))
            return True

    def submit_render(self, backend: Any, meta: Any, image_png: bytes, hint_png: bytes) -> Any:
        """Keep render admission and its durable claim together against a stop."""
        from deploy.cloud.common.jobs import PreEnqueueError
        with self._admission_lock:
            if not self.before_dispatch("render"):
                raise PreEnqueueError("service_unavailable_pre_queue", "The GPU was just stopped")
            return backend.submit(meta, image_png, hint_png)

    @contextmanager
    def analysis_admission(self):
        """Hold stop until an analysis call has spawned and recorded its ID."""
        from deploy.cloud.common.analysis_seed import AnalysisUnavailable
        with self._admission_lock:
            if not self.before_dispatch("analysis"):
                raise AnalysisUnavailable("The GPU was just stopped")
            yield
