"""The deployed shape end to end, in one process.

The /mc/v1 ASGI app, the dispatch backend, the provider store and the GPU task body
run exactly as deployed; only the provider transport is a fake. A test plays the
provider by running the worker body when the queue would.
"""

from __future__ import annotations

import json
import tempfile
import unittest
from typing import Any, Dict, List

from deploy.cloud.beam.backend import BeamMapStore, BeamWorkerDispatcher, beam_worker_render
from deploy.cloud.common.api import CloudGateway
from deploy.cloud.common.jobs import DispatchJobBackend
from deploy.cloud.common.manifest import (
    MODEL_PROD_FLUX,
    RECIPE_PROD_SDNQ,
    REVISION_PROD_FLUX,
    get_production_model_info,
    production_limits,
)
from deploy.cloud.common.worker import WorkerRuntime
from deploy.cloud.modal.backend import ModalDictStore, ModalWorkerDispatcher, worker_render
from deploy.cloud.tests.support import (
    FakeBeamMap,
    FakeFunctionCall,
    FakeModalDict,
    FakeRunner,
    ModalErrors,
    asgi_request,
    make_job,
    metadata_json,
    multipart_body,
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


class ModalHarness:
    provider = "modal"

    def __init__(self, runtime: WorkerRuntime):
        self.runtime = runtime
        self.store = ModalDictStore(FakeModalDict())
        self.calls: Dict[str, FakeFunctionCall] = {}
        self.queued: Dict[str, tuple] = {}
        self.fail_dispatch = False
        self.dispatcher = ModalWorkerDispatcher(self.spawn, self.calls.__getitem__, ModalErrors, self.store)

    def spawn(self, handle: str, metadata: str, image: bytes, hint: bytes) -> FakeFunctionCall:
        if self.fail_dispatch:
            raise ModalErrors.ConnectionError("queue unreachable")
        call = FakeFunctionCall(f"fc-{len(self.calls) + 1}")
        self.calls[call.object_id] = call
        self.queued[handle] = (call, metadata, image, hint)
        return call

    @property
    def dispatch_count(self) -> int:
        return len(self.calls)

    def run_worker(self, handle: str) -> None:
        call, metadata, image, hint = self.queued.pop(handle)
        call.result = worker_render(self.runtime, self.store, handle, metadata, image, hint)
        call.error = None


class BeamHarness:
    provider = "beam"

    def __init__(self, runtime: WorkerRuntime):
        self.runtime = runtime
        self.store = BeamMapStore(FakeBeamMap())
        self.enqueued: List[str] = []
        self.stopped: List[str] = []
        self.fail_dispatch = False
        self.dispatcher = BeamWorkerDispatcher(self.store, self.enqueue, stop_task=self.stopped.append)

    def enqueue(self, handle: str) -> str:
        if self.fail_dispatch:
            raise ConnectionError("queue unreachable")
        self.enqueued.append(handle)
        return f"task-{len(self.enqueued)}"

    @property
    def dispatch_count(self) -> int:
        return len(self.enqueued)

    def run_worker(self, handle: str) -> None:
        beam_worker_render(self.runtime, self.store, handle, sleep=lambda s: None)


class EndToEndCases:
    harness_class: Any = None

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        write_marker(self._tmp.name)
        self.runners: List[FakeRunner] = []
        runtime = WorkerRuntime(self._tmp.name, runner_factory=self._runner)
        self.harness = self.harness_class(runtime)
        self.app = self.gateway_app()

    def _runner(self, path: str) -> FakeRunner:
        runner = FakeRunner(path)
        self.runners.append(runner)
        return runner

    def gateway_app(self):
        limits = production_limits()
        gateway = CloudGateway(
            provider=self.harness.provider,
            model_info=get_production_model_info(self.harness.provider),
            limits=limits,
            backend=DispatchJobBackend(self.harness.provider, self.harness.store, self.harness.dispatcher, limits),
            trust_edge_auth=True,
        )
        return gateway.as_asgi_app()

    def request(self, method: str, path: str, body: bytes = b"", content_type: str = "") -> tuple:
        headers = {"content-type": content_type} if content_type else {}
        status, headers, payload = asgi_request(self.app, method, path, headers, body)
        return status, headers, payload

    def submit(self, meta, image, hint) -> tuple:
        content_type, body = multipart_body(metadata_json(meta), image, hint)
        status, _, payload = self.request("POST", "/mc/v1/jobs", body, content_type)
        return status, json.loads(payload)

    def status(self, handle: str) -> Dict[str, Any]:
        status, _, payload = self.request("GET", f"/mc/v1/jobs/{handle}")
        self.assertEqual(status, 200)
        return json.loads(payload)

    def test_health_and_model_info_never_touch_the_gpu(self) -> None:
        status, _, payload = self.request("GET", "/mc/v1/health")
        self.assertEqual((status, json.loads(payload)["status"]), (200, "ok"))
        status, _, payload = self.request("GET", "/mc/v1/model-info")
        info = json.loads(payload)
        self.assertEqual(status, 200)
        self.assertEqual(
            (info["recipe_id"], info["model_id"], info["model_revision"], info["preprocessing_version"]),
            (RECIPE_PROD_SDNQ, MODEL_PROD_FLUX, REVISION_PROD_FLUX, "2.0.0"),
        )
        self.assertFalse(info["native_mask_conditioning"])
        self.assertEqual(info["limits"]["worker_timeout_seconds"], 600)
        status, _, payload = self.request("POST", "/mc/v1/warmup")
        self.assertEqual(json.loads(payload)["worker_state"], "on_demand")
        self.assertEqual((self.harness.dispatch_count, self.runners), (0, []))

    def test_submit_poll_result(self) -> None:
        meta, image, hint = make_job(width=48, height=32)
        status, accepted = self.submit(meta, image, hint)
        self.assertEqual((status, accepted["status"]), (202, "pending"))
        handle = accepted["handle"]
        self.assertEqual(self.status(handle)["status"], "pending")

        # A client that lost the response resubmits the same attempt.
        status, again = self.submit(meta, image, hint)
        self.assertEqual((status, again["handle"]), (202, handle))
        self.assertEqual(self.harness.dispatch_count, 1)

        self.harness.run_worker(handle)
        done = self.status(handle)
        self.assertEqual(done["status"], "completed")
        status, headers, png = self.request("GET", f"/mc/v1/jobs/{handle}/result")
        self.assertEqual((status, headers["content-type"]), (200, "image/png"))
        self.assertEqual(headers["x-mc-result-digest"], sha256(png))
        self.assertEqual(done["result_digest"], sha256(png))
        self.assertEqual(self.runners[0].calls, [(48, 32, 1, 4, 100)])

        status, _, payload = self.request("POST", f"/mc/v1/jobs/{handle}/cancel")
        self.assertEqual((status, json.loads(payload)["error_code"]), (409, "job_terminal"))

        # A new gateway container answers from the durable store.
        self.app = self.gateway_app()
        self.assertEqual(self.status(handle)["status"], "completed")
        status, _, again_png = self.request("GET", f"/mc/v1/jobs/{handle}/result")
        self.assertEqual((status, again_png), (200, png))

    def test_cancel_before_the_worker_starts(self) -> None:
        meta, image, hint = make_job(job_id="job-cancel")
        _, accepted = self.submit(meta, image, hint)
        handle = accepted["handle"]
        status, _, payload = self.request("POST", f"/mc/v1/jobs/{handle}/cancel")
        self.assertEqual((status, json.loads(payload)["status"]), (200, "cancel_requested"))
        self.harness.run_worker(handle)
        self.assertEqual(self.status(handle)["status"], "cancelled")
        self.assertEqual(self.runners, [], "the cancelled job never reached the model")

    def test_pinned_sampling_is_enforced_before_the_queue(self) -> None:
        meta, image, hint = make_job(seed=5)
        status, rejection = self.submit(meta, image, hint)
        self.assertEqual((status, rejection["error_code"], rejection["enqueued"]), (400, "unsupported_recipe", False))
        self.assertEqual(self.harness.dispatch_count, 0)

    def test_unreachable_queue_keeps_the_attempt_unknown(self) -> None:
        meta, image, hint = make_job(job_id="job-retry")
        self.harness.fail_dispatch = True
        status, rejection = self.submit(meta, image, hint)
        self.assertEqual((status, rejection["error_code"]), (503, "submission_unknown"))
        self.assertNotIn("retryable", rejection)
        self.harness.fail_dispatch = False
        status, accepted = self.submit(meta, image, hint)
        self.assertEqual(status, 202)
        self.assertEqual(self.harness.dispatch_count, 0)

    def test_worker_failure_is_reported_typed(self) -> None:
        meta, image, hint = make_job(job_id="job-fail")
        _, accepted = self.submit(meta, image, hint)
        self.harness.runtime.weights_root = self._tmp.name + "/missing"
        self.harness.run_worker(accepted["handle"])
        report = self.status(accepted["handle"])
        self.assertEqual((report["status"], report["error"]["error_code"]), ("failed", "weights_missing"))


class ModalEndToEndTest(EndToEndCases, unittest.TestCase):
    harness_class = ModalHarness

    def test_cancel_reaches_the_function_call(self) -> None:
        meta, image, hint = make_job(job_id="job-native-cancel")
        _, accepted = self.submit(meta, image, hint)
        self.request("POST", f"/mc/v1/jobs/{accepted['handle']}/cancel")
        self.assertEqual(self.harness.calls["fc-1"].cancels, [{"terminate_containers": False}])


class BeamEndToEndTest(EndToEndCases, unittest.TestCase):
    harness_class = BeamHarness

    def test_cancel_stops_the_task(self) -> None:
        meta, image, hint = make_job(job_id="job-native-cancel")
        _, accepted = self.submit(meta, image, hint)
        self.request("POST", f"/mc/v1/jobs/{accepted['handle']}/cancel")
        self.assertEqual(self.harness.stopped, ["task-1"])


try:
    from starlette.applications import Starlette
    from starlette.routing import Mount
except ImportError:  # the Beam container runtime ships Starlette; this Python may not
    Starlette = None


@unittest.skipIf(Starlette is None, "starlette is not installed")
class BeamStarletteMountTest(BeamEndToEndTest):
    """The Beam gateway serves the same app mounted at /mc/v1 inside Starlette."""

    def gateway_app(self):
        from deploy.cloud.beam.routes import mount_gateway_app
        return mount_gateway_app(super().gateway_app())

    def test_analysis_path_reaches_gateway(self):
        status, _, body = self.request("GET", "/mc/analysis/v1/capabilities")
        self.assertEqual(status, 503)
        self.assertEqual(json.loads(body)["error_code"], "capability_unavailable")


if __name__ == "__main__":
    unittest.main()
