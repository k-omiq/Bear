"""GPU presence and stop (common/gpu.py), the gateway routes and the Modal helpers, offline."""

from __future__ import annotations

import json
import unittest
from typing import Any, Dict, List, Optional

from deploy.cloud.common.api import CloudGateway
from deploy.cloud.common.contract import (
    ContractValidationError,
    JobExecutionStatus,
    validate_gpu_status_response,
    validate_gpu_stop_request,
    validate_gpu_stop_response,
)
from deploy.cloud.common.gpu import (
    ANALYSIS_REFUSE_SECONDS,
    RELEASE_KEY,
    RELEASE_WINDOW_SECONDS,
    STALE_GRACE_SECONDS,
    GpuControl,
    GpuHeartbeat,
    heartbeat_key,
    read_heartbeat,
    release_key,
    release_container,
    release_recent,
)
from deploy.cloud.common.jobs import ACTIVE_JOBS_KEY, DispatchJobBackend, MemoryStore, WorkerOutcome, job_key
from deploy.cloud.common.manifest import production_limits
from deploy.cloud.modal.backend import (
    ANALYSIS_CALL_EXPIRY_SECONDS,
    ANALYSIS_CALLS_KEY,
    ModalDictStore,
    ModalWorkerDispatcher,
    cancel_tracked_analysis,
    count_tracked_analysis,
    run_tracked_analysis,
)
from deploy.cloud.common.flux import encode_png_rgb8
from deploy.cloud.tests.support import FakeFunctionCall, FakeModalDict, ModalErrors, make_job, sha256, silence_deploy_logs

_restore_logs = None


def setUpModule() -> None:
    global _restore_logs
    _restore_logs = silence_deploy_logs()


def tearDownModule() -> None:
    _restore_logs()


class Clock:
    def __init__(self, now: float = 10_000.0):
        self.now = now

    def __call__(self) -> float:
        return self.now


class TerminatingDispatcher:
    """Just enough of a WorkerDispatcher, recording terminate calls."""

    def __init__(self) -> None:
        self.dispatched: List[str] = []
        self.terminated: List[str] = []
        self.outcomes: Dict[str, Any] = {}

    def dispatch(self, handle, meta, image_png, hint_png) -> str:
        self.dispatched.append(handle)
        return f"native-{len(self.dispatched)}"

    def poll(self, handle, native_id) -> WorkerOutcome:
        outcome = self.outcomes.get(native_id, WorkerOutcome("running"))
        if isinstance(outcome, Exception):
            raise outcome
        return outcome

    def fetch_result(self, handle, native_id) -> Optional[bytes]:
        return None

    def cancel(self, handle, native_id) -> None:
        raise AssertionError("a GPU stop terminates, it does not soft-cancel")

    def terminate(self, handle, native_id) -> None:
        self.terminated.append(native_id)


def heartbeat(store, clock, role="render") -> GpuHeartbeat:
    return GpuHeartbeat(store, role, "L4", 120, 1200, 900, clock=clock)


