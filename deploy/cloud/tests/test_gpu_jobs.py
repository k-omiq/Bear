"""Submit-then-poll analysis tiles and denoise pages (modal/backend.py ModalGpuJobs and
the /jobs routes of common/api.py), offline with fake Modal calls.

Modal answers a web request still running after 150 s with a 303, which the desktop
refuses. These tests hold the job routes to their promise: no request waits on the
GPU, a job that takes far longer than 150 s of simulated time still completes, a
repeated submit never spawns twice, and a cancel reaches the call.
"""

from __future__ import annotations

import base64
import hashlib
import io
import json
import time
import unittest
from contextlib import contextmanager
from typing import Any, Dict, List, Optional

import numpy as np
from PIL import Image

from deploy.cloud.common import denoise as d
from deploy.cloud.common.analysis_seed import AnalysisUnavailable
from deploy.cloud.common.api import CloudGateway
from deploy.cloud.common.capacity import MODAL_WEB_REDIRECT_SECONDS, SYNC_GPU_WAIT_SECONDS
from deploy.cloud.common.contract import (
    ANALYSIS_BATCH_MAX_REQUESTS,
    ANALYSIS_RT,
    ANALYSIS_SAM,
    ANALYSIS_VERSION,
    GPU_JOB_OPERATIONS,
    analysis_batch_digest,
    analysis_request_digest,
    gpu_job_handle,
    valid_gpu_job_attempt,
    valid_gpu_job_handle,
)
from deploy.cloud.common.jobs import GpuWaitExceeded
from deploy.cloud.modal.backend import (
    ANALYSIS_CALLS_KEY,
    GPU_JOB_DISPATCH_GRACE_SECONDS,
    ModalDictStore,
    ModalGpuJobs,
    analysis_failure,
    cancel_tracked_analysis,
    count_tracked_analysis,
    denoise_failure,
    gpu_job_key,
    run_tracked_analysis,
)
from deploy.cloud.tests.support import FakeFunctionCall, FakeModalDict, ModalErrors, silence_deploy_logs

_restore_logs = None


def setUpModule() -> None:
    global _restore_logs
    _restore_logs = silence_deploy_logs()


def tearDownModule() -> None:
    _restore_logs()


ATTEMPT = "attempt-0123456789ab"
GRAIN = {"op": "denoise", "engine": "waifu2x-art-scan", "level": 1}


def grey_png(level: int = 90) -> bytes:
    out = io.BytesIO()
    Image.fromarray(np.full((8, 8), level, dtype=np.uint8)).save(out, format="PNG")
    return out.getvalue()


def rgb_png(size=(16, 12)) -> bytes:
    out = io.BytesIO()
    Image.new("RGB", size, (100, 50, 25)).save(out, format="PNG")
    return out.getvalue()


def denoise_meta(page: bytes) -> Dict[str, Any]:
    raw = {"schema": 1, "steps": [GRAIN]}
    return {"protocol_version": d.DENOISE_VERSION, "recipe": raw, "request_digest": d.request_digest(raw, page)}


def analysis_meta(tile: bytes, capability: str = ANALYSIS_SAM, page: str = "e" * 64) -> Dict[str, Any]:
    request = {
        "protocol_version": ANALYSIS_VERSION, "capability": capability,
        "graph_sha256s": ["a" * 64, "b" * 64] if capability == ANALYSIS_SAM else ["d" * 64],
        "model_revision": "c" * 40, "tile_id": "tile_01",
        "tile_rect": {"x": 0, "y": 0, "width": 16, "height": 12},
        "tile_png_sha256": hashlib.sha256(tile).hexdigest(),
        "source_page_sha256": page, "page_width": 16, "page_height": 12,
        "tile_core": {"x": 0, "y": 0, "width": 16, "height": 12},
    }
    request["request_digest"] = analysis_request_digest(request)[1]
    return request


class Clock:
    def __init__(self, now: float = 1_000.0):
        self.now = now

    def __call__(self) -> float:
        return self.now


