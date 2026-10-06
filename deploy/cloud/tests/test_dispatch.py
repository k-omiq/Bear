"""Dispatch backend and the Modal and Beam dispatchers, driven through fakes."""

from __future__ import annotations

import io
import json
import tempfile
import unittest
from pathlib import Path
from typing import Any, Dict, List, Optional

from deploy.cloud.beam.backend import (
    CHUNK_BYTES,
    MAP_TTL_SECONDS,
    STALE_GRACE_SECONDS,
    UNSTARTED_TIMEOUT_SECONDS,
    BeamMapStore,
    BeamWorkerDispatcher,
    beam_worker_render,
    input_key,
    make_http_enqueue,
    output_key,
)
from deploy.cloud.common.contract import ContractValidationError, JobExecutionStatus
from deploy.cloud.common.deployment import JOB_TIMEOUT_SECONDS
from deploy.cloud.common.flux import encode_png_rgb8
from deploy.cloud.common.jobs import (
    ACTIVE_JOBS_KEY,
    DispatchJobBackend,
    MemoryStore,
    PreEnqueueError,
    ResultUnavailableError,
    WorkerOutcome,
    dispatch_handle,
    job_key,
    run_key,
)
from deploy.cloud.common.manifest import production_limits
from deploy.cloud.common.worker import WorkerRuntime
from deploy.cloud.modal.backend import ModalDictStore, ModalWorkerDispatcher
from deploy.cloud.tests.support import (
    FakeBeamMap,
    FakeFunctionCall,
    FakeModalDict,
    ModalErrors,
    FakeRunner,
    make_job,
    sha256,
    silence_deploy_logs,
    write_marker,
)

_restore_logs = None


def setUpModule() -> None:
    global _restore_logs
    _restore_logs = silence_deploy_logs()


def tearDownModule() -> None:
    _restore_logs()


class Clock:
    def __init__(self, now: float = 1000.0):
        self.now = now

    def __call__(self) -> float:
        return self.now


class FakeDispatcher:
    def __init__(self) -> None:
        self.dispatched: List[str] = []
        self.cancelled: List[str] = []
        self.outcome = WorkerOutcome("pending")
        self.result: Optional[bytes] = None
        self.dispatch_error: Optional[Exception] = None
        self.poll_error: Optional[Exception] = None
        self.cancel_error: Optional[Exception] = None

    def dispatch(self, handle, meta, image_png, hint_png) -> str:
        if self.dispatch_error is not None:
            raise self.dispatch_error
        self.dispatched.append(handle)
        return f"native-{len(self.dispatched)}"

    def poll(self, handle, native_id) -> WorkerOutcome:
        if self.poll_error is not None:
            raise self.poll_error
        return self.outcome

    def fetch_result(self, handle, native_id) -> Optional[bytes]:
        return self.result

    def cancel(self, handle, native_id) -> None:
        self.cancelled.append(native_id)
        if self.cancel_error is not None:
            raise self.cancel_error