class HeartbeatTest(unittest.TestCase):
    def setUp(self) -> None:
        self.store = MemoryStore()
        self.clock = Clock()

    def test_idle_heartbeat_is_reported_until_the_scale_down_window_passes(self) -> None:
        heartbeat(self.store, self.clock).idle()
        view = read_heartbeat(self.store, "render", self.clock.now + 60)
        self.assertEqual(view["state"], "idle")
        self.assertEqual(view["gpu"], "L4")
        self.assertEqual(view["scaledown_estimate_at"], self.clock.now + 120)
        self.assertNotIn("container", view)
        self.assertIsNotNone(read_heartbeat(self.store, "render", self.clock.now + 120 + STALE_GRACE_SECONDS))
        self.assertIsNone(read_heartbeat(self.store, "render", self.clock.now + 121 + STALE_GRACE_SECONDS))

    def test_busy_heartbeat_lives_for_the_call_timeout_as_well(self) -> None:
        heartbeat(self.store, self.clock).busy()
        later = self.clock.now + 900 + 120
        view = read_heartbeat(self.store, "render", later)
        self.assertEqual(view["state"], "busy")
        self.assertIsNone(view["scaledown_estimate_at"])
        self.assertIsNone(read_heartbeat(self.store, "render", later + STALE_GRACE_SECONDS + 1))

    def test_starting_uses_the_startup_timeout(self) -> None:
        heartbeat(self.store, self.clock).starting()
        self.assertEqual(read_heartbeat(self.store, "render", self.clock.now + 1300)["state"], "starting")

    def test_malformed_documents_are_ignored(self) -> None:
        good = heartbeat(self.store, self.clock)
        good.idle()
        doc = self.store.get(heartbeat_key("render"))
        for patch in ({"state": "warm"}, {"gpu": "L4; rm -rf"}, {"idle_seconds": True},
                      {"last_active_at": float("nan")}, {"started_at": -1}, {"timeout_seconds": 0}):
            self.store.put(heartbeat_key("render"), {**doc, **patch})
            self.assertIsNone(read_heartbeat(self.store, "render", self.clock.now), patch)
        self.store._data[heartbeat_key("render")] = "not a dict"  # type: ignore[assignment]
        self.assertIsNone(read_heartbeat(self.store, "render", self.clock.now))

    def test_clear_only_removes_its_own_container(self) -> None:
        old = heartbeat(self.store, self.clock)
        old.idle()
        successor = heartbeat(self.store, self.clock)
        successor.starting()
        old.clear()
        self.assertEqual(read_heartbeat(self.store, "render", self.clock.now)["state"], "starting")
        successor.clear()
        self.assertIsNone(self.store.get(heartbeat_key("render")))

    def test_store_failures_never_raise(self) -> None:
        class Broken:
            def put(self, *_):
                raise OSError("dict down")

            get = delete = put

        beat = GpuHeartbeat(Broken(), "analysis", "L4", 120, 180, 180)
        beat.starting()
        beat.busy()
        beat.idle()
        beat.clear()
        with self.assertRaises(OSError):
            read_heartbeat(Broken(), "analysis", 0)
        with self.assertRaises(OSError):
            release_recent(Broken(), 0, "render")

    def test_release_keeps_heartbeat_until_exit_hook(self) -> None:
        beat = heartbeat(self.store, self.clock)
        beat.idle()
        calls = []
        self.assertEqual(release_container(beat, lambda: calls.append("stop")), {"ok": True})
        self.assertEqual(calls, ["stop"])
        self.assertIsNotNone(self.store.get(heartbeat_key("render")))
        beat.clear()
        self.assertIsNone(self.store.get(heartbeat_key("render")))