class FakeModal:
    """Spawned calls by id. A call stays unfinished (get(timeout=0) raises the builtin
    TimeoutError) until a test finishes it."""

    def __init__(self) -> None:
        self.calls: Dict[str, FakeFunctionCall] = {}
        self.spawned: List[Any] = []
        self.fail_spawn = False

    def from_id(self, call_id: str) -> FakeFunctionCall:
        return self.calls[call_id]

    def spawn(self, metadata: Dict[str, Any], png: bytes) -> FakeFunctionCall:
        if self.fail_spawn:
            raise ModalErrors.InternalError("spawn failed")
        call = FakeFunctionCall(f"fc-{len(self.calls) + 1}")
        self.calls[call.object_id] = call
        self.spawned.append((metadata, png))
        return call


class Proxy:
    """What app.py's proxies do for the job routes, on fakes."""

    def __init__(self, jobs: ModalGpuJobs, kind: str, spawn, admission=None):
        self.jobs, self.kind, self.spawn, self.admission = jobs, kind, spawn, admission

    def capabilities(self) -> Dict[str, Any]:
        return {"protocol_version": "x"}

    def submit_job(self, metadata, png, attempt_id):
        return self.jobs.submit(self.kind, self.spawn, metadata, png, attempt_id, admission=self.admission)

    def submit_batch_job(self, metadata, pairs, attempt_id):
        return self.jobs.submit("analysis_batch", self.spawn, metadata, pairs, attempt_id, admission=self.admission)

    def _kind(self, handle):
        return "analysis_batch" if handle.startswith("analysis_batch-") else self.kind

    def job_status(self, handle):
        return self.jobs.status(self._kind(handle), handle,
                                denoise_failure if self.kind == "denoise" else analysis_failure)

    def cancel_job(self, handle):
        return self.jobs.cancel(self._kind(handle), handle)


class HandleTest(unittest.TestCase):
    def test_handle_is_shared_with_the_desktop(self) -> None:
        # The same vector is in crates/cleaner-core/src/cloud_job_wire.rs.
        self.assertEqual(gpu_job_handle("denoise", "a" * 64, ATTEMPT), "denoise-487b9e31a97b580d5a4a192d3b1f9e29")
        self.assertEqual(gpu_job_handle("analysis_batch", "a" * 64, ATTEMPT),
                         "analysis_batch-440222160b1226445bb30b6bf856c227")
        self.assertNotEqual(gpu_job_handle("analysis", "a" * 64, ATTEMPT), gpu_job_handle("denoise", "a" * 64, ATTEMPT))
        self.assertTrue(valid_gpu_job_handle("denoise", gpu_job_handle("denoise", "a" * 64, ATTEMPT)))
        self.assertFalse(valid_gpu_job_handle("analysis", gpu_job_handle("denoise", "a" * 64, ATTEMPT)))
        self.assertFalse(valid_gpu_job_handle("denoise", "denoise-../../gpu"))

    def test_attempt_ids_are_bounded_and_plain(self) -> None:
        self.assertTrue(valid_gpu_job_attempt(ATTEMPT))
        for bad in ("short", "x" * 65, "has space in it ok", "slash/in/the/attempt", 12345678901234567):
            self.assertFalse(valid_gpu_job_attempt(bad), bad)


class ModalGpuJobsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.clock = Clock()
        self.store = ModalDictStore(FakeModalDict())
        self.modal = FakeModal()
        self.jobs = ModalGpuJobs(self.store, self.modal.from_id, ModalErrors, clock=self.clock)
        self.page = grey_png()
        self.meta = denoise_meta(self.page)

    def submit(self, attempt: str = ATTEMPT, admission=None) -> Dict[str, Any]:
        return self.jobs.submit("denoise", self.modal.spawn, self.meta, self.page, attempt, admission=admission)

    def status(self, handle: str) -> Optional[Dict[str, Any]]:
        return self.jobs.status("denoise", handle, denoise_failure)

    def tracked(self) -> List[str]:
        doc = self.store.get(ANALYSIS_CALLS_KEY)
        return [c["id"] for c in doc["calls"]] if doc else []

    def test_a_repeated_submit_answers_the_same_job_and_spawns_once(self) -> None:
        first = self.submit()
        again = self.submit()
        self.assertEqual(first, again)
        self.assertEqual(first["status"], "running")
        self.assertEqual(first["handle"], gpu_job_handle("denoise", self.meta["request_digest"], ATTEMPT))
        self.assertEqual(len(self.modal.spawned), 1, "a repeated submit spawned a second GPU call")
        self.assertEqual(self.tracked(), ["fc-1"])
        # A new attempt of the same page is a new job.
        other = self.submit("attempt-second-0123456")
        self.assertNotEqual(other["handle"], first["handle"])
        self.assertEqual(len(self.modal.spawned), 2)

    def test_status_never_waits_and_answers_the_result_until_collected(self) -> None:
        handle = self.submit()["handle"]
        self.assertEqual(self.status(handle)["status"], "running")
        call = self.modal.calls["fc-1"]
        call.error, call.result = None, {"page_png": b"png", "request_digest": self.meta["request_digest"]}
        done = self.status(handle)
        self.assertEqual((done["status"], done["result"]["page_png"]), ("completed", b"png"))
        # The output stays with the provider: a repeated status answers it again.
        self.assertEqual(self.status(handle)["result"]["page_png"], b"png")
        self.assertEqual(self.tracked(), [], "a collected call stayed listed and would hold an idle release")
        self.assertIsNone(self.jobs.status("analysis", handle, analysis_failure), "kinds share a handle space")
        self.assertIsNone(self.status("denoise-" + "0" * 32))

    def test_failures_keep_their_typed_codes(self) -> None:
        cases = [
            (d.DenoiseError("weights gone", "weights_missing"), "capability_unavailable"),
            (d.DenoiseError("P page", "unsupported_mode"), "unsupported_page"),
            (RuntimeError("boom"), "inference_failed"),
            (ModalErrors.OutputExpiredError("gone"), "result_expired"),
            (ModalErrors.FunctionTimeoutError("slow"), "worker_timeout"),
        ]
        for index, (error, code) in enumerate(cases):
            handle = self.submit(f"attempt-failure-{index:04d}")["handle"]
            self.modal.calls[f"fc-{index + 1}"].error = error
            view = self.status(handle)
            self.assertEqual((view["status"], view["error"]["error_code"]), ("failed", code), error)
            # Terminal and final: a later poll reads the document, not the call.
            self.modal.calls[f"fc-{index + 1}"].error = None
            self.assertEqual(self.status(handle)["status"], "failed")
        self.assertEqual(self.tracked(), [])
        # An analysis call's own refusal (graphs gone) keeps its code too.
        handle = self.jobs.submit("analysis", self.modal.spawn, self.meta, self.page, ATTEMPT)["handle"]
        self.modal.calls["fc-6"].error = AnalysisUnavailable("gone")
        self.assertEqual(self.jobs.status("analysis", handle, analysis_failure)["error"]["error_code"], "capability_unavailable")

    def test_a_provider_hiccup_keeps_the_last_state(self) -> None:
        handle = self.submit()["handle"]
        self.modal.calls["fc-1"].error = ModalErrors.ConnectionError("blip")
        self.assertEqual(self.status(handle)["status"], "running")
        self.assertEqual(self.tracked(), ["fc-1"])

    def test_cancel_reaches_the_call_and_keeps_the_container(self) -> None:
        handle = self.submit()["handle"]
        view = self.jobs.cancel("denoise", handle)
        self.assertEqual(view["status"], "cancelled")
        self.assertEqual(self.modal.calls["fc-1"].cancels, [{"terminate_containers": False}])
        self.assertEqual(self.tracked(), [])
        self.assertEqual(self.status(handle)["status"], "cancelled")
        # Idempotent: a second cancel does not reach the provider again.
        self.assertEqual(self.jobs.cancel("denoise", handle)["status"], "cancelled")
        self.assertEqual(len(self.modal.calls["fc-1"].cancels), 1)
        self.assertIsNone(self.jobs.cancel("denoise", "denoise-" + "1" * 32))

    def test_a_failed_cancel_leaves_the_call_for_a_stop(self) -> None:
        handle = self.submit()["handle"]

        def refuse(**_: Any) -> None:
            raise ModalErrors.InternalError("no")
        self.modal.calls["fc-1"].cancel = refuse  # type: ignore[method-assign]
        self.assertEqual(self.jobs.cancel("denoise", handle)["status"], "running")
        self.assertEqual(self.tracked(), ["fc-1"])
        # The call then ends as cancelled, not failed, since the app asked for it.
        self.modal.calls["fc-1"].error = RuntimeError("interrupted")
        self.assertEqual(self.status(handle)["status"], "cancelled")

    def test_a_gpu_stop_cancels_a_polled_job(self) -> None:
        self.submit()
        self.assertEqual(count_tracked_analysis(self.store, clock=self.clock), 1)
        self.assertEqual(cancel_tracked_analysis(self.store, self.modal.from_id, clock=self.clock), 1)
        self.assertEqual(self.modal.calls["fc-1"].cancels, [{"terminate_containers": True}])

    def test_a_refused_admission_claims_nothing(self) -> None:
        @contextmanager
        def refused():
            raise AnalysisUnavailable("The GPU was just stopped")
            yield  # pragma: no cover

        with self.assertRaises(AnalysisUnavailable):
            self.submit(admission=refused)
        self.assertIsNone(self.store.get(gpu_job_key(gpu_job_handle("denoise", self.meta["request_digest"], ATTEMPT))))
        self.assertEqual(self.modal.spawned, [])

    def test_a_failed_spawn_is_never_repeated(self) -> None:
        self.modal.fail_spawn = True
        with self.assertRaises(ModalErrors.InternalError):
            self.submit()
        self.modal.fail_spawn = False
        again = self.submit()
        self.assertEqual((again["status"], again["error"]["error_code"]), ("failed", "dispatch_failed"))
        self.assertEqual(self.modal.spawned, [])

    def test_a_claim_without_a_call_is_lost_after_the_grace(self) -> None:
        handle = gpu_job_handle("denoise", self.meta["request_digest"], ATTEMPT)
        self.store.put(gpu_job_key(handle), {
            "v": 1, "kind": "denoise", "handle": handle, "request_digest": self.meta["request_digest"],
            "call_id": None, "status": "pending", "error": None, "cancel_requested": False,
            "created_at": self.clock.now, "updated_at": self.clock.now})
        self.assertEqual(self.status(handle)["status"], "pending")
        # A cancel before the call id is known is kept for the submit that spawns it.
        self.assertEqual(self.jobs.cancel("denoise", handle)["status"], "pending")
        self.clock.now += GPU_JOB_DISPATCH_GRACE_SECONDS + 1
        view = self.status(handle)
        self.assertEqual((view["status"], view["error"]["error_code"]), ("failed", "dispatch_lost"))


class GatewayJobRoutesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.clock = Clock()
        self.store = ModalDictStore(FakeModalDict())
        self.modal = FakeModal()
        self.jobs = ModalGpuJobs(self.store, self.modal.from_id, ModalErrors, clock=self.clock)
        self.gateway = CloudGateway(
            "modal", trust_edge_auth=True,
            analysis_worker=Proxy(self.jobs, "analysis", self.modal.spawn),
            denoise_worker=Proxy(self.jobs, "denoise", self.modal.spawn),
        )
        self.page = grey_png()
        self.tile = rgb_png()

    def call(self, method: str, path: str, payload: Any = None):
        body = json.dumps(payload).encode() if payload is not None else b""
        status, headers, raw = self.gateway.handle_http_request(method, path, {"Content-Type": "application/json"}, body)
        return status, json.loads(raw)

    def submit_page(self, attempt: str = ATTEMPT):
        return self.call("POST", "/mc/denoise/v1/jobs", {
            "metadata": denoise_meta(self.page), "page_png_b64": base64.b64encode(self.page).decode(),
            "attempt_id": attempt})

    def test_capabilities_advertise_the_job_operations(self) -> None:
        _, caps = self.call("GET", "/mc/v1/capabilities")
        self.assertIn(GPU_JOB_OPERATIONS["analysis"], caps["operations"])
        self.assertIn(GPU_JOB_OPERATIONS["denoise"], caps["operations"])
        # A worker without the job methods (Beam, or a test double of the synchronous
        # route) is not advertised, and the app keeps the synchronous route.
        plain = CloudGateway("modal", trust_edge_auth=True, denoise_worker=object())
        status, _, raw = plain.handle_http_request("GET", "/mc/v1/capabilities", {}, b"")
        self.assertNotIn("denoise.jobs", json.loads(raw)["operations"])
        status, _, raw = plain.handle_http_request("POST", "/mc/denoise/v1/jobs", {"Content-Type": "application/json"}, b"{}")
        self.assertEqual((status, json.loads(raw)["error_code"]), (503, "capability_unavailable"))

    def test_page_submit_poll_and_result(self) -> None:
        status, accepted = self.submit_page()
        self.assertEqual(status, 202)
        digest = denoise_meta(self.page)["request_digest"]
        self.assertEqual(accepted, {"protocol_version": d.DENOISE_VERSION, "request_digest": digest,
                                    "handle": gpu_job_handle("denoise", digest, ATTEMPT), "status": "running",
                                    "error": None, "result": None})
        handle = accepted["handle"]
        status, running = self.call("GET", f"/mc/denoise/v1/jobs/{handle}")
        self.assertEqual((status, running["status"], running["result"]), (200, "running", None))
        call = self.modal.calls["fc-1"]
        call.error, call.result = None, {"protocol_version": d.DENOISE_VERSION, "request_digest": digest,
                                         "page_png": self.page, "info": {"steps": []}}
        status, done = self.call("GET", f"/mc/denoise/v1/jobs/{handle}")
        self.assertEqual((status, done["status"]), (200, "completed"))
        self.assertEqual(base64.b64decode(done["result"]["page_png_b64"]), self.page)
        self.assertEqual(done["result"]["request_digest"], digest)
        # The same body again answers the same job; nothing is spawned twice.
        self.assertEqual(self.submit_page()[1]["handle"], handle)
        self.assertEqual(len(self.modal.spawned), 1)

    def test_tile_submit_poll_and_cancel(self) -> None:
        meta = analysis_meta(self.tile)
        status, accepted = self.call("POST", "/mc/analysis/v1/jobs", {
            "metadata": meta, "tile_png_b64": base64.b64encode(self.tile).decode(), "attempt_id": ATTEMPT})
        self.assertEqual((status, accepted["protocol_version"]), (202, ANALYSIS_VERSION))
        handle = accepted["handle"]
        self.assertTrue(handle.startswith("analysis-"))
        status, cancelled = self.call("POST", f"/mc/analysis/v1/jobs/{handle}/cancel")
        self.assertEqual((status, cancelled["status"]), (200, "cancelled"))
        self.assertEqual(self.modal.calls["fc-1"].cancels, [{"terminate_containers": False}])

    def test_completed_tile_answers_the_synchronous_shape(self) -> None:
        meta = analysis_meta(self.tile)
        _, accepted = self.call("POST", "/mc/analysis/v1/jobs", {
            "metadata": meta, "tile_png_b64": base64.b64encode(self.tile).decode(), "attempt_id": ATTEMPT})
        call = self.modal.calls["fc-1"]
        call.error, call.result = None, {"request_digest": meta["request_digest"], "mask_png": b"mask",
                                         "components": [], "boxes": []}
        _, done = self.call("GET", f"/mc/analysis/v1/jobs/{accepted['handle']}")
        self.assertEqual(done["result"]["mask_png_b64"], base64.b64encode(b"mask").decode())
        self.assertNotIn("mask_png", done["result"])

    def test_bad_requests_never_reach_the_gpu(self) -> None:
        status, body = self.submit_page("bad attempt")
        self.assertEqual((status, body["error_code"]), (400, "invalid_request"))
        status, body = self.call("POST", "/mc/denoise/v1/jobs", {
            "metadata": {**denoise_meta(self.page), "request_digest": "0" * 64},
            "page_png_b64": base64.b64encode(self.page).decode(), "attempt_id": ATTEMPT})
        self.assertEqual(status, 400)
        self.assertEqual(self.modal.spawned, [])
        for path in ("/mc/denoise/v1/jobs/denoise-nothex", "/mc/denoise/v1/jobs/" + "analysis-" + "0" * 32,
                     "/mc/denoise/v1/jobs/denoise-" + "0" * 32):
            self.assertEqual(self.call("GET", path)[0], 404, path)

    def test_a_slow_job_is_polled_past_the_redirect_without_a_long_request(self) -> None:
        """A page that queues and runs 400 s of simulated time: every request returns
        at once, the GPU call is only ever asked with get(timeout=0), and the job is
        collected well past Modal's 150 s redirect."""
        timeouts: List[Any] = []
        finish_at = self.clock.now + 400.0
        digest = denoise_meta(self.page)["request_digest"]
        clock = self.clock

        class SlowCall(FakeFunctionCall):
            def get(self, timeout: Optional[float] = None) -> Any:
                timeouts.append(timeout)
                if clock.now < finish_at:
                    raise TimeoutError()
                return {"protocol_version": d.DENOISE_VERSION, "request_digest": digest,
                        "page_png": b"page", "info": {"steps": []}}

        def slow_spawn(metadata, png):
            call = SlowCall("fc-slow")
            self.modal.calls[call.object_id] = call
            return call

        self.gateway.denoise_worker = Proxy(self.jobs, "denoise", slow_spawn)
        started = self.clock.now
        longest = 0.0

        def timed(method, path, payload=None):
            nonlocal longest
            wall = time.monotonic()
            answer = self.call(method, path, payload)
            longest = max(longest, time.monotonic() - wall)
            return answer

        status, accepted = timed("POST", "/mc/denoise/v1/jobs", {
            "metadata": denoise_meta(self.page), "page_png_b64": base64.b64encode(self.page).decode(),
            "attempt_id": ATTEMPT})
        self.assertEqual(status, 202)
        polls = 0
        while True:
            self.clock.now += 2.0  # the app's poll interval
            status, view = timed("GET", f"/mc/denoise/v1/jobs/{accepted['handle']}")
            polls += 1
            self.assertEqual(status, 200)
            if view["status"] != "running":
                break
            self.assertLess(polls, 1000)
        self.assertEqual(view["status"], "completed")
        self.assertGreater(self.clock.now - started, MODAL_WEB_REDIRECT_SECONDS)
        self.assertTrue(all(t == 0 for t in timeouts), timeouts)
        self.assertLess(longest, 1.0, "a job request waited")