class DispatchJobBackendTest(unittest.TestCase):
    def setUp(self) -> None:
        self.store = MemoryStore()
        self.dispatcher = FakeDispatcher()
        self.clock = Clock()
        self.backend = DispatchJobBackend("modal", self.store, self.dispatcher, production_limits(), clock=self.clock)
        self.meta, self.image, self.hint = make_job()

    def test_missing_weights_rejects_before_claim_or_gpu_dispatch(self) -> None:
        def unready():
            raise PreEnqueueError("weights_missing", "Repair the installed weights", retryable=False)
        self.backend.preflight = unready
        with self.assertRaises(PreEnqueueError) as caught:
            self.backend.submit(self.meta, self.image, self.hint)
        self.assertEqual(caught.exception.error_code, "weights_missing")
        self.assertFalse(caught.exception.retryable)
        self.assertIsNone(self.store.get(job_key(dispatch_handle("modal", self.meta.job_id, self.meta.attempt_id))))
        self.assertIsNone(self.store.get(ACTIVE_JOBS_KEY))
        self.assertEqual(self.dispatcher.dispatched, [])
        # Repairing the install allows exactly one submission of this attempt.
        self.backend.preflight = lambda: None
        first = self.backend.submit(self.meta, self.image, self.hint)
        self.backend.preflight = unready
        self.assertEqual(self.backend.submit(self.meta, self.image, self.hint).app_handle, first.app_handle)
        self.assertEqual(self.dispatcher.dispatched, [first.app_handle])

    def completed(self, meta=None) -> WorkerOutcome:
        meta = meta or self.meta
        png = encode_png_rgb8(meta.width, meta.height)
        return WorkerOutcome("completed", png=png, digest=sha256(png))

    def test_submit_is_idempotent_per_attempt(self) -> None:
        first = self.backend.submit(self.meta, self.image, self.hint)
        again = self.backend.submit(self.meta, self.image, self.hint)
        self.assertEqual(first.app_handle, dispatch_handle("modal", self.meta.job_id, self.meta.attempt_id))
        self.assertEqual(again.app_handle, first.app_handle)
        self.assertEqual(self.dispatcher.dispatched, [first.app_handle])
        self.assertEqual(self.store.get(job_key(first.app_handle))["native_id"], "native-1")

    def test_same_attempt_with_other_payload_is_refused(self) -> None:
        self.backend.submit(self.meta, self.image, self.hint)
        other, image, hint = make_job(fill=(1, 2, 3))
        with self.assertRaises(ContractValidationError):
            self.backend.submit(other, image, hint)

    def test_failed_dispatch_keeps_claim_and_does_not_spawn_again(self) -> None:
        self.dispatcher.dispatch_error = ConnectionError("queue down")
        from deploy.cloud.common.jobs import SubmissionUnknownError
        with self.assertRaises(SubmissionUnknownError):
            self.backend.submit(self.meta, self.image, self.hint)
        self.assertTrue(self.store.get(job_key(self.backend.submit(self.meta, self.image, self.hint).app_handle))["dispatch_unknown"])
        self.dispatcher.dispatch_error = None
        self.assertEqual(self.backend.submit(self.meta, self.image, self.hint).status, JobExecutionStatus.PENDING)
        self.assertEqual(len(self.dispatcher.dispatched), 0)

    def test_index_write_failure_refuses_before_dispatch(self) -> None:
        original = self.store.put
        def fail_index(key, value):
            if key == ACTIVE_JOBS_KEY:
                raise OSError("index down")
            return original(key, value)
        self.store.put = fail_index
        with self.assertRaises(PreEnqueueError):
            self.backend.submit(self.meta, self.image, self.hint)
        self.assertEqual(self.dispatcher.dispatched, [])
        self.assertIsNone(self.store.get(job_key(dispatch_handle("modal", self.meta.job_id, self.meta.attempt_id))))

    def test_status_follows_the_worker(self) -> None:
        handle = self.backend.submit(self.meta, self.image, self.hint).app_handle
        self.assertEqual(self.backend.get(handle).status, JobExecutionStatus.PENDING)
        self.dispatcher.outcome = WorkerOutcome("running")
        self.assertEqual(self.backend.get(handle).status, JobExecutionStatus.RUNNING)
        self.dispatcher.outcome = self.completed()
        record = self.backend.get(handle)
        self.assertEqual(record.status, JobExecutionStatus.COMPLETED)
        self.assertEqual(record.result_digest, self.dispatcher.outcome.digest)
        # Terminal documents never change again.
        self.dispatcher.outcome = WorkerOutcome("failed", error_code="worker_failed", message="late")
        self.assertEqual(self.backend.get(handle).status, JobExecutionStatus.COMPLETED)

    def test_result_survives_a_gateway_restart(self) -> None:
        handle = self.backend.submit(self.meta, self.image, self.hint).app_handle
        self.dispatcher.outcome = self.completed()
        record, png = self.backend.result_png(handle)
        self.assertEqual(png, self.dispatcher.outcome.png)

        restarted = DispatchJobBackend("modal", self.store, self.dispatcher, production_limits(), clock=self.clock)
        self.dispatcher.result = self.dispatcher.outcome.png
        self.assertEqual(restarted.result_png(handle)[1], self.dispatcher.outcome.png)
        self.dispatcher.result = None
        with self.assertRaises(ResultUnavailableError):
            DispatchJobBackend("modal", self.store, self.dispatcher, production_limits()).result_png(handle)
        self.dispatcher.result = encode_png_rgb8(32, 32, (0, 0, 0))
        with self.assertRaises(ResultUnavailableError):
            DispatchJobBackend("modal", self.store, self.dispatcher, production_limits()).result_png(handle)

    def test_invalid_worker_output_fails_the_job(self) -> None:
        handle = self.backend.submit(self.meta, self.image, self.hint).app_handle
        png = encode_png_rgb8(48, 32)
        self.dispatcher.outcome = WorkerOutcome("completed", png=png, digest=sha256(png))
        record = self.backend.get(handle)
        self.assertEqual((record.status, record.error.error_code), (JobExecutionStatus.FAILED, "invalid_output"))

    def test_worker_failure_is_typed(self) -> None:
        handle = self.backend.submit(self.meta, self.image, self.hint).app_handle
        self.dispatcher.outcome = WorkerOutcome("failed", error_code="weights_missing", message="seed first")
        record = self.backend.get(handle)
        self.assertEqual((record.status, record.error.error_code), (JobExecutionStatus.FAILED, "weights_missing"))

    def test_cancel(self) -> None:
        handle = self.backend.submit(self.meta, self.image, self.hint).app_handle
        self.dispatcher.cancel_error = ConnectionError("provider down")
        record, accepted = self.backend.cancel(handle)
        self.assertTrue(accepted)
        self.assertTrue(record.cancel_requested)
        self.assertEqual(self.dispatcher.cancelled, ["native-1"])
        # A failure reported after a cancel request is the cancellation landing.
        self.dispatcher.outcome = WorkerOutcome("failed", error_code="worker_failed", message="interrupted")
        self.assertEqual(self.backend.get(handle).status, JobExecutionStatus.CANCELLED)
        self.assertEqual(self.backend.cancel(handle)[1], False)
        self.assertEqual(self.backend.cancel("handle-unknown"), (None, False))

    def test_lost_dispatch_fails_after_the_grace_period(self) -> None:
        handle = dispatch_handle("modal", "job-x", "attempt-x")
        meta, _, _ = make_job(job_id="job-x", attempt_id="attempt-x")
        doc = {
            "v": 1, "handle": handle, "job_id": meta.job_id, "attempt_id": meta.attempt_id,
            "request_digest": meta.request_digest, "recipe": meta.recipe.to_dict(), "width": 32, "height": 32,
            "seed": 1, "steps": 4, "guidance_scaled": 100, "status": "pending", "native_id": None,
            "cancel_requested": False, "created_at": self.clock.now, "updated_at": self.clock.now,
        }
        self.store.put(job_key(handle), doc)
        self.assertEqual(self.backend.get(handle).status, JobExecutionStatus.PENDING)
        self.clock.now += DispatchJobBackend.DISPATCH_GRACE_SECONDS + 1
        record = self.backend.get(handle)
        self.assertEqual((record.status, record.error.error_code), (JobExecutionStatus.FAILED, "dispatch_lost"))

    def test_poll_trouble_keeps_the_last_state(self) -> None:
        handle = self.backend.submit(self.meta, self.image, self.hint).app_handle
        self.dispatcher.outcome = WorkerOutcome("running")
        self.backend.get(handle)
        self.dispatcher.poll_error = ConnectionError("provider down")
        self.assertEqual(self.backend.get(handle).status, JobExecutionStatus.RUNNING)

    def test_warmup_never_starts_a_gpu(self) -> None:
        self.assertEqual(self.backend.warmup(), "on_demand")
        self.assertEqual(self.dispatcher.dispatched, [])