class GpuControlTest(unittest.TestCase):
    def setUp(self) -> None:
        self.store = MemoryStore()
        self.clock = Clock()
        self.dispatcher = TerminatingDispatcher()
        self.backend = DispatchJobBackend("modal", self.store, self.dispatcher, production_limits(), clock=self.clock)
        self.released: List[str] = []
        self.analysis_cancels = 0
        self.analysis_in_flight = 0
        self.release_error: Optional[Exception] = None

        def cancel_analysis() -> int:
            self.analysis_cancels += 1
            return self.analysis_in_flight

        def releaser(role):
            def spawn():
                if self.release_error is not None:
                    raise self.release_error
                self.released.append(role)
            return spawn

        self.control = GpuControl(
            "modal", self.store, "L4", backend=self.backend,
            release={role: releaser(role) for role in ("render", "analysis")},
            cancel_analysis=cancel_analysis, analysis_in_flight=lambda: self.analysis_in_flight,
            list_prices={"L4": 0.8, "A10": 1.1}, clock=self.clock,
        )

    def test_status_lists_fresh_heartbeats_and_the_deployed_gpu_price(self) -> None:
        heartbeat(self.store, self.clock, "analysis").busy()
        status = self.control.status()
        validate_gpu_status_response(status)
        self.assertEqual(status["provider"], "modal")
        self.assertTrue(status["supported"])
        self.assertEqual([c["role"] for c in status["containers"]], ["analysis"])
        self.assertEqual(status["list_price_usd_per_hour"], {"L4": 0.8})

    def test_status_reports_unknown_when_heartbeat_store_fails(self) -> None:
        self.control.store = type("Broken", (), {"get": lambda *_: (_ for _ in ()).throw(OSError("down"))})()
        status = self.control.status()
        self.assertFalse(status["supported"])
        self.assertEqual(status["containers"], [])

    def test_failed_termination_keeps_job_and_heartbeat_for_retry(self) -> None:
        meta, image, hint = make_job()
        record = self.backend.submit(meta, image, hint)
        heartbeat(self.store, self.clock).busy()
        original = self.dispatcher.terminate
        self.dispatcher.terminate = lambda *_: (_ for _ in ()).throw(OSError("cancel failed"))
        from deploy.cloud.common.jobs import StopUncertainError
        with self.assertRaises(StopUncertainError):
            self.control.stop("render")
        self.assertEqual(self.backend.active_count(), 1)
        self.assertNotIn(self.backend.get(record.app_handle).status, (JobExecutionStatus.CANCELLED, JobExecutionStatus.FAILED))
        self.assertIsNotNone(self.store.get(heartbeat_key("render")))
        self.dispatcher.terminate = original
        self.assertEqual(self.control.stop("render")["cancelled_jobs"], 1)

    def test_active_index_keeps_more_than_sixty_four_jobs(self) -> None:
        for index in range(65):
            self.backend.submit(*make_job(job_id=f"job-{index}", attempt_id=f"attempt-{index}"))
        self.assertEqual(self.backend.active_count(), 65)
        self.assertEqual(self.control.stop("render")["cancelled_jobs"], 65)
        self.assertEqual(len(self.dispatcher.terminated), 65)

    def test_release_markers_are_independent_per_role(self) -> None:
        self.control.stop("render")
        self.control.stop("analysis")
        self.clock.now += ANALYSIS_REFUSE_SECONDS
        self.assertTrue(self.control.before_dispatch("render"))
        self.assertIsNotNone(self.store.get(release_key("analysis")))

    def test_analysis_admission_holds_stop_until_call_is_recorded(self) -> None:
        import threading
        entered = threading.Event()
        finished = threading.Event()

        def stop() -> None:
            entered.set()
            self.control.stop("analysis")
            finished.set()

        with self.control.analysis_admission():
            thread = threading.Thread(target=stop)
            thread.start()
            self.assertTrue(entered.wait(1))
            self.assertFalse(finished.wait(0.02))
        self.assertTrue(finished.wait(1))
        thread.join()

    def test_stop_with_nothing_up_is_an_idempotent_no_op(self) -> None:
        for _ in range(2):
            result = self.control.stop()
            validate_gpu_stop_response(result)
            self.assertEqual(result, {"provider": "modal", "stopped": [], "cancelled_jobs": 0})
        self.assertEqual(self.released, [])

    def test_stop_releases_an_idle_container_and_leaves_its_heartbeat_to_it(self) -> None:
        beat = heartbeat(self.store, self.clock)
        beat.idle()
        result = self.control.stop()
        self.assertEqual(result["stopped"], ["render"])
        self.assertEqual(self.released, ["render"])
        # Still up, and still billing, until the release reaches it.
        self.assertEqual([c["role"] for c in self.control.status()["containers"]], ["render"])
        self.assertTrue(release_recent(self.store, self.clock.now, "render"))
        release_container(beat, lambda: None)
        self.assertEqual([c["role"] for c in self.control.status()["containers"]], ["render"])
        beat.clear()
        self.assertEqual(self.control.status()["containers"], [])

    def test_a_release_that_could_not_be_sent_is_not_reported_as_stopped(self) -> None:
        heartbeat(self.store, self.clock).idle()
        self.release_error = OSError("modal down")
        self.assertEqual(self.control.stop()["stopped"], [])
        self.assertEqual([c["role"] for c in self.control.status()["containers"]], ["render"])

    def test_stop_cancels_running_jobs_and_terminates_their_containers(self) -> None:
        meta, image, hint = make_job()
        other, image2, hint2 = make_job(job_id="job-2", attempt_id="attempt-2")
        first = self.backend.submit(meta, image, hint)
        second = self.backend.submit(other, image2, hint2)
        self.assertEqual(self.store.get(ACTIVE_JOBS_KEY)["handles"], [first.app_handle, second.app_handle])
        self.dispatcher.outcomes["native-2"] = WorkerOutcome("pending")
        heartbeat(self.store, self.clock).busy()

        result = self.control.stop()

        self.assertEqual(result, {"provider": "modal", "stopped": ["render"], "cancelled_jobs": 2})
        self.assertEqual(self.dispatcher.terminated, ["native-1", "native-2"])
        # The busy container was terminated with its call; a release would only cold-start another.
        self.assertEqual(self.released, [])
        self.assertIsNone(self.store.get(heartbeat_key("render")))
        for handle in (first.app_handle, second.app_handle):
            doc = self.store.get(job_key(handle))
            self.assertEqual(doc["status"], JobExecutionStatus.CANCELLED.value)
            self.assertTrue(doc["cancel_requested"])
            self.assertEqual(self.backend.get(handle).status, JobExecutionStatus.CANCELLED)
        self.assertIsNone(self.store.get(ACTIVE_JOBS_KEY))
        self.assertEqual(self.control.stop()["cancelled_jobs"], 0)

    def test_stop_keeps_a_render_the_gpu_already_finished(self) -> None:
        meta, image, hint = make_job()
        failing, image2, hint2 = make_job(job_id="job-2", attempt_id="attempt-2")
        running, image3, hint3 = make_job(job_id="job-3", attempt_id="attempt-3")
        done = self.backend.submit(meta, image, hint)
        broke = self.backend.submit(failing, image2, hint2)
        busy = self.backend.submit(running, image3, hint3)
        png = encode_png_rgb8(meta.width, meta.height)
        self.dispatcher.outcomes["native-1"] = WorkerOutcome("completed", png=png, digest=sha256(png))
        self.dispatcher.outcomes["native-2"] = WorkerOutcome("failed", error_code="worker_failed", message="boom")

        result = self.control.stop()

        self.assertEqual(result["cancelled_jobs"], 1)
        self.assertEqual(self.dispatcher.terminated, ["native-3"])
        self.assertEqual(self.backend.get(done.app_handle).status, JobExecutionStatus.COMPLETED)
        self.assertEqual(self.backend.result_png(done.app_handle)[1], png)
        self.assertEqual(self.backend.get(broke.app_handle).status, JobExecutionStatus.FAILED)
        self.assertEqual(self.backend.get(busy.app_handle).status, JobExecutionStatus.CANCELLED)
        self.assertIsNone(self.store.get(ACTIVE_JOBS_KEY))

    def test_a_job_the_provider_cannot_describe_is_still_stopped(self) -> None:
        meta, image, hint = make_job()
        record = self.backend.submit(meta, image, hint)
        self.dispatcher.outcomes["native-1"] = OSError("provider hiccup")
        self.assertEqual(self.control.stop()["cancelled_jobs"], 1)
        self.assertEqual(self.backend.get(record.app_handle).status, JobExecutionStatus.CANCELLED)

    def test_terminal_jobs_leave_the_active_index(self) -> None:
        meta, image, hint = make_job()
        record = self.backend.submit(meta, image, hint)
        self.backend._update(record.app_handle, lambda d: self.backend._apply_failure(d, "worker_failed", "boom"))
        self.assertIsNone(self.store.get(ACTIVE_JOBS_KEY))

    def test_stop_cancels_analysis_tiles_in_flight(self) -> None:
        self.analysis_in_flight = 3
        heartbeat(self.store, self.clock, "analysis").busy()
        result = self.control.stop()
        self.assertEqual(result["stopped"], ["analysis"])
        self.assertEqual(self.analysis_cancels, 1)
        self.assertEqual(self.released, [])
        self.assertIsNone(self.store.get(heartbeat_key("analysis")))

    def test_an_idle_analysis_container_is_released_even_when_a_queued_tile_was_cancelled(self) -> None:
        self.analysis_in_flight = 1
        heartbeat(self.store, self.clock, "analysis").idle()
        self.assertEqual(self.control.stop()["stopped"], ["analysis"])
        self.assertEqual(self.released, ["analysis"])
        self.assertIsNotNone(self.store.get(heartbeat_key("analysis")))

    def test_a_busy_container_without_a_known_call_still_gets_a_release(self) -> None:
        heartbeat(self.store, self.clock).busy()
        self.assertEqual(self.control.stop()["stopped"], ["render"])
        self.assertEqual(self.released, ["render"])
        self.assertIsNotNone(self.store.get(heartbeat_key("render")))

    def test_stopping_one_role_leaves_the_other_alone(self) -> None:
        meta, image, hint = make_job()
        record = self.backend.submit(meta, image, hint)
        heartbeat(self.store, self.clock).busy()
        heartbeat(self.store, self.clock, "analysis").idle()
        self.analysis_in_flight = 1

        result = self.control.stop("analysis")

        self.assertEqual(result, {"provider": "modal", "stopped": ["analysis"], "cancelled_jobs": 0})
        self.assertEqual(self.released, ["analysis"])
        self.assertEqual(self.dispatcher.terminated, [])
        self.assertNotEqual(self.backend.get(record.app_handle).status, JobExecutionStatus.CANCELLED)
        self.assertFalse(release_recent(self.store, self.clock.now, "render"))
        self.assertTrue(release_recent(self.store, self.clock.now, "analysis"))

        self.assertEqual(self.control.stop("render")["cancelled_jobs"], 1)
        self.assertEqual(self.analysis_cancels, 1)
        with self.assertRaises(ValueError):
            self.control.stop("seed")

    def test_before_dispatch_refuses_analysis_just_after_a_stop_and_clears_the_marker_for_renders(self) -> None:
        self.control.stop()
        self.assertFalse(self.control.before_dispatch("analysis"))
        self.assertTrue(release_recent(self.store, self.clock.now, "analysis"))
        self.assertFalse(self.control.before_dispatch("render"))
        self.assertTrue(release_recent(self.store, self.clock.now, "render"))
        self.assertTrue(release_recent(self.store, self.clock.now, "analysis"))
        self.clock.now += ANALYSIS_REFUSE_SECONDS
        self.assertTrue(self.control.before_dispatch("render"))
        self.assertTrue(self.control.before_dispatch("analysis"))
        self.assertIsNone(self.store.get(release_key("render")))

    def test_a_render_stop_does_not_refuse_analysis(self) -> None:
        self.control.stop("render")
        self.assertTrue(self.control.before_dispatch("analysis"))

    def test_idle_only_releases_an_idle_role_and_touches_nothing_else(self) -> None:
        meta, image, hint = make_job()
        record = self.backend.submit(meta, image, hint)
        heartbeat(self.store, self.clock, "analysis").idle()
        result = self.control.stop("analysis", idle_only=True)
        validate_gpu_stop_response(result)
        self.assertEqual(result, {"provider": "modal", "stopped": ["analysis"], "cancelled_jobs": 0})
        self.assertEqual(self.released, ["analysis"])
        # Nothing cancelled, no marker: the next tile is not refused and the next
        # container loads its model as usual.
        self.assertEqual(self.analysis_cancels, 0)
        self.assertEqual(self.dispatcher.terminated, [])
        self.assertNotEqual(self.backend.get(record.app_handle).status, JobExecutionStatus.CANCELLED)
        self.assertIsNone(self.store.get(RELEASE_KEY))
        self.assertTrue(self.control.before_dispatch("analysis"))
        # Its heartbeat stays until the container has gone, as with a plain release.
        self.assertIsNotNone(self.store.get(heartbeat_key("analysis")))

    def test_idle_only_skips_a_busy_starting_absent_or_working_role(self) -> None:
        self.assertEqual(self.control.stop("analysis", idle_only=True)["stopped"], [])
        heartbeat(self.store, self.clock, "analysis").busy()
        self.assertEqual(self.control.stop("analysis", idle_only=True)["stopped"], [])
        heartbeat(self.store, self.clock, "analysis").starting()
        self.assertEqual(self.control.stop("analysis", idle_only=True)["stopped"], [])
        # Idle by its heartbeat, with a tile of another caller still tracked in flight.
        heartbeat(self.store, self.clock, "analysis").idle()
        self.analysis_in_flight = 1
        self.assertEqual(self.control.stop("analysis", idle_only=True)["stopped"], [])
        self.assertEqual((self.released, self.analysis_cancels), ([], 0))
        self.assertIsNone(self.store.get(RELEASE_KEY))
        self.assertIsNotNone(self.store.get(heartbeat_key("analysis")))

    def test_idle_only_treats_an_unreadable_call_count_as_busy(self) -> None:
        def broken() -> int:
            raise OSError("dict down")
        self.control.analysis_in_flight = broken
        heartbeat(self.store, self.clock, "analysis").idle()
        self.assertEqual(self.control.stop("analysis", idle_only=True)["stopped"], [])
        self.assertEqual(self.released, [])

    def test_idle_only_for_both_roles_releases_only_the_idle_one(self) -> None:
        heartbeat(self.store, self.clock).busy()
        heartbeat(self.store, self.clock, "analysis").idle()
        self.assertEqual(self.control.stop(idle_only=True)["stopped"], ["analysis"])
        self.assertEqual(self.released, ["analysis"])
        self.release_error = OSError("modal down")
        self.assertEqual(self.control.stop("analysis", idle_only=True)["stopped"], [])

    def test_idle_only_releases_an_idle_render_gpu_only_with_no_job_in_flight(self) -> None:
        heartbeat(self.store, self.clock).idle()
        meta, image, hint = make_job()
        record = self.backend.submit(meta, image, hint)
        # Idle by its heartbeat, with a job still pending behind it.
        self.assertEqual(self.control.stop("render", idle_only=True)["stopped"], [])
        self.assertEqual((self.released, self.dispatcher.terminated), ([], []))
        self.backend._update(record.app_handle, lambda d: self.backend._apply_failure(d, "worker_failed", "boom"))
        result = self.control.stop("render", idle_only=True)
        validate_gpu_stop_response(result)
        self.assertEqual(result, {"provider": "modal", "stopped": ["render"], "cancelled_jobs": 0})
        self.assertEqual(self.released, ["render"])
        self.assertIsNone(self.store.get(RELEASE_KEY))
        self.assertTrue(self.control.before_dispatch("render"))

    def test_idle_only_treats_an_unreadable_render_count_as_busy(self) -> None:
        heartbeat(self.store, self.clock).idle()

        class Broken:
            def active_count(self) -> int:
                raise OSError("dict down")

        self.control.backend = Broken()
        self.assertEqual(self.control.stop("render", idle_only=True)["stopped"], [])
        # A backend that keeps no index has nothing in flight to wait for.
        self.control.backend = None
        self.assertEqual(self.control.stop("render", idle_only=True)["stopped"], ["render"])

    def test_active_count_skips_terminal_and_missing_documents(self) -> None:
        self.assertEqual(self.backend.active_count(), 0)
        first = self.backend.submit(*make_job())
        self.backend.submit(*make_job(job_id="job-2", attempt_id="attempt-2"))
        self.assertEqual(self.backend.active_count(), 2)
        self.backend._update(first.app_handle, lambda d: self.backend._apply_failure(d, "worker_failed", "boom"))
        self.assertEqual(self.backend.active_count(), 1)

    def test_release_marker_expires(self) -> None:
        self.control.stop()
        self.assertTrue(release_recent(self.store, self.clock.now + RELEASE_WINDOW_SECONDS - 1, "render"))
        self.assertFalse(release_recent(self.store, self.clock.now + RELEASE_WINDOW_SECONDS, "render"))


class GatewayGpuRoutesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.store = MemoryStore()
        self.clock = Clock()
        backend = DispatchJobBackend("modal", self.store, TerminatingDispatcher(), production_limits(), clock=self.clock)
        self.control = GpuControl("modal", self.store, "L4", backend=backend, list_prices={"L4": 0.8}, clock=self.clock)
        self.gateway = CloudGateway(provider="modal", backend=backend, trust_edge_auth=True, gpu_control=self.control)
        self.plain = CloudGateway(provider="beam", proxy_token_id="k", proxy_token_secret="s", bearer_token="t")

    def call(self, gateway, method, path, headers=None):
        status, response_headers, body = gateway.handle_http_request(method, path, headers or {}, b"")
        return status, response_headers, json.loads(body)

    def test_status_route_reports_heartbeats(self) -> None:
        heartbeat(self.store, self.clock).idle()
        status, headers, body = self.call(self.gateway, "GET", "/mc/v1/gpu")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "application/json")
        self.assertEqual(body["containers"][0]["state"], "idle")

    def test_uncertain_stop_returns_server_error_for_retry(self) -> None:
        from deploy.cloud.common.jobs import StopUncertainError
        self.control.backend.stop_active = lambda: (_ for _ in ()).throw(StopUncertainError("failed"))
        status, _, body = self.call(self.gateway, "POST", "/mc/v1/gpu/stop")
        self.assertEqual((status, body["error_code"]), (503, "stop_uncertain"))

    def call_with(self, gateway, path, body: bytes):
        status, _, raw = gateway.handle_http_request("POST", path, {"Content-Type": "application/json"}, body)
        return status, json.loads(raw)

    def test_stop_route_is_idempotent(self) -> None:
        for _ in range(2):
            status, _, body = self.call(self.gateway, "POST", "/mc/v1/gpu/stop")
            self.assertEqual((status, body["stopped"], body["cancelled_jobs"]), (200, [], 0))

    def test_stop_route_takes_an_optional_role(self) -> None:
        heartbeat(self.store, self.clock).idle()
        heartbeat(self.store, self.clock, "analysis").idle()
        self.control.release = {"render": lambda: None, "analysis": lambda: None}
        status, body = self.call_with(self.gateway, "/mc/v1/gpu/stop", b'{"role":"analysis"}')
        self.assertEqual((status, body["stopped"]), (200, ["analysis"]))
        status, body = self.call_with(self.gateway, "/mc/v1/gpu/stop", b"{}")
        self.assertEqual((status, body["stopped"]), (200, ["render", "analysis"]))
        for bad in (b'{"role":"seed"}', b'{"role":null}', b'{"role":"render","x":1}', b"[]", b"not json", b"\xff",
                    b'{"role":"analysis","idle_only":"yes"}', b'{"idle_only":1}', b'{"idle_only":null}'):
            status, body = self.call_with(self.gateway, "/mc/v1/gpu/stop", bad)
            self.assertEqual((status, body["error_code"]), (400, "invalid_request"), bad)

    def test_stop_route_idle_only_releases_idle_and_false_is_a_plain_stop(self) -> None:
        released: List[str] = []
        self.control.release = {"render": lambda: released.append("render"),
                                "analysis": lambda: released.append("analysis")}
        heartbeat(self.store, self.clock).busy()
        heartbeat(self.store, self.clock, "analysis").idle()
        status, body = self.call_with(self.gateway, "/mc/v1/gpu/stop", b'{"role":"render","idle_only":true}')
        self.assertEqual((status, body), (200, {"provider": "modal", "stopped": [], "cancelled_jobs": 0}))
        status, body = self.call_with(self.gateway, "/mc/v1/gpu/stop", b'{"role":"analysis","idle_only":true}')
        self.assertEqual((status, body["stopped"]), (200, ["analysis"]))
        self.assertIsNone(self.store.get(RELEASE_KEY))
        self.assertTrue(self.gateway._gpu_may_dispatch("analysis"))
        status, body = self.call_with(self.gateway, "/mc/v1/gpu/stop", b'{"role":"render","idle_only":false}')
        self.assertEqual((status, body["stopped"]), (200, ["render"]))
        self.assertEqual(released, ["analysis", "render"])
        self.assertTrue(release_recent(self.store, self.clock.now, "render"))

    def test_a_deployment_without_gpu_control_says_unsupported(self) -> None:
        auth = {"Authorization": "Bearer t"}
        status, _, body = self.call(self.plain, "GET", "/mc/v1/gpu", auth)
        self.assertIsInstance(body.pop("now"), float)
        self.assertEqual((status, body), (200, {"provider": "beam", "supported": False, "containers": [],
                                                "list_price_usd_per_hour": {}}))
        status, _, body = self.call(self.plain, "POST", "/mc/v1/gpu/stop", auth)
        self.assertEqual((status, body["error_code"]), (501, "unsupported"))

    def test_gpu_routes_require_auth(self) -> None:
        for method, path in (("GET", "/mc/v1/gpu"), ("POST", "/mc/v1/gpu/stop")):
            status, _, body = self.call(self.plain, method, path)
            self.assertEqual((status, body["error_code"]), (401, "unauthorized"))

    def test_analysis_tiles_are_refused_just_after_a_stop(self) -> None:
        self.control.stop()
        self.assertFalse(self.gateway._gpu_may_dispatch("analysis"))
        self.assertFalse(self.gateway._gpu_may_dispatch("render"))


