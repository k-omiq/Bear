"""Offline deployment and interrupted-submission compatibility checks."""
import json
import re
import unittest
from pathlib import Path
from unittest.mock import Mock

from deploy.cloud.common.api import CloudGateway
from deploy.cloud.common import analysis_seed, manifest
from deploy.cloud.common.contract import PROTOCOL_VERSION, validate_response_binding, JobStatusResponse
from deploy.cloud.common.jobs import DispatchJobBackend, MemoryStore, WorkerOutcome, dispatch_handle
from deploy.cloud.common.manifest import production_limits
from deploy.cloud.tests.support import make_job
from deploy.cloud.tests.test_dispatch import FakeDispatcher

ROOT = Path(__file__).resolve().parents[3]


class RecoveryContractTest(unittest.TestCase):
    def gateway(self, **kwargs):
        return CloudGateway("modal", proxy_token_id="id", proxy_token_secret="secret", **kwargs)

    headers = {"Modal-Key": "id", "Modal-Secret": "secret"}

    def test_capabilities_require_auth_and_do_not_dispatch(self):
        backend = Mock()
        gpu = Mock()
        gateway = self.gateway(backend=backend, gpu_control=gpu)
        self.assertEqual(gateway.handle_http_request("GET", "/mc/v1/capabilities", {}, b"")[0], 401)
        code, _, body = gateway.handle_http_request("GET", "/mc/v1/capabilities", self.headers, b"")
        self.assertEqual(code, 200)
        payload = json.loads(body)
        self.assertEqual(payload["protocol_version"], PROTOCOL_VERSION)
        self.assertEqual(payload["provider"], "modal")
        backend.assert_not_called()
        self.assertEqual(backend.mock_calls, [])
        self.assertEqual(gpu.mock_calls, [])

    def test_every_required_client_operation_has_a_served_route(self):
        source = (ROOT / "src-tauri/src/inference/http.rs").read_text()
        block = re.search(r"REQUIRED_GATEWAY_OPERATIONS:.*?= &\[(.*?)\];", source, re.S).group(1)
        required = set(re.findall(r'"([^"]+)"', block))
        routes = {
            "jobs.submit": ("POST", "/jobs", "_route_submit_job"),
            "jobs.status": ("GET", "/jobs/handle-test", "_route_job_status"),
            "jobs.result": ("GET", "/jobs/handle-test/result", "_route_job_result"),
            "jobs.cancel": ("POST", "/jobs/handle-test/cancel", "_route_job_cancel"),
            "gpu.status": ("GET", "/gpu", "_route_gpu_status"),
            "gpu.stop": ("POST", "/gpu/stop", "_route_gpu_stop"),
            "gpu.release_idle": ("POST", "/gpu/stop", "_route_gpu_stop"),
        }
        gpu_block = re.search(r"MODAL_GPU_OPERATIONS:.*?= &\[(.*?)\];", source, re.S).group(1)
        gpu_required = set(re.findall(r'"([^"]+)"', gpu_block))
        self.assertEqual(required | gpu_required, set(routes))
        self.assertTrue(all(not operation.startswith("gpu.") for operation in required))
        beam = CloudGateway("beam", bearer_token="beam-token")
        code, _, body = beam.handle_http_request("GET", "/mc/v1/capabilities", {"Authorization": "Bearer beam-token"}, b"")
        self.assertEqual(code, 200)
        payload = json.loads(body)
        self.assertEqual(payload["provider"], "beam")
        self.assertEqual(payload["protocol_version"], PROTOCOL_VERSION)
        self.assertTrue(required.issubset(payload["operations"]))
        self.assertTrue(gpu_required.isdisjoint(payload["operations"]))
        modal = self.gateway(gpu_control=Mock())
        for gateway, headers, operations in [(beam, {"Authorization": "Bearer beam-token"}, required),
                                              (modal, self.headers, required | gpu_required)]:
            _, _, body = gateway.handle_http_request("GET", "/mc/v1/capabilities", headers, b"")
            self.assertTrue(operations.issubset(json.loads(body)["operations"]))
            for operation in operations:
                method, route, handler = routes[operation]
                with self.subTest(provider=json.loads(body)["provider"], operation=operation):
                    original = getattr(gateway, handler)
                    mocked = Mock(return_value=(200, {}, b"{}"))
                    setattr(gateway, handler, mocked)
                    self.assertEqual(gateway.handle_http_request(method, "/mc/v1" + route, headers, b"{}")[0], 200)
                    mocked.assert_called_once()
                    setattr(gateway, handler, original)

    def test_idle_release_is_forwarded_as_idle_only(self):
        gpu = Mock()
        gpu.stop.return_value = {"provider": "modal", "stopped": [], "cancelled_jobs": 0}
        gateway = self.gateway(gpu_control=gpu)
        response = gateway.handle_http_request("POST", "/mc/v1/gpu/stop", self.headers,
                                          b'{"role":"render","idle_only":true}')
        self.assertEqual(response[0], 200)
        gpu.stop.assert_called_once_with("render", idle_only=True)

    def test_missing_gpu_control_is_not_advertised(self):
        _, _, body = self.gateway().handle_http_request("GET", "/mc/v1/capabilities", self.headers, b"")
        self.assertNotIn("gpu.release_idle", json.loads(body)["operations"])

    def test_modal_lookup_survives_gateway_restart_without_dispatch(self):
        store, dispatcher = MemoryStore(), FakeDispatcher()
        meta, image, hint = make_job()
        backend = DispatchJobBackend("modal", store, dispatcher, production_limits())
        accepted = backend.submit(meta, image, hint)
        handle = dispatch_handle("modal", meta.job_id, meta.attempt_id)
        self.assertEqual(handle, accepted.app_handle)
        dispatcher.outcome = WorkerOutcome("running")
        gateway = self.gateway(backend=DispatchJobBackend("modal", store, dispatcher, production_limits()))
        self.assertEqual(gateway.handle_http_request("GET", "/mc/v1/jobs/" + handle, {}, b"")[0], 401)
        code, _, body = gateway.handle_http_request("GET", "/mc/v1/jobs/" + handle, self.headers, b"")
        self.assertEqual(code, 200)
        status = JobStatusResponse.from_dict(json.loads(body))
        validate_response_binding(meta, status, handle)
        self.assertEqual(len(dispatcher.dispatched), 1)
        self.assertEqual(gateway.handle_http_request("GET", "/mc/v1/jobs/handle-modal-missing", self.headers, b"")[0], 404)
        self.assertEqual(len(dispatcher.dispatched), 1)

    def test_modal_handle_contract_vector(self):
        self.assertEqual(dispatch_handle("modal", "job-test", "att-test"),
                         "handle-modal-32f9dc0aafefdec6245d2cf8723c7b70")

    def test_analysis_model_revisions_and_desktop_graph_pins_agree(self):
        source = (ROOT / "src-tauri/src/model_workflows.rs").read_text()
        # The pins live once in cleaner-core; the desktop workflows alias them.
        groups = (ROOT / "crates/cleaner-core/src/text_groups.rs").read_text()
        self.assertIn('pub const SAM_TS_L_REVISION: &str = "' + analysis_seed.SAM_REVISION + '"', groups)
        self.assertIn('pub const OGKALU_FULL_REVISION: &str = "' + analysis_seed.RT_REVISION + '"', groups)
        self.assertIn("const SAM_REVISION: &str = cleaner_core::text_groups::SAM_TS_L_REVISION;", source)
        self.assertIn("const FULL_RT_REVISION: &str = cleaner_core::text_groups::OGKALU_FULL_REVISION;", source)
        self.assertIn(analysis_seed.SAM_HEAD_SHA256_MAC, source)
        for capability in [analysis_seed.ANALYSIS_SAM, analysis_seed.ANALYSIS_RT]:
            # The SAM head has explicitly separate macOS and Linux export hashes.
            for name, _, digest in analysis_seed.GRAPH_FILES[capability]:
                if name != "koharu_samts_text_head.onnx":
                    self.assertIn(digest, source)

    def test_client_captures_advertised_recipe_and_mock_pins_match_production(self):
        source = (ROOT / "src-tauri/src/inference/cloud_clean.rs").read_text()
        for field in ["recipe_id", "preprocessing_version", "model_id", "model_revision", "native_mask_conditioning"]:
            self.assertIn("info." + field, source)
        mock = (ROOT / "src/lib/api/mock.js").read_text()
        for pin in [manifest.MODEL_PROD_FLUX, manifest.REVISION_PROD_FLUX, manifest.RECIPE_PROD_SDNQ]:
            self.assertIn(pin, mock)

    def test_modal_deployment_wires_both_release_methods_and_persistent_store(self):
        source = (ROOT / "deploy/cloud/modal/app.py").read_text()
        self.assertIn('"render": lambda: Worker().release.spawn()', source)
        self.assertIn('"analysis": lambda: AnalysisGPU().release.spawn()', source)
        self.assertEqual(source.count("def release(self)"), 2)
        self.assertIn("DispatchJobBackend(", source)
        rust_wire = (ROOT / "crates/cleaner-core/src/cloud_wire.rs").read_text()
        self.assertIn('pub const PROTOCOL_VERSION: &str = "' + PROTOCOL_VERSION + '"', rust_wire)