# ---------------------------------------------------------------------------
# Modal
# ---------------------------------------------------------------------------


class ModalDispatcherTest(unittest.TestCase):
    def setUp(self) -> None:
        self.calls: Dict[str, FakeFunctionCall] = {}
        self.spawned: List[tuple] = []
        self.store = ModalDictStore(FakeModalDict())
        self.dispatcher = ModalWorkerDispatcher(self.spawn, self.calls.__getitem__, ModalErrors, self.store)
        self.meta, self.image, self.hint = make_job()

    def spawn(self, *args: Any) -> FakeFunctionCall:
        self.spawned.append(args)
        call = FakeFunctionCall(f"fc-{len(self.spawned)}")
        self.calls[call.object_id] = call
        return call

    def test_dispatch_passes_the_job_by_value(self) -> None:
        native = self.dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        self.assertEqual(native, "fc-1")
        handle, metadata, image, hint = self.spawned[0]
        self.assertEqual((handle, image, hint), ("h1", self.image, self.hint))
        self.assertEqual(json.loads(metadata), self.meta.to_dict())

    def test_poll_states(self) -> None:
        native = self.dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        call = self.calls[native]
        self.assertEqual(self.dispatcher.poll("h1", native).state, "pending")
        self.store.put(run_key("h1"), {"started_at": 1.0})
        self.assertEqual(self.dispatcher.poll("h1", native).state, "running")

        png = encode_png_rgb8(32, 32)
        call.error, call.result = None, {"ok": True, "png": png, "digest": sha256(png)}
        outcome = self.dispatcher.poll("h1", native)
        self.assertEqual((outcome.state, outcome.png), ("completed", png))
        self.assertEqual(self.dispatcher.fetch_result("h1", native), png)

        for error, code in (
            (ModalErrors.OutputExpiredError("gone"), "result_expired"),
            (ModalErrors.FunctionTimeoutError("slow"), "worker_timeout"),
            (ModalErrors.RemoteError("boom"), "worker_failed"),
        ):
            call.error = error
            self.assertEqual(self.dispatcher.poll("h1", native).error_code, code)
        call.error = None
        call.result = {"ok": False, "error_code": "weights_missing", "message": "seed first"}
        self.assertEqual(self.dispatcher.poll("h1", native).error_code, "weights_missing")

    def test_transient_errors_propagate(self) -> None:
        native = self.dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        for error in (ModalErrors.ConnectionError("net"), ModalErrors.InternalError("500"), OSError("socket")):
            self.calls[native].error = error
            with self.assertRaises(type(error)):
                self.dispatcher.poll("h1", native)

    def test_cancel_keeps_the_warm_container(self) -> None:
        native = self.dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        self.dispatcher.cancel("h1", native)
        self.assertEqual(self.calls[native].cancels, [{"terminate_containers": False}])

    def test_dict_store(self) -> None:
        self.assertTrue(self.store.put_if_absent("k", {"a": 1}))
        self.assertFalse(self.store.put_if_absent("k", {"a": 2}))
        self.assertEqual(self.store.get("k"), {"a": 1})
        self.store.delete("k")
        self.store.delete("k")
        self.assertIsNone(self.store.get("k"))