class ContractTest(unittest.TestCase):
    def test_analysis_metadata_must_be_an_object(self) -> None:
        from deploy.cloud.common.contract import analysis_request_digest
        for value in (None, []):
            with self.assertRaises(ContractValidationError):
                analysis_request_digest(value)
    def status(self, **patch: Any) -> Dict[str, Any]:
        base = {"provider": "modal", "supported": True, "now": 3.0, "list_price_usd_per_hour": {"L4": 0.8},
                "containers": [{"role": "render", "gpu": "L4", "state": "idle", "started_at": 1.0,
                                "last_active_at": 2.0, "idle_seconds": 120, "scaledown_estimate_at": 122.0}]}
        return {**base, **patch}

    def test_rejects_unknown_shapes(self) -> None:
        validate_gpu_status_response(self.status())
        container = self.status()["containers"][0]
        for bad in (
            self.status(provider="aws"),
            self.status(now=None),
            self.status(extra=1),
            self.status(supported=False),
            self.status(containers=[container, container]),
            self.status(containers=[{**container, "state": "warm"}]),
            self.status(containers=[{**container, "started_at": float("inf")}]),
            self.status(containers=[{**container, "note": "x"}]),
            self.status(list_price_usd_per_hour={"L4": -1}),
        ):
            with self.assertRaises(ContractValidationError):
                validate_gpu_status_response(bad)
        validate_gpu_stop_request({})
        validate_gpu_stop_request({"role": "render"})
        validate_gpu_stop_request({"role": "analysis", "idle_only": True})
        validate_gpu_stop_request({"idle_only": False})
        for bad_request in ({"role": "gpu"}, {"role": None}, {"all": True}, [], {"idle_only": "true"},
                            {"idle_only": 0}):
            with self.assertRaises(ContractValidationError):
                validate_gpu_stop_request(bad_request)
        for bad in ({"provider": "modal", "stopped": ["gpu"], "cancelled_jobs": 0},
                    {"provider": "modal", "stopped": ["render", "render"], "cancelled_jobs": 0},
                    {"provider": "modal", "stopped": [], "cancelled_jobs": -1}):
            with self.assertRaises(ContractValidationError):
                validate_gpu_stop_response(bad)


