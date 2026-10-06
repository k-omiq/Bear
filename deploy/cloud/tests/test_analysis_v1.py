"""Offline analysis contract, graph, tensor, and ASGI checks."""
from __future__ import annotations

import base64
import hashlib
import io
import json
import tempfile
import unittest
from pathlib import Path

import numpy as np
from PIL import Image

from deploy.cloud.common.analysis import AnalysisWorker, sam_prepare
from deploy.cloud.common.api import CloudGateway
from deploy.cloud.beam.routes import restore_gateway_path
from deploy.cloud.common.contract import (
    ANALYSIS_RT, ANALYSIS_SAM, ANALYSIS_TILE_CONTEXT, ANALYSIS_VERSION, ContractValidationError,
    analysis_request_digest, analysis_tile_core, analysis_tile_plan,
    validate_analysis_request, validate_analysis_result, validate_analysis_wire_result,
)
from deploy.cloud.tests.support import asgi_request

ROOT = Path(__file__).resolve().parents[3]
VECTORS = ROOT / "deploy/cloud/fixtures/analysis_v1/vectors.json"
SYNTHETIC = ROOT / "spikes/sam-ts-l/fixtures/synthetic_page.png"
PARITY = ROOT / "spikes/sam-ts-l/fixtures/fp32_parity.npz"


def tile_png(size=(16, 12)):
    image = Image.new("RGB", size, (100, 50, 25))
    buffer = io.BytesIO()
    image.save(buffer, format="PNG")
    return buffer.getvalue()


class FakeSession:
    def __init__(self, path, seen):
        self.path = path
        self.seen = seen

    def run(self, outputs, inputs):
        self.seen.append((self.path, outputs, inputs))
        if outputs == ["embedding"]:
            return [np.zeros((1, 256, 64, 64), dtype=np.float32)]
        if outputs == ["high_res_logits"]:
            logits = np.full((1, 1, 1024, 1024), -1, dtype=np.float32)
            logits[0, 0, 0:500, 0:500] = 1
            return [logits]
        return [np.array([[1]], dtype=np.int64), np.array([[[1, 2, 10, 9]]], dtype=np.float32), np.array([[0.9]], dtype=np.float32)]