# ---------------------------------------------------------------------------
# Beam
# ---------------------------------------------------------------------------


class FakeResponse(io.BytesIO):
    def __enter__(self) -> "FakeResponse":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()


class BeamMapStoreTest(unittest.TestCase):
    def setUp(self) -> None:
        self.map = FakeBeamMap()
        self.store = BeamMapStore(self.map)

    def test_documents_carry_the_ttl(self) -> None:
        self.store.put("job:h", {"a": 1})
        self.assertEqual(self.store.get("job:h"), {"a": 1})
        self.assertEqual(self.map.ttls["job:h"], MAP_TTL_SECONDS)
        self.assertFalse(self.store.put_if_absent("job:h", {"a": 2}))
        self.assertTrue(self.store.put_if_absent("job:g", {"a": 3}))
        self.store.delete("job:h")
        self.store.delete("job:h")
        self.assertIsNone(self.store.get("job:h"))

    def test_blobs_are_chunked_below_the_value_limit(self) -> None:
        data = bytes(range(256)) * (CHUNK_BYTES // 128 + 7)
        info = self.store.put_blob("out:h:png", data)
        self.assertEqual(info["chunks"], 3)
        self.assertTrue(all(len(v) <= CHUNK_BYTES for k, v in self.map.data.items()))
        self.assertEqual(self.store.get_blob("out:h:png", info), data)
        self.assertEqual(self.store.put_blob("empty", b"")["chunks"], 1)
        self.assertEqual(self.store.get_blob("empty", self.store.put_blob("empty", b"")), b"")

    def test_damaged_blobs_read_as_missing(self) -> None:
        info = self.store.put_blob("b", b"x" * (CHUNK_BYTES + 1))
        self.assertIsNone(self.store.get_blob("b", {"chunks": "x"}))
        self.assertIsNone(self.store.get_blob("b", {**info, "sha256": "0" * 64}))
        del self.map.data["b:1"]
        self.assertIsNone(self.store.get_blob("b", info))
        self.store.delete_blob("b", info)
        self.assertNotIn("b:0", self.map.data)


class HttpEnqueueTest(unittest.TestCase):
    def test_posts_the_handle_with_the_workspace_token(self) -> None:
        requests: List[Any] = []

        def urlopen(request, timeout):
            requests.append((request, timeout))
            return FakeResponse(b'{"task_id": "task-9"}')

        enqueue = make_http_enqueue("https://worker.example/v1", lambda: "workspace-token", urlopen=urlopen)
        self.assertEqual(enqueue("h1"), "task-9")
        request, timeout = requests[0]
        self.assertEqual((request.full_url, request.get_method()), ("https://worker.example/v1", "POST"))
        self.assertEqual(request.get_header("Authorization"), "Bearer workspace-token")
        self.assertEqual(json.loads(request.data), {"handle": "h1"})
        self.assertEqual(timeout, 15.0)

    def test_refuses_without_url_token_or_task_id(self) -> None:
        def urlopen(request, timeout):
            return FakeResponse(b'{"error": "nope"}')

        for enqueue in (
            make_http_enqueue("", lambda: "t", urlopen=urlopen),
            make_http_enqueue("https://w", lambda: "", urlopen=urlopen),
            make_http_enqueue("https://w", lambda: "t", urlopen=urlopen),
        ):
            with self.assertRaises(RuntimeError):
                enqueue("h1")


class BeamFlowTest(unittest.TestCase):
    """Gateway dispatcher and GPU task body sharing one fake Map."""

    def setUp(self) -> None:
        self.map = FakeBeamMap()
        self.store = BeamMapStore(self.map)
        self.enqueued: List[str] = []
        self.stopped: List[str] = []
        self.clock = Clock()
        self.dispatcher = BeamWorkerDispatcher(
            self.store, self.enqueue, stop_task=self.stopped.append, clock=self.clock
        )
        self.meta, self.image, self.hint = make_job()
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        write_marker(Path(self._tmp.name))
        self.runners: List[FakeRunner] = []
        self.runtime = WorkerRuntime(self._tmp.name, runner_factory=self.make_runner)

    def make_runner(self, path: str) -> FakeRunner:
        runner = FakeRunner(path)
        self.runners.append(runner)
        return runner

    def enqueue(self, handle: str) -> str:
        self.enqueued.append(handle)
        return f"task-{len(self.enqueued)}"

    def test_job_round_trip(self) -> None:
        task = self.dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        self.assertEqual((task, self.enqueued), ("task-1", ["h1"]))
        self.assertEqual(self.dispatcher.poll("h1", task).state, "pending")

        summary = beam_worker_render(self.runtime, self.store, "h1", clock=self.clock, sleep=lambda s: None)
        self.assertTrue(summary["ok"])
        self.assertNotIn("png", summary, "task results stay small; the PNG lives in the Map")
        outcome = self.dispatcher.poll("h1", task)
        self.assertEqual(outcome.state, "completed")
        self.assertEqual(outcome.digest, summary["digest"])
        self.assertEqual(self.dispatcher.fetch_result("h1", task), outcome.png)
        # Inputs are removed once the outcome is stored.
        self.assertFalse([k for k in self.map.data if k.startswith(input_key("h1"))])

    def test_failed_enqueue_discards_the_inputs(self) -> None:
        def broken(handle: str) -> str:
            raise ConnectionError("queue down")

        dispatcher = BeamWorkerDispatcher(self.store, broken)
        with self.assertRaises(ConnectionError):
            dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        self.assertEqual(self.map.data, {})

    def test_cancel_before_start(self) -> None:
        task = self.dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        self.store.put(job_key("h1"), {"cancel_requested": True, "created_at": self.clock.now})
        self.assertEqual(self.dispatcher.poll("h1", task).state, "cancelled")
        self.dispatcher.cancel("h1", task)
        self.assertEqual(self.stopped, [task])
        summary = beam_worker_render(self.runtime, self.store, "h1", sleep=lambda s: None)
        self.assertEqual(summary["error_code"], "cancelled")
        self.assertEqual(self.runners, [], "a cancelled job never loads or renders")

    def test_missing_inputs_fail_typed(self) -> None:
        summary = beam_worker_render(self.runtime, self.store, "h-none", sleep=lambda s: None)
        self.assertEqual(summary["error_code"], "input_missing")
        self.assertEqual(self.dispatcher.poll("h-none", "t").error_code, "input_missing")

    def test_silent_jobs_time_out(self) -> None:
        self.store.put(job_key("h1"), {"created_at": self.clock.now})
        self.clock.now += UNSTARTED_TIMEOUT_SECONDS + STALE_GRACE_SECONDS + 1
        self.assertEqual(self.dispatcher.poll("h1", "t").error_code, "worker_unavailable")

        self.store.put(run_key("h2"), {"started_at": self.clock.now})
        self.assertEqual(self.dispatcher.poll("h2", "t").state, "running")
        self.clock.now += JOB_TIMEOUT_SECONDS + STALE_GRACE_SECONDS + 1
        self.assertEqual(self.dispatcher.poll("h2", "t").error_code, "worker_timeout")

    def test_incomplete_result_is_reported(self) -> None:
        task = self.dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        beam_worker_render(self.runtime, self.store, "h1", sleep=lambda s: None)
        del self.map.data[output_key("h1", "png") + ":0"]
        self.assertEqual(self.dispatcher.poll("h1", task).error_code, "result_expired")
        self.assertIsNone(self.dispatcher.fetch_result("h1", task))

    def test_outcome_write_is_retried_then_raises(self) -> None:
        self.dispatcher.dispatch("h1", self.meta, self.image, self.hint)
        sleeps: List[float] = []
        original = self.map.set
        attempts = {"n": 0}

        def flaky(key: str, value: Any, ttl: Optional[int] = None) -> bool:
            if key.startswith("out:"):
                attempts["n"] += 1
                if attempts["n"] == 1:
                    raise ConnectionError("blip")
            return original(key, value, ttl)

        self.map.set = flaky
        self.assertIs(beam_worker_render(self.runtime, self.store, "h2", sleep=sleeps.append)["ok"], False)
        self.assertEqual(sleeps, [2.0])

        self.map.fail_set = True
        self.map.set = original
        with self.assertRaises(RuntimeError):
            beam_worker_render(self.runtime, self.store, "h1", sleep=lambda s: None)


if __name__ == "__main__":
    unittest.main()