class ModalHelpersTest(unittest.TestCase):
    def test_parallel_tiles_are_all_tracked_and_all_cancelled(self) -> None:
        store = ModalDictStore(FakeModalDict())
        calls: Dict[str, FakeFunctionCall] = {}
        seen: List[Any] = []

        def from_id(call_id):
            return calls.setdefault(call_id, FakeFunctionCall(call_id))

        def spawn_as(call_id, inner=None):
            def spawn(metadata, tile):
                call = from_id(call_id)
                call.error = None
                call.result = {"ok": call_id}

                def get(timeout=None):
                    if inner is not None:
                        inner()
                    seen.append(sorted(c["id"] for c in store.get(ANALYSIS_CALLS_KEY)["calls"]))
                    return call.result

                call.get = get  # type: ignore[method-assign]
                return call
            return spawn

        # A tile in flight while a second one is sent and waited on: both are listed.
        def second():
            self.assertEqual(run_tracked_analysis(spawn_as("fc-2"), store, {}, b"", clock=lambda: 5.0), {"ok": "fc-2"})

        self.assertEqual(run_tracked_analysis(spawn_as("fc-1", second), store, {}, b"", clock=lambda: 5.0), {"ok": "fc-1"})
        self.assertEqual(seen, [["fc-1", "fc-2"], ["fc-1"]])
        self.assertIsNone(store.get(ANALYSIS_CALLS_KEY))

        store.put(ANALYSIS_CALLS_KEY, {"calls": [{"id": "fc-a", "at": 100.0}, {"id": "fc-b", "at": 100.0},
                                                 {"id": "fc-old", "at": 100.0 - ANALYSIS_CALL_EXPIRY_SECONDS}]})
        self.assertEqual(cancel_tracked_analysis(store, from_id, clock=lambda: 100.0), 2)
        self.assertEqual(calls["fc-a"].cancels, [{"terminate_containers": True}])
        self.assertEqual(calls["fc-b"].cancels, [{"terminate_containers": True}])
        self.assertNotIn("fc-old", calls)
        self.assertIsNone(store.get(ANALYSIS_CALLS_KEY))
        self.assertEqual(cancel_tracked_analysis(store, from_id, clock=lambda: 100.0), 0)

    def test_a_cancel_that_fails_is_kept_for_the_next_stop(self) -> None:
        store = ModalDictStore(FakeModalDict())
        store.put(ANALYSIS_CALLS_KEY, {"calls": [{"id": "fc-1", "at": 1.0}]})

        def from_id(call_id):
            raise OSError("modal down")

        from deploy.cloud.common.jobs import StopUncertainError
        with self.assertRaises(StopUncertainError):
            cancel_tracked_analysis(store, from_id, clock=lambda: 2.0)
        self.assertEqual(store.get(ANALYSIS_CALLS_KEY)["calls"], [{"id": "fc-1", "at": 1.0}])

    def test_tracked_tiles_are_counted_without_being_cancelled(self) -> None:
        store = ModalDictStore(FakeModalDict())
        self.assertEqual(count_tracked_analysis(store, clock=lambda: 100.0), 0)
        store.put(ANALYSIS_CALLS_KEY, {"calls": [{"id": "fc-a", "at": 100.0},
                                                 {"id": "fc-old", "at": 100.0 - ANALYSIS_CALL_EXPIRY_SECONDS}]})
        self.assertEqual(count_tracked_analysis(store, clock=lambda: 100.0), 1)
        self.assertEqual(len(store.get(ANALYSIS_CALLS_KEY)["calls"]), 2)

    def test_dispatcher_terminate_cancels_with_containers(self) -> None:
        call = FakeFunctionCall("fc-render")
        dispatcher = ModalWorkerDispatcher(spawn=lambda *a: call, function_call_from_id=lambda _: call,
                                           exceptions=ModalErrors, store=MemoryStore())
        dispatcher.cancel("h", "fc-render")
        dispatcher.terminate("h", "fc-render")
        self.assertEqual(call.cancels, [{"terminate_containers": False}, {"terminate_containers": True}])


if __name__ == "__main__":
    unittest.main()