class AnalysisCases(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.paths = []
        for name in ("encoder.onnx", "head.onnx", "rt.onnx"):
            path = Path(self.tmp.name) / name
            path.write_bytes(name.encode())
            self.paths.append(str(path))
        self.seen = []
        self.worker = AnalysisWorker(
            {ANALYSIS_SAM: self.paths[:2], ANALYSIS_RT: self.paths[2:]},
            {ANALYSIS_SAM: "c" * 40, ANALYSIS_RT: "c" * 40},
            lambda path: FakeSession(path, self.seen),
        )
        self.png = tile_png()

    def request(self, capability=ANALYSIS_SAM):
        graphs = self.paths[:2] if capability == ANALYSIS_SAM else self.paths[2:]
        request = {
            "protocol_version": ANALYSIS_VERSION, "capability": capability,
            "graph_sha256s": [hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in graphs],
            "model_revision": "c" * 40, "tile_id": "tile_01",
            "tile_rect": {"x": 0, "y": 0, "width": 16, "height": 12},
            "tile_png_sha256": hashlib.sha256(self.png).hexdigest(),
            "source_page_sha256": "e" * 64, "page_width": 16, "page_height": 12,
            "tile_core": {"x": 0, "y": 0, "width": 16, "height": 12},
        }
        request["request_digest"] = analysis_request_digest(request)[1]
        return request

    def test_shared_vectors_and_collisions(self):
        vectors = json.loads(VECTORS.read_text())
        self.assertNotEqual(vectors["valid"][0]["digest"], vectors["valid"][1]["digest"])
        for case in vectors["valid"]:
            canonical, digest = analysis_request_digest(case["request"])
            self.assertEqual(canonical, case["canonical"].encode())
            self.assertEqual(digest, case["digest"])
        for case in vectors["invalid"]:
            with self.assertRaises(ContractValidationError):
                analysis_request_digest(case)
        response = json.loads((VECTORS.parent / "sam_response.json").read_text())
        mask = validate_analysis_wire_result(response, vectors["valid"][0]["request"])
        self.assertEqual(response["request_digest"], vectors["valid_response"]["request_digest"])
        self.assertEqual(hashlib.sha256(mask).hexdigest(), vectors["valid_response"]["mask_png_sha256"])

    def test_shared_tile_plans_overlap_and_partition(self):
        plans = json.loads((VECTORS.parent / "tile_plans.json").read_text())
        for case in plans["plans"]:
            width, height = case["width"], case["height"]
            tiles = analysis_tile_plan(width, height)
            self.assertEqual(tiles, case["tiles"], (width, height))
            owned = 0
            for entry in tiles:
                tile, core = entry["tile"], entry["core"]
                self.assertEqual(analysis_tile_core(width, height, tile), core)
                self.assertEqual((tile["width"], tile["height"]), (min(width, 1024), min(height, 1024)))
                self.assertGreaterEqual(core["x"], tile["x"])
                self.assertLessEqual(core["x"] + core["width"], tile["x"] + tile["width"])
                if core["x"] > 0:
                    self.assertGreaterEqual(core["x"] - tile["x"], ANALYSIS_TILE_CONTEXT)
                if core["y"] + core["height"] < height:
                    self.assertGreaterEqual(tile["y"] + tile["height"] - core["y"] - core["height"], ANALYSIS_TILE_CONTEXT)
                owned += core["width"] * core["height"]
            self.assertEqual(owned, width * height)
            xs = sorted({entry["core"]["x"] for entry in tiles} - {0})
            ys = sorted({entry["core"]["y"] for entry in tiles} - {0})
            self.assertEqual([xs, ys], case["core_boundaries"])
        for case in plans["refused"]:
            width, height = case["width"], case["height"]
            with self.assertRaises(ContractValidationError):
                analysis_tile_plan(width, height)
            # No tile of a refused page is on a plan, including one each axis
            # alone would have.
            if width >= 1 and height >= 1:
                tile = {"x": 0, "y": 0, "width": min(width, 1024), "height": min(height, 1024)}
                with self.assertRaises(ContractValidationError):
                    analysis_tile_core(width, height, tile)
        # A tile the plan does not have, such as a 1.0.0 edge tile, is refused.
        self.assertIsNone(analysis_tile_core(1136, 1601, {"x": 1024, "y": 0, "width": 112, "height": 1024}))
        self.assertIsNone(analysis_tile_core(1136, 1601, {"x": 111, "y": 0, "width": 1024, "height": 1024}))
        with self.assertRaises(ContractValidationError):
            analysis_tile_core(2**32 - 1, 1, {"x": 0, "y": 0, "width": 1024, "height": 1})

    def test_saved_sam_preprocessing_tensor_parity(self):
        prepared, resized = sam_prepare(Image.open(SYNTHETIC).convert("RGB"))
        saved = np.load(PARITY)["prepared_rgb_f32"]
        self.assertEqual(resized, (750, 1024))
        self.assertEqual(hashlib.sha256(prepared.tobytes()).hexdigest(), hashlib.sha256(saved.tobytes()).hexdigest())

    def test_sam_mask_is_tile_sized_and_result_bounded(self):
        request = self.request()
        result = self.worker.analyze(request, self.png)
        self.assertEqual(self.seen[0][1], ["embedding"])
        with Image.open(io.BytesIO(result["mask_png"])) as image:
            self.assertEqual(image.size, (16, 12))
            self.assertEqual(image.mode, "L")
        self.assertTrue(result["components"])
        validate_analysis_result(result, request)
        result["mask_png"] = result["mask_png"] + b"x" * 4_194_304
        with self.assertRaises(ContractValidationError):
            validate_analysis_result(result, request)

    def test_unknown_capability_and_graph_mismatch_precede_model_load(self):
        request = self.request()
        request["capability"] = "coo@1"
        with self.assertRaises(ContractValidationError):
            self.worker.analyze(request, self.png)
        self.assertFalse(self.seen)
        request = self.request()
        request["graph_sha256s"][0] = "0" * 64
        request["request_digest"] = analysis_request_digest(request)[1]
        with self.assertRaises(ContractValidationError):
            self.worker.analyze(request, self.png)
        self.assertFalse(self.seen)

    def test_rt_boxes_and_asgi_round_trip(self):
        app = CloudGateway("modal", analysis_worker=self.worker, trust_edge_auth=True).as_asgi_app()
        status, _, body = asgi_request(app, "GET", "/mc/analysis/v1/capabilities", {}, b"")
        self.assertEqual(status, 200)
        capabilities = json.loads(body)
        self.assertEqual({c["capability"] for c in capabilities["capabilities"]}, {ANALYSIS_SAM, ANALYSIS_RT})
        request = self.request(ANALYSIS_RT)
        envelope = {"metadata": request, "tile_png_b64": base64.b64encode(self.png).decode()}
        status, _, body = asgi_request(app, "POST", "/mc/analysis/v1/analyze",
                                       {"content-type": "application/json"}, json.dumps(envelope).encode())
        self.assertEqual(status, 200, body)
        result = json.loads(body)
        self.assertEqual(result["boxes"][0]["class"], 1)
        self.assertIsNone(result["mask_png_b64"])
        self.assertIsNone(validate_analysis_wire_result(result, request))
        self.assertEqual(result["request_digest"], request["request_digest"])
        status, _, model_info = asgi_request(app, "GET", "/mc/v1/model-info", {}, b"")
        golden = json.loads((ROOT / "deploy/cloud/fixtures/model_info_response.json").read_text())
        self.assertEqual((status, model_info), (200, json.dumps(golden).encode()))

    def test_each_request_is_bounded_whatever_page_it_names(self):
        """Plan membership trusts the page size a request names; the per-request
        limits do not depend on it: a bounded body, a bounded tile PNG whose
        digest and geometry must match a tile of at most 1024 square."""
        app = CloudGateway("modal", analysis_worker=self.worker, trust_edge_auth=True).as_asgi_app()
        request = self.request()
        request.update({"page_width": 100_000, "page_height": 100_000,
                        "tile_rect": {"x": 0, "y": 0, "width": 1024, "height": 1024},
                        "tile_core": analysis_tile_core(100_000, 100_000, {"x": 0, "y": 0, "width": 1024, "height": 1024})})
        request["request_digest"] = analysis_request_digest(request)[1]

        def post(metadata, png):
            envelope = {"metadata": metadata, "tile_png_b64": base64.b64encode(png).decode()}
            return asgi_request(app, "POST", "/mc/analysis/v1/analyze",
                                {"content-type": "application/json"}, json.dumps(envelope).encode())

        # The tile PNG must be the named tile's size.
        status, _, body = post(request, self.png)
        self.assertEqual((status, json.loads(body)["error_code"]), (400, "invalid_request"))
        # A PNG over the byte limit is refused before it is decoded.
        big = b"\x89PNG" + bytes(4_194_305)
        oversized = dict(request, tile_png_sha256=hashlib.sha256(big).hexdigest())
        oversized["request_digest"] = analysis_request_digest(oversized)[1]
        status, _, body = post(oversized, big)
        self.assertEqual((status, json.loads(body)["error_code"]), (400, "invalid_request"))
        # A body over the bound is refused unread.
        status, _, body = asgi_request(app, "POST", "/mc/analysis/v1/analyze",
                                       {"content-type": "application/json"}, b" " * 8_000_001)
        self.assertEqual((status, json.loads(body)["error_code"]), (413, "payload_too_large"))
        self.assertEqual(self.seen, [], "nothing reached a model")

    def test_sam_wire_uses_bounded_base64(self):
        app = CloudGateway("modal", analysis_worker=self.worker, trust_edge_auth=True).as_asgi_app()
        request = self.request()
        envelope = {"metadata": request, "tile_png_b64": base64.b64encode(self.png).decode()}
        status, _, body = asgi_request(app, "POST", "/mc/analysis/v1/analyze",
                                       {"content-type": "application/json"}, json.dumps(envelope).encode())
        self.assertEqual(status, 200, body)
        wire = json.loads(body)
        self.assertNotIn("mask_png", wire)
        self.assertIsInstance(wire["mask_png_b64"], str)
        self.assertIsNotNone(validate_analysis_wire_result(wire, request))
        wire["mask_png_b64"] = "A" * 5_592_408
        with self.assertRaises(ContractValidationError):
            validate_analysis_wire_result(wire, request)

    def test_worker_failure_is_bound_analysis_rejection(self):
        def failed_session(_path):
            raise RuntimeError("session failed")
        worker = AnalysisWorker(
            {ANALYSIS_SAM: self.paths[:2]}, {ANALYSIS_SAM: "c" * 40}, failed_session,
        )
        app = CloudGateway("modal", analysis_worker=worker, trust_edge_auth=True).as_asgi_app()
        request = self.request()
        envelope = {"metadata": request, "tile_png_b64": base64.b64encode(self.png).decode()}
        status, _, body = asgi_request(app, "POST", "/mc/analysis/v1/analyze",
                                       {"content-type": "application/json"}, json.dumps(envelope).encode())
        self.assertEqual(status, 500)
        rejection = json.loads(body)
        self.assertEqual(rejection["error_code"], "inference_failed")
        self.assertEqual(rejection["request_digest"], request["request_digest"])
        self.assertFalse(rejection["enqueued"])
        expected = json.loads((VECTORS.parent / "inference_failed_error.json").read_text())
        self.assertEqual(expected["request_digest"], json.loads(VECTORS.read_text())["valid"][0]["digest"])
        expected["request_digest"] = request["request_digest"]
        self.assertEqual(rejection, expected)

    def test_response_processing_failure_retains_parsed_digest(self):
        class MalformedWorker:
            def __init__(self, result):
                self.result = result

            def analyze(self, _metadata, _tile_png):
                return self.result

        request = self.request()
        envelope = {"metadata": request, "tile_png_b64": base64.b64encode(self.png).decode()}
        expected = json.loads((VECTORS.parent / "inference_failed_error.json").read_text())
        expected["request_digest"] = request["request_digest"]
        for result in ({"unexpected": "result"}, {"mask_png": object()}):
            gateway = CloudGateway("modal", analysis_worker=MalformedWorker(result), trust_edge_auth=True)
            status, _, body = gateway.handle_http_request(
                "POST", "/mc/analysis/v1/analyze", {"content-type": "application/json"}, json.dumps(envelope).encode(),
            )
            self.assertEqual(status, 500)
            self.assertEqual(json.loads(body), expected)

    def test_unknown_fields_and_digest_rejected(self):
        request = self.request()
        request["extra"] = 1
        with self.assertRaises(ContractValidationError):
            validate_analysis_request(request, self.png)
        request.pop("extra")
        request["request_digest"] = "0" * 64
        with self.assertRaises(ContractValidationError):
            validate_analysis_request(request, self.png)

    def test_beam_entry_point_mounts_analysis(self):
        source = (ROOT / "deploy/cloud/beam/app.py").read_text()
        self.assertIn("return mount_gateway_app(gateway_app.as_asgi_app())", source)

    def test_beam_stripped_mount_paths_route_and_enforce_auth(self):
        gateway = CloudGateway("beam", analysis_worker=self.worker, bearer_token="test-token")
        app = restore_gateway_path(gateway.as_asgi_app())

        for prefix, suffix in (("/mc/v1", "/model-info"), ("/mc/analysis/v1", "/capabilities")):
            async def mounted(scope, receive, send):
                child_scope = {**scope, "root_path": prefix, "path": suffix, "raw_path": suffix.encode()}
                await app(child_scope, receive, send)

            status, _, _ = asgi_request(mounted, "GET", prefix + suffix, {}, b"")
            self.assertEqual(status, 401)
            status, _, body = asgi_request(mounted, "GET", prefix + suffix,
                                           {"authorization": "Bearer test-token"}, b"")
            self.assertEqual(status, 200, body)
            if prefix == "/mc/analysis/v1":
                self.assertIn("capabilities", json.loads(body))
            else:
                self.assertIn("recipe_id", json.loads(body))

        request = self.request(ANALYSIS_RT)
        envelope = {"metadata": request, "tile_png_b64": base64.b64encode(self.png).decode()}

        async def mounted_analyze(scope, receive, send):
            child_scope = {**scope, "root_path": "/mc/analysis/v1", "path": "/analyze", "raw_path": b"/analyze"}
            await app(child_scope, receive, send)

        status, _, _ = asgi_request(mounted_analyze, "POST", "/mc/analysis/v1/analyze",
                                    {"content-type": "application/json"}, json.dumps(envelope).encode())
        self.assertEqual(status, 401)
        status, _, body = asgi_request(mounted_analyze, "POST", "/mc/analysis/v1/analyze",
                                       {"content-type": "application/json", "authorization": "Bearer test-token"},
                                       json.dumps(envelope).encode())
        self.assertEqual(status, 200, body)
        self.assertEqual(json.loads(body)["request_digest"], request["request_digest"])

        async def unstripped_mount(scope, receive, send):
            await app({**scope, "root_path": "/mc/analysis/v1"}, receive, send)

        status, _, body = asgi_request(unstripped_mount, "GET", "/mc/analysis/v1/capabilities",
                                       {"authorization": "Bearer test-token"}, b"")
        self.assertEqual(status, 200, body)

    def test_invalid_tile_rejection_retains_request_digest(self):
        request = self.request()
        envelope = {"metadata": request, "tile_png_b64": base64.b64encode(tile_png((8, 8))).decode()}
        status, _, body = CloudGateway("modal", analysis_worker=self.worker, trust_edge_auth=True).handle_http_request(
            "POST", "/mc/analysis/v1/analyze", {"content-type": "application/json"}, json.dumps(envelope).encode(),
        )
        self.assertEqual(status, 400)
        self.assertEqual(json.loads(body)["request_digest"], request["request_digest"])

    def test_invalid_metadata_error_does_not_echo_secret_field(self):
        request = self.request()
        request["secret-token-sentinel"] = "hidden"
        envelope = {"metadata": request, "tile_png_b64": base64.b64encode(self.png).decode()}
        status, _, body = CloudGateway("modal", analysis_worker=self.worker, trust_edge_auth=True).handle_http_request(
            "POST", "/mc/analysis/v1/analyze", {"content-type": "application/json"}, json.dumps(envelope).encode(),
        )
        self.assertEqual(status, 400)
        self.assertNotIn("secret-token-sentinel", body.decode())

    def test_unauthorized_analysis_uses_analysis_error_shape(self):
        status, _, body = CloudGateway("modal", analysis_worker=self.worker).handle_http_request(
            "POST", "/mc/analysis/v1/analyze", {"content-type": "application/json"}, b"{}",
        )
        self.assertEqual(status, 401)
        expected = json.loads((VECTORS.parent / "unauthorized_error.json").read_text())
        self.assertEqual(json.loads(body), expected)

    def test_missing_graph_capabilities_are_unavailable_without_path(self):
        missing = str(Path(self.tmp.name) / "missing-secret-graph.onnx")
        worker = AnalysisWorker({ANALYSIS_SAM: [missing, self.paths[1]]},
                                {ANALYSIS_SAM: "c" * 40}, lambda path: FakeSession(path, self.seen))
        status, _, body = CloudGateway("modal", analysis_worker=worker, trust_edge_auth=True).handle_http_request(
            "GET", "/mc/analysis/v1/capabilities", {}, b"",
        )
        self.assertEqual(status, 503)
        self.assertEqual(json.loads(body)["error_code"], "capability_unavailable")
        self.assertNotIn(missing, body.decode())

    def test_unconfigured_analysis_uses_shared_rejection(self):
        status, _, body = CloudGateway("modal", trust_edge_auth=True).handle_http_request(
            "GET", "/mc/analysis/v1/capabilities", {}, b"",
        )
        self.assertEqual(status, 503)
        expected = json.loads((VECTORS.parent / "capability_unavailable_error.json").read_text())
        self.assertEqual(json.loads(body), expected)

    def test_analysis_content_type_is_invalid_request(self):
        status, _, body = CloudGateway("modal", analysis_worker=self.worker, trust_edge_auth=True).handle_http_request(
            "POST", "/mc/analysis/v1/analyze", {"content-type": "text/plain"}, b"{}",
        )
        self.assertEqual(status, 400)
        self.assertEqual(json.loads(body)["error_code"], "invalid_request")


if __name__ == "__main__":
    unittest.main()