class AnalysisBatchTest(unittest.TestCase):
    """One page's tile requests in one GPU job (`POST /mc/analysis/v1/batches`)."""

    def setUp(self) -> None:
        self.store = ModalDictStore(FakeModalDict())
        self.modal = FakeModal()
        self.jobs = ModalGpuJobs(self.store, self.modal.from_id, ModalErrors, clock=Clock())
        self.gateway = CloudGateway("modal", trust_edge_auth=True,
                                    analysis_worker=Proxy(self.jobs, "analysis", self.modal.spawn))
        self.tile = rgb_png()
        self.requests = [analysis_meta(self.tile, ANALYSIS_SAM), analysis_meta(self.tile, ANALYSIS_RT)]

    def call(self, method: str, path: str, payload: Any = None):
        body = json.dumps(payload).encode() if payload is not None else b""
        status, _, raw = self.gateway.handle_http_request(method, path, {"Content-Type": "application/json"}, body)
        return status, json.loads(raw)

    def body(self, requests=None, tiles=None, digest=None, attempt=ATTEMPT) -> Dict[str, Any]:
        requests = self.requests if requests is None else requests
        tiles = [self.tile] if tiles is None else tiles
        return {"metadata": {"protocol_version": ANALYSIS_VERSION, "requests": requests,
                             "request_digest": digest or analysis_batch_digest([r["request_digest"] for r in requests])},
                "tiles_png_b64": [base64.b64encode(tile).decode() for tile in tiles], "attempt_id": attempt}

    def test_the_digest_is_shared_with_the_desktop(self) -> None:
        # The same vector is asserted in cleaner_core::cloud_analysis_wire.
        self.assertEqual(analysis_batch_digest(["a" * 64, "b" * 64]),
                         "913f9338fb6c17253f3a14816fc08d52522454bfc19a8c99538a197cfb23fb41")

    def test_capabilities_advertise_batches_only_with_the_batch_method(self) -> None:
        _, caps = self.call("GET", "/mc/v1/capabilities")
        self.assertIn(GPU_JOB_OPERATIONS["analysis_batch"], caps["operations"])
        tiles_only = Proxy(self.jobs, "analysis", self.modal.spawn)
        tiles_only.submit_batch_job = None
        older = CloudGateway("modal", trust_edge_auth=True, analysis_worker=tiles_only)
        _, _, raw = older.handle_http_request("GET", "/mc/v1/capabilities", {}, b"")
        operations = json.loads(raw)["operations"]
        self.assertIn("analysis.jobs", operations)
        self.assertNotIn("analysis.batches", operations)

    def test_one_job_runs_every_request_and_answers_in_order(self) -> None:
        status, accepted = self.call("POST", "/mc/analysis/v1/batches", self.body())
        self.assertEqual(status, 202)
        digest = analysis_batch_digest([r["request_digest"] for r in self.requests])
        handle = gpu_job_handle("analysis_batch", digest, ATTEMPT)
        self.assertEqual((accepted["handle"], accepted["request_digest"]), (handle, digest))
        # One GPU call; the one tile PNG serves both models.
        self.assertEqual(len(self.modal.spawned), 1)
        metadata, pairs = self.modal.spawned[0]
        self.assertEqual(metadata["request_digest"], digest)
        self.assertEqual([(request["capability"], png) for request, png in pairs],
                         [(ANALYSIS_SAM, self.tile), (ANALYSIS_RT, self.tile)])
        call = self.modal.calls["fc-1"]
        call.error, call.result = None, {"protocol_version": ANALYSIS_VERSION, "request_digest": digest, "results": [
            {"request_digest": self.requests[0]["request_digest"], "mask_png": b"mask", "components": [], "boxes": []},
            {"request_digest": self.requests[1]["request_digest"], "mask_png": None, "components": [], "boxes": []}]}
        status, done = self.call("GET", f"/mc/analysis/v1/batches/{handle}")
        self.assertEqual((status, done["status"]), (200, "completed"))
        self.assertEqual(done["result"]["request_digest"], digest)
        results = done["result"]["results"]
        self.assertEqual([r["request_digest"] for r in results], [r["request_digest"] for r in self.requests])
        self.assertEqual((results[0]["mask_png_b64"], results[1]["mask_png_b64"]),
                         (base64.b64encode(b"mask").decode(), None))
        # The same body again names the same job and spawns nothing.
        self.assertEqual(self.call("POST", "/mc/analysis/v1/batches", self.body())[1]["handle"], handle)
        self.assertEqual(len(self.modal.spawned), 1)

    def test_a_batch_is_cancelled_by_its_own_handle_only(self) -> None:
        _, accepted = self.call("POST", "/mc/analysis/v1/batches", self.body())
        handle = accepted["handle"]
        self.assertEqual(self.call("GET", f"/mc/analysis/v1/jobs/{handle}")[0], 404)
        status, cancelled = self.call("POST", f"/mc/analysis/v1/batches/{handle}/cancel")
        self.assertEqual((status, cancelled["status"]), (200, "cancelled"))
        self.assertEqual(self.modal.calls["fc-1"].cancels, [{"terminate_containers": False}])

    def test_malformed_batches_never_reach_the_gpu(self) -> None:
        out = io.BytesIO()
        Image.new("RGB", (16, 12), (1, 2, 3)).save(out, format="PNG")
        unused = out.getvalue()
        sam, rt = self.requests
        cases = {
            "wrong digest": self.body(digest="0" * 64),
            "unused tile": self.body(tiles=[self.tile, unused]),
            "missing tile": self.body(tiles=[unused]),
            "repeated tile": self.body(tiles=[self.tile, self.tile]),
            "repeated request": self.body(requests=[sam, sam]),
            "two pages": self.body(requests=[sam, analysis_meta(self.tile, ANALYSIS_RT, page="f" * 64)]),
            "no requests": self.body(requests=[]),
            "too many requests": self.body(requests=[sam] * (ANALYSIS_BATCH_MAX_REQUESTS + 1)),
            "bad attempt": self.body(attempt="bad attempt"),
        }
        for name, body in cases.items():
            status, answer = self.call("POST", "/mc/analysis/v1/batches", body)
            self.assertEqual((status, answer["error_code"]), (400, "invalid_request"), name)
        self.assertEqual(self.modal.spawned, [])


class SynchronousWaitTest(unittest.TestCase):
    def test_the_synchronous_wait_ends_before_the_redirect_and_cancels(self) -> None:
        self.assertLess(SYNC_GPU_WAIT_SECONDS, MODAL_WEB_REDIRECT_SECONDS)
        store = ModalDictStore(FakeModalDict())
        call = FakeFunctionCall("fc-sync")
        waited: List[Any] = []

        def get(timeout=None):
            waited.append(timeout)
            raise TimeoutError()
        call.get = get  # type: ignore[method-assign]
        with self.assertRaises(GpuWaitExceeded):
            run_tracked_analysis(lambda *_: call, store, {}, b"", clock=lambda: 5.0)
        self.assertEqual(waited, [SYNC_GPU_WAIT_SECONDS])
        self.assertEqual(call.cancels, [{"terminate_containers": False}])
        self.assertIsNone(store.get(ANALYSIS_CALLS_KEY))

        def refuse(**_: Any) -> None:
            raise ModalErrors.InternalError("no")
        call.cancel = refuse  # type: ignore[method-assign]
        with self.assertRaises(GpuWaitExceeded):
            run_tracked_analysis(lambda *_: call, store, {}, b"", clock=lambda: 5.0)
        self.assertEqual([c["id"] for c in store.get(ANALYSIS_CALLS_KEY)["calls"]], ["fc-sync"],
                         "a call whose cancel failed must stay reachable by a stop")

    def test_the_gateway_answers_504_for_an_exceeded_wait(self) -> None:
        class Waited:
            def capabilities(self):
                return {}

            def denoise(self, metadata, page_png):
                raise GpuWaitExceeded("no answer")

        gateway = CloudGateway("modal", trust_edge_auth=True, denoise_worker=Waited())
        page = grey_png()
        body = json.dumps({"metadata": denoise_meta(page), "page_png_b64": base64.b64encode(page).decode()}).encode()
        status, _, raw = gateway.handle_http_request("POST", "/mc/denoise/v1/page", {"Content-Type": "application/json"}, body)
        self.assertEqual((status, json.loads(raw)["error_code"]), (504, "inference_failed"))


if __name__ == "__main__":
    unittest.main()
