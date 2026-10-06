"""Unit tests for the provider-neutral CPU Control Gateway (/mc/v1)."""

import asyncio
import json
import threading
import unittest
from io import BytesIO
from pathlib import Path
from typing import Dict, Tuple

from deploy.cloud.common.contract import (
    PROTOCOL_VERSION,
    CloudProvider,
    HealthResponse,
    JobAcceptedResponse,
    JobCancelResponse,
    JobExecutionStatus,
    JobRequestMetadata,
    JobStatusResponse,
    ModelInfoResponse,
    PreEnqueueRejectionResponse,
    ResultMetadata,
    WarmupResponse,
    provisional_fixture_limits,
    validate_cancel_response,
    validate_health_response,
    validate_job_accepted_response,
    validate_job_request_metadata,
    validate_model_info_response,
    validate_response_binding,
    validate_result_bytes,
    validate_png_header,
    validate_worker_result,
    compute_request_digest,
)
from deploy.cloud.common.api import CloudGateway, parse_multipart_payload
from deploy.cloud.common.flux import FluxWorker
from deploy.cloud.common.handle_mapping import InMemoryHandleRegistry
from deploy.cloud.common.manifest import get_default_model_info
from deploy.cloud.common.redact import REDACTED_SECRET, redact_text

FIXTURES_DIR = Path(__file__).resolve().parent.parent / "fixtures"

# Upper bound for every thread handoff below. A lock-ordering regression then fails
# the test in seconds instead of hanging the whole suite.
THREAD_TIMEOUT_SECONDS = 10.0


def run_concurrently(test: unittest.TestCase, *targets) -> None:
    """Run each target on its own daemon thread and fail if any does not finish in time."""
    threads = [threading.Thread(target=target, daemon=True) for target in targets]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(THREAD_TIMEOUT_SECONDS)
    test.assertFalse(any(t.is_alive() for t in threads), "a concurrent request did not finish")


def build_multipart_body(
    metadata_json: str,
    image_bytes: bytes,
    hint_bytes: bytes,
    boundary: str = "----MangaCleanerBoundary12345",
) -> Tuple[str, bytes]:
    """Helper to construct standard multipart/form-data payload."""
    ct_header = f"multipart/form-data; boundary={boundary}"
    buf = BytesIO()

    # Part 1: metadata
    buf.write(f"--{boundary}\r\n".encode("utf-8"))
    buf.write(b'Content-Disposition: form-data; name="metadata"\r\n')
    buf.write(b"Content-Type: application/json\r\n\r\n")
    buf.write(metadata_json.encode("utf-8"))
    buf.write(b"\r\n")

    # Part 2: image
    buf.write(f"--{boundary}\r\n".encode("utf-8"))
    buf.write(b'Content-Disposition: form-data; name="image"; filename="image.png"\r\n')
    buf.write(b"Content-Type: image/png\r\n\r\n")
    buf.write(image_bytes)
    buf.write(b"\r\n")

    # Part 3: hint
    buf.write(f"--{boundary}\r\n".encode("utf-8"))
    buf.write(b'Content-Disposition: form-data; name="hint"; filename="hint.png"\r\n')
    buf.write(b"Content-Type: image/png\r\n\r\n")
    buf.write(hint_bytes)
    buf.write(b"\r\n")

    # Closing boundary
    buf.write(f"--{boundary}--\r\n".encode("utf-8"))

    return ct_header, buf.getvalue()


class TestCloudGateway(unittest.TestCase):
    def setUp(self):
        with open(FIXTURES_DIR / "tiny_image.png", "rb") as f:
            self.tiny_image_bytes = f.read()
        with open(FIXTURES_DIR / "tiny_hint.png", "rb") as f:
            self.tiny_hint_bytes = f.read()
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            self.job_meta_dict = json.load(f)
            self.job_meta_json = json.dumps(self.job_meta_dict)
            self.job_meta = JobRequestMetadata.from_dict(self.job_meta_dict)

        self.limits = provisional_fixture_limits()
        self.worker = FluxWorker(provider="modal", limits=self.limits)
        self.registry = InMemoryHandleRegistry()

        self.modal_gateway = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
            handle_registry=self.registry,
            worker=self.worker,
            proxy_token_id="modal-key-123",
            proxy_token_secret="modal-secret-456",
        )

        self.beam_gateway = CloudGateway(
            provider=CloudProvider.BEAM,
            limits=self.limits,
            handle_registry=InMemoryHandleRegistry(),
            worker=FluxWorker(provider="beam", limits=self.limits),
            bearer_token="beam-token-789",
        )

        self.modal_auth_headers = {
            "Modal-Key": "modal-key-123",
            "Modal-Secret": "modal-secret-456",
        }
        self.beam_auth_headers = {
            "Authorization": "Bearer beam-token-789",
        }

    def test_health_authenticated_modal_and_beam(self):
        # Modal health
        code, headers, body = self.modal_gateway.handle_http_request(
            method="GET",
            raw_path="/mc/v1/health",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        self.assertEqual(headers["Content-Type"], "application/json")
        resp = HealthResponse.from_dict(json.loads(body.decode("utf-8")))
        validate_health_response(resp)
        self.assertEqual(resp.provider, "modal")
        self.assertEqual(resp.status, "ok")
        # Proves zero GPU/worker inference during health check
        self.assertEqual(self.worker.inference_count, 0)
        self.assertFalse(self.worker.is_warm)

        # Beam health
        code, headers, body = self.beam_gateway.handle_http_request(
            method="GET",
            raw_path="/health",  # test root-normalized path
            headers=self.beam_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        resp = HealthResponse.from_dict(json.loads(body.decode("utf-8")))
        self.assertEqual(resp.provider, "beam")

    def test_model_info_authenticated(self):
        code, headers, body = self.modal_gateway.handle_http_request(
            method="GET",
            raw_path="/mc/v1/model-info",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        resp = ModelInfoResponse.from_dict(json.loads(body.decode("utf-8")))
        validate_model_info_response(resp)
        self.assertEqual(resp.protocol_version, PROTOCOL_VERSION)
        self.assertEqual(resp.provider, "modal")
        self.assertEqual(resp.recipe_id, "test-sdnq-v1")
        self.assertEqual(resp.model_id, "test-flux-schnell")
        self.assertFalse(resp.native_mask_conditioning)

    def test_submit_valid_crop_and_result_flow(self):
        ct_header, multipart_body = build_multipart_body(
            self.job_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        # 1. POST /jobs - synchronous completion in offline mode reports COMPLETED
        code, headers, body = self.modal_gateway.handle_http_request(
            method="POST",
            raw_path="/mc/v1/jobs",
            headers=req_headers,
            body=multipart_body,
        )
        self.assertEqual(code, 202)
        accepted = JobAcceptedResponse.from_dict(json.loads(body.decode("utf-8")))
        validate_job_accepted_response(accepted)
        self.assertEqual(accepted.job_id, self.job_meta.job_id)
        self.assertEqual(accepted.attempt_id, self.job_meta.attempt_id)
        self.assertEqual(accepted.request_digest, self.job_meta.request_digest)
        self.assertEqual(accepted.status, JobExecutionStatus.PENDING.value)
        handle = accepted.handle
        self.assertTrue(handle.startswith("handle-modal-"))

        # 2. GET /jobs/{handle}
        code, headers, body = self.modal_gateway.handle_http_request(
            method="GET",
            raw_path=f"/mc/v1/jobs/{handle}",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        status_resp = JobStatusResponse.from_dict(json.loads(body.decode("utf-8")))
        validate_response_binding(self.job_meta, status_resp, expected_handle=handle)
        self.assertEqual(status_resp.status, JobExecutionStatus.COMPLETED.value)
        self.assertIsNotNone(status_resp.reported_cost_usd)
        self.assertIsNotNone(status_resp.result_digest)
        self.assertIsNotNone(status_resp.result_bytes)

        # 3. GET /jobs/{handle}/result
        code, headers, body = self.modal_gateway.handle_http_request(
            method="GET",
            raw_path=f"/mc/v1/jobs/{handle}/result",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        self.assertEqual(headers["Content-Type"], "image/png")
        self.assertEqual(headers["X-MC-Result-Digest"], status_resp.result_digest)
        self.assertEqual(headers["X-MC-Reported-Cost-Usd"], str(status_resp.reported_cost_usd))

        result_meta = ResultMetadata(
            handle=handle,
            job_id=self.job_meta.job_id,
            attempt_id=self.job_meta.attempt_id,
            request_digest=self.job_meta.request_digest,
            recipe_id=self.job_meta.recipe.recipe_id,
            preprocessing_version=self.job_meta.recipe.preprocessing_version,
            model_id=self.job_meta.recipe.model_id,
            model_revision=self.job_meta.recipe.model_revision,
            native_mask_conditioning=False,
            result_digest=status_resp.result_digest,
            reported_cost_usd=status_resp.reported_cost_usd,
            width=self.job_meta.width,
            height=self.job_meta.height,
            byte_length=len(body),
        )
        validate_result_bytes(body, result_meta, self.job_meta, self.limits, expected_handle=handle)

    def test_pre_enqueue_rejection_forbidden_coordinate_fields(self):
        bad_meta_dict = dict(self.job_meta_dict)
        bad_meta_dict["page_x"] = 100
        bad_meta_json = json.dumps(bad_meta_dict)

        ct_header, multipart_body = build_multipart_body(
            bad_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        code, headers, body = self.modal_gateway.handle_http_request(
            method="POST",
            raw_path="/mc/v1/jobs",
            headers=req_headers,
            body=multipart_body,
        )
        self.assertEqual(code, 400)
        rej_dict = json.loads(body.decode("utf-8"))
        rej = PreEnqueueRejectionResponse.from_dict(rej_dict)
        self.assertFalse(rej.enqueued)
        self.assertEqual(rej.error_code, "forbidden_fields")
        self.assertFalse(rej.retryable)

    def test_pre_enqueue_rejection_unaligned_stride(self):
        bad_meta_dict = dict(self.job_meta_dict)
        bad_meta_dict["width"] = 15  # not divisible by 16
        bad_meta_json = json.dumps(bad_meta_dict)

        ct_header, multipart_body = build_multipart_body(
            bad_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        code, headers, body = self.modal_gateway.handle_http_request(
            method="POST",
            raw_path="/mc/v1/jobs",
            headers=req_headers,
            body=multipart_body,
        )
        self.assertEqual(code, 400)
        rej = PreEnqueueRejectionResponse.from_dict(json.loads(body.decode("utf-8")))
        self.assertFalse(rej.enqueued)
        self.assertEqual(rej.error_code, "invalid_dimensions")
        self.assertFalse(rej.retryable)

    def test_pre_enqueue_rejection_unsupported_recipe(self):
        bad_meta_dict = dict(self.job_meta_dict)
        bad_recipe = dict(bad_meta_dict["recipe"])
        bad_recipe["recipe_id"] = "unknown-recipe-xyz"
        bad_meta_dict["recipe"] = bad_recipe
        # Recompute digest so digest check passes, but recipe compatibility check fails
        from deploy.cloud.common.contract import compute_request_digest
        temp_meta = JobRequestMetadata.from_dict(bad_meta_dict)
        bad_meta_dict["request_digest"] = compute_request_digest(temp_meta)
        bad_meta_json = json.dumps(bad_meta_dict)

        ct_header, multipart_body = build_multipart_body(
            bad_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        code, headers, body = self.modal_gateway.handle_http_request(
            method="POST",
            raw_path="/mc/v1/jobs",
            headers=req_headers,
            body=multipart_body,
        )
        self.assertEqual(code, 400)
        rej = PreEnqueueRejectionResponse.from_dict(json.loads(body.decode("utf-8")))
        self.assertFalse(rej.enqueued)
        self.assertEqual(rej.error_code, "unsupported_recipe")
        self.assertFalse(rej.retryable)

    def test_pre_enqueue_rejection_invalid_digest(self):
        bad_meta_dict = dict(self.job_meta_dict)
        bad_meta_dict["request_digest"] = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        bad_meta_json = json.dumps(bad_meta_dict)

        ct_header, multipart_body = build_multipart_body(
            bad_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        code, headers, body = self.modal_gateway.handle_http_request(
            method="POST",
            raw_path="/mc/v1/jobs",
            headers=req_headers,
            body=multipart_body,
        )
        self.assertEqual(code, 400)
        rej = PreEnqueueRejectionResponse.from_dict(json.loads(body.decode("utf-8")))
        self.assertFalse(rej.enqueued)
        self.assertEqual(rej.error_code, "invalid_digest")
        self.assertFalse(rej.retryable)

    def test_job_cancellation_lifecycle(self):
        # Register job without auto-executing
        manual_gateway = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
            handle_registry=InMemoryHandleRegistry(),
            worker=self.worker,
            proxy_token_id="modal-key-123",
            proxy_token_secret="modal-secret-456",
            auto_execute=False,
        )
        ct_header, multipart_body = build_multipart_body(
            self.job_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        code, _, body = manual_gateway.handle_http_request(
            method="POST",
            raw_path="/mc/v1/jobs",
            headers=req_headers,
            body=multipart_body,
        )
        self.assertEqual(code, 202)
        accepted = JobAcceptedResponse.from_dict(json.loads(body.decode("utf-8")))
        handle = accepted.handle
        self.assertEqual(accepted.status, JobExecutionStatus.PENDING.value)

        # POST /jobs/{handle}/cancel -> returns nonterminal cancel_requested
        code, _, body = manual_gateway.handle_http_request(
            method="POST",
            raw_path=f"/mc/v1/jobs/{handle}/cancel",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        cancel_resp = JobCancelResponse.from_dict(json.loads(body.decode("utf-8")))
        validate_cancel_response(self.job_meta, cancel_resp, expected_handle=handle)
        self.assertEqual(cancel_resp.status, "cancel_requested")
        self.assertTrue(cancel_resp.acknowledged)

        # GET /jobs/{handle} -> cancel requested, remains non-terminal (PENDING) until executed/finalized
        code, _, body = manual_gateway.handle_http_request(
            method="GET",
            raw_path=f"/mc/v1/jobs/{handle}",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        status_resp = JobStatusResponse.from_dict(json.loads(body.decode("utf-8")))
        self.assertEqual(status_resp.status, JobExecutionStatus.PENDING.value)
        record = manual_gateway.handle_registry.get_by_app_handle(handle)
        self.assertIsNotNone(record)
        self.assertTrue(record.cancel_requested)

        # When worker halts and marks job cancelled in registry, it transitions to terminal CANCELLED
        manual_gateway.handle_registry.update_status(handle, JobExecutionStatus.CANCELLED)
        code, _, body = manual_gateway.handle_http_request(
            method="GET",
            raw_path=f"/mc/v1/jobs/{handle}",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        status_resp2 = JobStatusResponse.from_dict(json.loads(body.decode("utf-8")))
        self.assertEqual(status_resp2.status, JobExecutionStatus.CANCELLED.value)

        # POST /jobs/{handle}/cancel on already-terminal job returns typed 409 Conflict without mutating
        code, _, body = manual_gateway.handle_http_request(
            method="POST",
            raw_path=f"/mc/v1/jobs/{handle}/cancel",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 409)
        err = json.loads(body.decode("utf-8"))
        self.assertEqual(err["error_code"], "job_terminal")
        self.assertEqual(record.status, JobExecutionStatus.CANCELLED)

    def test_warmup_endpoint(self):
        self.assertFalse(self.worker.is_warm)
        code, headers, body = self.modal_gateway.handle_http_request(
            method="POST",
            raw_path="/mc/v1/warmup",
            headers=self.modal_auth_headers,
            body=b"",
        )
        self.assertEqual(code, 200)
        warmup_resp = WarmupResponse.from_dict(json.loads(body.decode("utf-8")))
        self.assertEqual(warmup_resp.status, "ok")
        self.assertEqual(warmup_resp.provider, "modal")
        self.assertEqual(warmup_resp.worker_state, "warm")
        self.assertTrue(self.worker.is_warm)

    def test_unauthenticated_requests_rejected(self):
        # Missing Modal auth
        code, _, body = self.modal_gateway.handle_http_request(
            method="GET",
            raw_path="/mc/v1/health",
            headers={},
            body=b"",
        )
        self.assertEqual(code, 401)
        err = json.loads(body.decode("utf-8"))
        self.assertEqual(err["error_code"], "unauthorized")

        # Wrong Beam token
        code, _, body = self.beam_gateway.handle_http_request(
            method="GET",
            raw_path="/mc/v1/health",
            headers={"Authorization": "Bearer wrong-token"},
            body=b"",
        )
        self.assertEqual(code, 401)
        err = json.loads(body.decode("utf-8"))
        self.assertEqual(err["error_code"], "unauthorized")

    def test_auth_fails_closed_when_credentials_absent(self):
        """Verify CloudGateway fails closed (401) when provider credentials/validator are absent."""
        unconfigured_gateway = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
        )
        code, _, body = unconfigured_gateway.handle_http_request(
            method="GET",
            raw_path="/mc/v1/health",
            headers={},
            body=b"",
        )
        self.assertEqual(code, 401)
        err = json.loads(body.decode("utf-8"))
        self.assertEqual(err["error_code"], "unauthorized")

        # Explicit test validator allows test suite to opt-in cleanly
        test_gw = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
            auth_validator=lambda h: True,
        )
        code, _, _ = test_gw.handle_http_request(
            method="GET",
            raw_path="/mc/v1/health",
            headers={},
            body=b"",
        )
        self.assertEqual(code, 200)

    def test_external_routes_reject_native_handles(self):
        """External status/result/cancel routes must accept only opaque app handles, never native handles."""
        native_h = "native-modal-internal-12345"
        app_h = "handle-modal-9999888877776666"
        self.registry.register_job(
            job_meta=self.job_meta,
            native_handle=native_h,
            app_handle=app_h,
            provider="modal",
        )

        # Lookup by public app handle succeeds
        code, _, _ = self.modal_gateway.handle_http_request(
            "GET", f"/mc/v1/jobs/{app_h}", self.modal_auth_headers, b""
        )
        self.assertEqual(code, 200)

        # Lookup by native handle MUST return 404 Not Found on external routes
        code, _, body = self.modal_gateway.handle_http_request(
            "GET", f"/mc/v1/jobs/{native_h}", self.modal_auth_headers, b""
        )
        self.assertEqual(code, 404)

        code, _, _ = self.modal_gateway.handle_http_request(
            "GET", f"/mc/v1/jobs/{native_h}/result", self.modal_auth_headers, b""
        )
        self.assertEqual(code, 404)

        code, _, _ = self.modal_gateway.handle_http_request(
            "POST", f"/mc/v1/jobs/{native_h}/cancel", self.modal_auth_headers, b""
        )
        self.assertEqual(code, 404)

    def test_multipart_rejects_unknown_and_duplicate_fields(self):
        """Reject duplicate and unknown multipart fields; enforce exactly metadata,image,hint."""
        boundary = "----TestBoundary999"

        # 1. Unknown field
        buf = BytesIO()
        buf.write(f"--{boundary}\r\n".encode("utf-8"))
        buf.write(b'Content-Disposition: form-data; name="metadata"\r\nContent-Type: application/json\r\n\r\n')
        buf.write(self.job_meta_json.encode("utf-8"))
        buf.write(b"\r\n")
        buf.write(f"--{boundary}\r\n".encode("utf-8"))
        buf.write(b'Content-Disposition: form-data; name="image"\r\nContent-Type: image/png\r\n\r\n')
        buf.write(self.tiny_image_bytes)
        buf.write(b"\r\n")
        buf.write(f"--{boundary}\r\n".encode("utf-8"))
        buf.write(b'Content-Disposition: form-data; name="hint"\r\nContent-Type: image/png\r\n\r\n')
        buf.write(self.tiny_hint_bytes)
        buf.write(b"\r\n")
        buf.write(f"--{boundary}\r\n".encode("utf-8"))
        buf.write(b'Content-Disposition: form-data; name="extra_field"\r\nContent-Type: text/plain\r\n\r\n')
        buf.write(b"malicious_extra")
        buf.write(b"\r\n")
        buf.write(f"--{boundary}--\r\n".encode("utf-8"))

        headers = dict(self.modal_auth_headers)
        headers["Content-Type"] = f"multipart/form-data; boundary={boundary}"
        code, _, body = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", headers, buf.getvalue())
        self.assertEqual(code, 400)
        rej = json.loads(body.decode("utf-8"))
        self.assertEqual(rej["error_code"], "forbidden_fields")

        # 2. Duplicate field
        buf2 = BytesIO()
        buf2.write(f"--{boundary}\r\n".encode("utf-8"))
        buf2.write(b'Content-Disposition: form-data; name="metadata"\r\nContent-Type: application/json\r\n\r\n')
        buf2.write(self.job_meta_json.encode("utf-8"))
        buf2.write(b"\r\n")
        buf2.write(f"--{boundary}\r\n".encode("utf-8"))
        buf2.write(b'Content-Disposition: form-data; name="metadata"\r\nContent-Type: application/json\r\n\r\n')
        buf2.write(self.job_meta_json.encode("utf-8"))
        buf2.write(b"\r\n")
        buf2.write(f"--{boundary}\r\n".encode("utf-8"))
        buf2.write(b'Content-Disposition: form-data; name="image"\r\nContent-Type: image/png\r\n\r\n')
        buf2.write(self.tiny_image_bytes)
        buf2.write(b"\r\n")
        buf2.write(f"--{boundary}\r\n".encode("utf-8"))
        buf2.write(b'Content-Disposition: form-data; name="hint"\r\nContent-Type: image/png\r\n\r\n')
        buf2.write(self.tiny_hint_bytes)
        buf2.write(b"\r\n")
        buf2.write(f"--{boundary}--\r\n".encode("utf-8"))

        code2, _, body2 = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", headers, buf2.getvalue())
        self.assertEqual(code2, 400)
        rej2 = json.loads(body2.decode("utf-8"))
        self.assertEqual(rej2["error_code"], "forbidden_fields")

    def test_preserve_unknown_reported_cost(self):
        """Preserve unknown reported cost: omit the result cost header or encode explicit null, never 0.0."""
        nocost_worker = FluxWorker(provider="modal", limits=self.limits, reported_cost_per_job=None)
        nocost_gateway = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
            handle_registry=InMemoryHandleRegistry(),
            worker=nocost_worker,
            proxy_token_id="modal-key-123",
            proxy_token_secret="modal-secret-456",
            auto_execute=True,
        )

        ct_header, multipart_body = build_multipart_body(
            self.job_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        # 1. Submit
        code, _, body = nocost_gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, multipart_body)
        self.assertEqual(code, 202)
        accepted = JobAcceptedResponse.from_dict(json.loads(body.decode("utf-8")))
        handle = accepted.handle

        # 2. Status polling: reported_cost_usd is None (serializes as JSON null)
        code, _, body = nocost_gateway.handle_http_request("GET", f"/mc/v1/jobs/{handle}", req_headers, b"")
        self.assertEqual(code, 200)
        status_dict = json.loads(body.decode("utf-8"))
        self.assertIsNone(status_dict["reported_cost_usd"])

        # 3. Result download: X-MC-Reported-Cost-Usd header is omitted entirely
        code, headers, _ = nocost_gateway.handle_http_request("GET", f"/mc/v1/jobs/{handle}/result", req_headers, b"")
        self.assertEqual(code, 200)
        self.assertNotIn("X-MC-Reported-Cost-Usd", headers)

    def test_wsgi_adapter_callable(self):
        wsgi_app = self.modal_gateway.as_wsgi_app()
        environ = {
            "REQUEST_METHOD": "GET",
            "PATH_INFO": "/mc/v1/health",
            "HTTP_MODAL_KEY": "modal-key-123",
            "HTTP_MODAL_SECRET": "modal-secret-456",
        }
        captured_status = []
        captured_headers = []

        def start_response(status, headers):
            captured_status.append(status)
            captured_headers.append(headers)

        body_iter = wsgi_app(environ, start_response)
        self.assertEqual(captured_status[0], "200 OK")
        raw_body = b"".join(body_iter)
        resp = HealthResponse.from_dict(json.loads(raw_body.decode("utf-8")))
        self.assertEqual(resp.status, "ok")

    def test_wsgi_max_multipart_bytes_rejected_with_413(self):
        """WSGI must enforce max_multipart_bytes using CONTENT_LENGTH and bounded reads returning 413."""
        wsgi_app = self.modal_gateway.as_wsgi_app()
        environ = {
            "REQUEST_METHOD": "POST",
            "PATH_INFO": "/mc/v1/jobs",
            "HTTP_MODAL_KEY": "modal-key-123",
            "HTTP_MODAL_SECRET": "modal-secret-456",
            "CONTENT_LENGTH": str(self.limits.max_multipart_bytes + 100),
            "wsgi.input": BytesIO(b"X" * 100),
        }
        captured_status = []
        captured_headers = []

        def start_response(status, headers):
            captured_status.append(status)
            captured_headers.append(headers)

        body_iter = wsgi_app(environ, start_response)
        self.assertEqual(captured_status[0], "413 Payload Too Large")
        raw_body = b"".join(body_iter)
        err = json.loads(raw_body.decode("utf-8"))
        self.assertEqual(err["error_code"], "payload_too_large")

    def test_asgi_adapter_callable(self):
        asgi_app = self.modal_gateway.as_asgi_app()
        scope = {
            "type": "http",
            "method": "GET",
            "path": "/mc/v1/health",
            "headers": [
                (b"modal-key", b"modal-key-123"),
                (b"modal-secret", b"modal-secret-456"),
            ],
        }
        messages = []

        async def run_asgi():
            async def receive():
                return {"type": "http.request", "body": b"", "more_body": False}

            async def send(msg):
                messages.append(msg)

            await asgi_app(scope, receive, send)

        asyncio.run(run_asgi())
        self.assertEqual(messages[0]["type"], "http.response.start")
        self.assertEqual(messages[0]["status"], 200)
        self.assertEqual(messages[1]["type"], "http.response.body")
        resp = HealthResponse.from_dict(json.loads(messages[1]["body"].decode("utf-8")))
        self.assertEqual(resp.status, "ok")

    def test_asgi_incremental_max_multipart_bytes_rejected_with_413(self):
        """ASGI must incrementally enforce max_multipart_bytes and return 413 without accumulating excess."""
        asgi_app = self.modal_gateway.as_asgi_app()
        scope = {
            "type": "http",
            "method": "POST",
            "path": "/mc/v1/jobs",
            "headers": [
                (b"modal-key", b"modal-key-123"),
                (b"modal-secret", b"modal-secret-456"),
            ],
        }
        messages = []
        oversized_chunk = b"X" * (self.limits.max_multipart_bytes + 1024)

        async def run_asgi():
            async def receive():
                return {"type": "http.request", "body": oversized_chunk, "more_body": True}

            async def send(msg):
                messages.append(msg)

            await asgi_app(scope, receive, send)

        asyncio.run(run_asgi())
        self.assertEqual(messages[0]["type"], "http.response.start")
        self.assertEqual(messages[0]["status"], 413)
        self.assertEqual(messages[1]["type"], "http.response.body")
        err = json.loads(messages[1]["body"].decode("utf-8"))
        self.assertEqual(err["error_code"], "payload_too_large")

    def test_gateway_worker_corrupted_output_fails_closed(self):
        """Worker returning corrupted output must become typed failed job and never retrievable as result."""
        corrupt_worker = FluxWorker(provider="modal", limits=self.limits)
        corrupt_worker.simulated_fault = "corrupted_output"
        gw = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
            handle_registry=InMemoryHandleRegistry(),
            worker=corrupt_worker,
            proxy_token_id="modal-key-123",
            proxy_token_secret="modal-secret-456",
            auto_execute=True,
        )

        job_dict = dict(self.job_meta_dict)
        job_dict["job_id"] = "job-gw-corrupt-001"
        job_dict["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(job_dict))
        ct_header, body = build_multipart_body(
            json.dumps(job_dict), self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = {"Modal-Key": "modal-key-123", "Modal-Secret": "modal-secret-456", "Content-Type": ct_header}

        # 1. Submit returns 202 with status pending
        code, _, resp_body = gw.handle_http_request("POST", "/mc/v1/jobs", req_headers, body)
        self.assertEqual(code, 202)
        accepted = JobAcceptedResponse.from_dict(json.loads(resp_body.decode("utf-8")))
        self.assertEqual(accepted.status, JobExecutionStatus.PENDING.value)
        handle = accepted.handle

        # 2. Status polling reports typed failed status with execution_failed
        code, _, resp_body = gw.handle_http_request("GET", f"/mc/v1/jobs/{handle}", self.modal_auth_headers, b"")
        self.assertEqual(code, 200)
        status_resp = JobStatusResponse.from_dict(json.loads(resp_body.decode("utf-8")))
        self.assertEqual(status_resp.status, JobExecutionStatus.FAILED.value)
        self.assertIsNotNone(status_resp.error)
        self.assertEqual(status_resp.error.error_code, "execution_failed")
        self.assertIsNone(status_resp.result_digest)
        self.assertIsNone(status_resp.result_bytes)

        # 3. Result download MUST return 409 Conflict and never expose corrupted output
        code, _, _ = gw.handle_http_request("GET", f"/mc/v1/jobs/{handle}/result", self.modal_auth_headers, b"")
        self.assertEqual(code, 409)

    def test_gateway_worker_wrong_dimensions_fails_closed(self):
        """Worker returning wrong dimensions must become typed failed job and never retrievable as result."""
        wrong_dim_worker = FluxWorker(provider="beam", limits=self.limits)
        wrong_dim_worker.simulated_fault = "wrong_dimensions"
        gw = CloudGateway(
            provider=CloudProvider.BEAM,
            limits=self.limits,
            handle_registry=InMemoryHandleRegistry(),
            worker=wrong_dim_worker,
            bearer_token="beam-token-789",
            auto_execute=True,
        )

        job_dict = dict(self.job_meta_dict)
        job_dict["job_id"] = "job-gw-wrong-dim-001"
        job_dict["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(job_dict))
        ct_header, body = build_multipart_body(
            json.dumps(job_dict), self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = {"Authorization": "Bearer beam-token-789", "Content-Type": ct_header}

        # 1. Submit returns 202 with status pending
        code, _, resp_body = gw.handle_http_request("POST", "/mc/v1/jobs", req_headers, body)
        self.assertEqual(code, 202)
        accepted = JobAcceptedResponse.from_dict(json.loads(resp_body.decode("utf-8")))
        self.assertEqual(accepted.status, JobExecutionStatus.PENDING.value)
        handle = accepted.handle

        # 2. Status polling reports typed failed status with execution_failed and dimension error details
        code, _, resp_body = gw.handle_http_request("GET", f"/mc/v1/jobs/{handle}", self.beam_auth_headers, b"")
        self.assertEqual(code, 200)
        status_resp = JobStatusResponse.from_dict(json.loads(resp_body.decode("utf-8")))
        self.assertEqual(status_resp.status, JobExecutionStatus.FAILED.value)
        self.assertIsNotNone(status_resp.error)
        self.assertEqual(status_resp.error.error_code, "execution_failed")
        self.assertIn("dimensions", status_resp.error.message.lower())
        self.assertIsNone(status_resp.result_digest)
        self.assertIsNone(status_resp.result_bytes)

        # 3. Result download MUST return 409 Conflict
        code, _, _ = gw.handle_http_request("GET", f"/mc/v1/jobs/{handle}/result", self.beam_auth_headers, b"")
        self.assertEqual(code, 409)

    def test_cancel_terminal_completed_job_returns_409_and_does_not_mutate(self):
        """Canceling a completed job returns typed 409 and does not mutate record state or timestamps."""
        ct_header, multipart_body = build_multipart_body(
            self.job_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        code, _, body = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, multipart_body)
        self.assertEqual(code, 202)
        accepted = JobAcceptedResponse.from_dict(json.loads(body.decode("utf-8")))
        handle = accepted.handle

        record = self.modal_gateway.handle_registry.get_by_app_handle(handle)
        self.assertIsNotNone(record)
        self.assertEqual(record.status, JobExecutionStatus.COMPLETED)
        initial_updated_at = record.updated_at
        initial_cancel_requested = record.cancel_requested

        # POST /jobs/{handle}/cancel on completed job
        code, _, body = self.modal_gateway.handle_http_request(
            "POST", f"/mc/v1/jobs/{handle}/cancel", self.modal_auth_headers, b""
        )
        self.assertEqual(code, 409)
        err = json.loads(body.decode("utf-8"))
        self.assertEqual(err["error_code"], "job_terminal")
        self.assertIn("terminal state 'completed'", err["message"])

        # Invariant: Record was NOT mutated
        self.assertEqual(record.status, JobExecutionStatus.COMPLETED)
        self.assertEqual(record.cancel_requested, initial_cancel_requested)
        self.assertEqual(record.updated_at, initial_updated_at)

    def test_pre_enqueue_rejection_image_and_hint_hash_mismatch_classified_invalid_digest(self):
        """SHA-256 hash mismatch on image or hint buffers must be classified as invalid_digest."""
        # 1. Image SHA-256 mismatch
        bad_img_meta = dict(self.job_meta_dict)
        bad_img_meta["image_sha256"] = "0" * 64
        bad_img_meta["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(bad_img_meta))
        ct_header1, body1 = build_multipart_body(
            json.dumps(bad_img_meta), self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers1 = {"Modal-Key": "modal-key-123", "Modal-Secret": "modal-secret-456", "Content-Type": ct_header1}

        code1, _, resp1 = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers1, body1)
        self.assertEqual(code1, 400)
        rej1 = json.loads(resp1.decode("utf-8"))
        self.assertEqual(rej1["error_code"], "invalid_digest")
        self.assertIn("Image SHA-256 mismatch", rej1["message"])

        # 2. Hint SHA-256 mismatch
        bad_hint_meta = dict(self.job_meta_dict)
        bad_hint_meta["hint_sha256"] = "1" * 64
        bad_hint_meta["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(bad_hint_meta))
        ct_header2, body2 = build_multipart_body(
            json.dumps(bad_hint_meta), self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers2 = {"Modal-Key": "modal-key-123", "Modal-Secret": "modal-secret-456", "Content-Type": ct_header2}

        code2, _, resp2 = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers2, body2)
        self.assertEqual(code2, 400)
        rej2 = json.loads(resp2.decode("utf-8"))
        self.assertEqual(rej2["error_code"], "invalid_digest")
        self.assertIn("Hint SHA-256 mismatch", rej2["message"])

    def test_pre_enqueue_rejection_valid_header_corrupted_idat_classified_invalid_image(self):
        """PNG with valid header but corrupted/truncated IDAT must be rejected with invalid_image."""
        import struct
        import zlib
        ihdr_data = struct.pack(">IIBBBBB", 16, 16, 8, 2, 0, 0, 0)
        ihdr_crc = zlib.crc32(b"IHDR" + ihdr_data) & 0xFFFFFFFF
        ihdr_chunk = struct.pack(">I", 13) + b"IHDR" + ihdr_data + struct.pack(">I", ihdr_crc)
        valid_hdr = b"\x89PNG\r\n\x1a\n" + ihdr_chunk
        corrupt_idat = valid_hdr + struct.pack(">I", 50) + b"IDAT" + b"CORRUPTED_TRUNCATED_BYTES"

        import hashlib
        corrupt_sha = hashlib.sha256(corrupt_idat).hexdigest()

        corrupt_meta_dict = dict(self.job_meta_dict)
        corrupt_meta_dict["image_sha256"] = corrupt_sha
        corrupt_meta_dict["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(corrupt_meta_dict))

        ct_header, body = build_multipart_body(
            json.dumps(corrupt_meta_dict), corrupt_idat, self.tiny_hint_bytes
        )
        req_headers = {"Modal-Key": "modal-key-123", "Modal-Secret": "modal-secret-456", "Content-Type": ct_header}

        code, _, resp_body = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, body)
        self.assertEqual(code, 400)
        rej = json.loads(resp_body.decode("utf-8"))
        self.assertEqual(rej["error_code"], "invalid_image")

    def test_hmac_compare_digest_timing_safe_token_checks(self):
        """Verify hmac.compare_digest authentication across Modal and Beam gateways."""
        # Modal exact match succeeds
        self.assertTrue(self.modal_gateway.check_auth({"Modal-Key": "modal-key-123", "Modal-Secret": "modal-secret-456"}))
        # Modal key prefix/suffix tampering fails
        self.assertFalse(self.modal_gateway.check_auth({"Modal-Key": "modal-key-12", "Modal-Secret": "modal-secret-456"}))
        self.assertFalse(self.modal_gateway.check_auth({"Modal-Key": "modal-key-1234", "Modal-Secret": "modal-secret-456"}))
        self.assertFalse(self.modal_gateway.check_auth({"Modal-Key": "modal-key-123", "Modal-Secret": "modal-secret-45"}))

        # Beam exact match succeeds
        self.assertTrue(self.beam_gateway.check_auth({"Authorization": "Bearer beam-token-789"}))
        # Beam bearer token tampering fails
        self.assertFalse(self.beam_gateway.check_auth({"Authorization": "Bearer beam-token-78"}))
        self.assertFalse(self.beam_gateway.check_auth({"Authorization": "Bearer beam-token-7890"}))
        self.assertFalse(self.beam_gateway.check_auth({"Authorization": "Basic beam-token-789"}))

    def test_duplicate_submission_with_conflicting_digest_rejected_409(self):
        """Duplicate (job_id, attempt_id) with altered digest must be rejected with 409 invalid_digest."""
        ct_header, multipart_body = build_multipart_body(
            self.job_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        # First submission succeeds
        code1, _, body1 = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, multipart_body)
        self.assertEqual(code1, 202)

        # Mutate seed to create conflicting digest for the same (job_id, attempt_id)
        bad_meta_dict = dict(self.job_meta_dict)
        bad_meta_dict["seed"] = 9999
        bad_meta_dict["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(bad_meta_dict))
        bad_meta_json = json.dumps(bad_meta_dict)
        ct_header2, body2 = build_multipart_body(
            bad_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers2 = dict(self.modal_auth_headers)
        req_headers2["Content-Type"] = ct_header2

        code2, _, body2 = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers2, body2)
        self.assertEqual(code2, 409)
        rej = json.loads(body2.decode("utf-8"))
        self.assertEqual(rej["error_code"], "invalid_digest")
        self.assertFalse(rej["enqueued"])

    def test_asgi_lifespan_startup_and_shutdown(self):
        """ASGI lifespan startup and shutdown messages must be acknowledged cleanly."""
        asgi_app = self.modal_gateway.as_asgi_app()
        scope = {"type": "lifespan"}
        messages = []
        inbound = [
            {"type": "lifespan.startup"},
            {"type": "lifespan.shutdown"},
        ]

        async def run_lifespan():
            async def receive():
                return inbound.pop(0)

            async def send(msg):
                messages.append(msg)

            await asgi_app(scope, receive, send)

        asyncio.run(run_lifespan())
        self.assertEqual(len(messages), 2)
        self.assertEqual(messages[0]["type"], "lifespan.startup.complete")
        self.assertEqual(messages[1]["type"], "lifespan.shutdown.complete")

    def test_wsgi_streaming_chunked_over_limit_rejected_413(self):
        """WSGI must enforce max_multipart_bytes on chunked POST bodies without CONTENT_LENGTH."""
        wsgi_app = self.modal_gateway.as_wsgi_app()
        oversized_data = b"X" * (self.limits.max_multipart_bytes + 2048)
        environ = {
            "REQUEST_METHOD": "POST",
            "PATH_INFO": "/mc/v1/jobs",
            "HTTP_MODAL_KEY": "modal-key-123",
            "HTTP_MODAL_SECRET": "modal-secret-456",
            "wsgi.input": BytesIO(oversized_data),
        }
        captured_status = []
        captured_headers = []

        def start_response(status, headers):
            captured_status.append(status)
            captured_headers.append(headers)

        body_iter = wsgi_app(environ, start_response)
        self.assertEqual(captured_status[0], "413 Payload Too Large")
        raw_body = b"".join(body_iter)
        err = json.loads(raw_body.decode("utf-8"))
        self.assertEqual(err["error_code"], "payload_too_large")

    def test_concurrent_duplicate_conflicting_digest_race_enforces_409_invalid_digest(self):
        """Concurrent submissions for same (job_id, attempt_id) with conflicting digests must enforce 409 invalid_digest under lock."""
        # Prepare request 1
        meta1_dict = dict(self.job_meta_dict)
        meta1_dict["job_id"] = "job-race-001"
        meta1_dict["attempt_id"] = "att-race-001"
        meta1_dict["seed"] = 42
        meta1_dict["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(meta1_dict))
        ct_header1, body1 = build_multipart_body(
            json.dumps(meta1_dict), self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers1 = dict(self.modal_auth_headers)
        req_headers1["Content-Type"] = ct_header1

        # Prepare request 2 with conflicting digest (different seed)
        meta2_dict = dict(self.job_meta_dict)
        meta2_dict["job_id"] = "job-race-001"
        meta2_dict["attempt_id"] = "att-race-001"
        meta2_dict["seed"] = 9999
        meta2_dict["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(meta2_dict))
        ct_header2, body2 = build_multipart_body(
            json.dumps(meta2_dict), self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers2 = dict(self.modal_auth_headers)
        req_headers2["Content-Type"] = ct_header2

        barrier = threading.Barrier(2, timeout=THREAD_TIMEOUT_SECONDS)
        results = [None, None]

        def submit(idx, headers, body):
            barrier.wait()
            results[idx] = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", headers, body)

        run_concurrently(
            self,
            lambda: submit(0, req_headers1, body1),
            lambda: submit(1, req_headers2, body2),
        )

        status_codes = sorted([results[0][0], results[1][0]])
        self.assertEqual(status_codes, [202, 409])

        # Identify which succeeded and which was rejected
        success_res = results[0] if results[0][0] == 202 else results[1]
        conflict_res = results[0] if results[0][0] == 409 else results[1]

        # Verify accepted response
        accepted = JobAcceptedResponse.from_dict(json.loads(success_res[2].decode("utf-8")))
        validate_job_accepted_response(accepted)
        self.assertEqual(accepted.job_id, "job-race-001")
        self.assertEqual(accepted.attempt_id, "att-race-001")

        # Verify 409 PreEnqueueRejectionResponse
        rej = PreEnqueueRejectionResponse.from_dict(json.loads(conflict_res[2].decode("utf-8")))
        self.assertFalse(rej.enqueued)
        self.assertEqual(rej.error_code, "invalid_digest")
        self.assertFalse(rej.retryable)
        self.assertEqual(rej.job_id, "job-race-001")
        self.assertEqual(rej.attempt_id, "att-race-001")
        self.assertIn("already registered with a different request digest", rej.message)

        # Invariant: exactly one job record in registry for this attempt
        record = self.registry.get_by_job_attempt("job-race-001", "att-race-001")
        self.assertIsNotNone(record)
        self.assertEqual(record.request_digest, accepted.request_digest)

    def test_concurrent_exact_duplicate_digest_both_return_202(self):
        """Concurrent submissions for same (job_id, attempt_id) with identical digest both return 202 and run worker once."""
        meta_dict = dict(self.job_meta_dict)
        meta_dict["job_id"] = "job-race-dup-001"
        meta_dict["attempt_id"] = "att-race-dup-001"
        meta_dict["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(meta_dict))
        ct_header, body = build_multipart_body(
            json.dumps(meta_dict), self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        initial_inference_count = self.worker.inference_count
        barrier = threading.Barrier(2, timeout=THREAD_TIMEOUT_SECONDS)
        results = [None, None]

        def submit(idx):
            barrier.wait()
            results[idx] = self.modal_gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, body)

        run_concurrently(self, lambda: submit(0), lambda: submit(1))

        self.assertEqual(results[0][0], 202)
        self.assertEqual(results[1][0], 202)

        acc1 = JobAcceptedResponse.from_dict(json.loads(results[0][2].decode("utf-8")))
        acc2 = JobAcceptedResponse.from_dict(json.loads(results[1][2].decode("utf-8")))

        self.assertEqual(acc1.handle, acc2.handle)
        self.assertEqual(acc1.request_digest, acc2.request_digest)
        # Worker was executed exactly once
        self.assertEqual(self.worker.inference_count - initial_inference_count, 1)

    def test_concurrent_exact_duplicate_with_slow_barrier_worker_forces_race_asserts_single_inference(self):
        """Deterministic barrier test forcing race while worker is running; asserts exactly 1 inference, no duplicate billable work."""
        class SlowBarrierWorker(FluxWorker):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, **kwargs)
                self.entered_infer = threading.Event()
                self.release_infer = threading.Event()
                self.infer_invocations = 0
                self._infer_lock = threading.Lock()

            def infer(self, record, img_bytes, hint_bytes):
                with self._infer_lock:
                    self.infer_invocations += 1
                self.entered_infer.set()
                # Block until the concurrent request attempt has executed its claim check
                self.release_infer.wait(timeout=5.0)
                return super().infer(record, img_bytes, hint_bytes)

        slow_worker = SlowBarrierWorker(provider="modal", limits=self.limits)
        gateway = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
            handle_registry=InMemoryHandleRegistry(),
            worker=slow_worker,
            proxy_token_id="modal-key-123",
            proxy_token_secret="modal-secret-456",
            auto_execute=True,
        )

        meta_dict = dict(self.job_meta_dict)
        meta_dict["job_id"] = "job-barrier-race-001"
        meta_dict["attempt_id"] = "att-barrier-race-001"
        meta_dict["request_digest"] = compute_request_digest(JobRequestMetadata.from_dict(meta_dict))
        ct_header, body = build_multipart_body(
            json.dumps(meta_dict), self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = {"Modal-Key": "modal-key-123", "Modal-Secret": "modal-secret-456", "Content-Type": ct_header}

        results = [None, None]

        def req1():
            results[0] = gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, body)

        def req2():
            # Wait until req1 has transitioned state to RUNNING and entered infer()
            self.assertTrue(slow_worker.entered_infer.wait(timeout=5.0))
            # Submit identical request while req1 is actively running inference
            results[1] = gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, body)
            # Release req1 to finish its inference
            slow_worker.release_infer.set()

        run_concurrently(self, req1, req2)

        # Both requests return HTTP 202 Accepted
        self.assertEqual(results[0][0], 202)
        self.assertEqual(results[1][0], 202)

        acc1 = JobAcceptedResponse.from_dict(json.loads(results[0][2].decode("utf-8")))
        acc2 = JobAcceptedResponse.from_dict(json.loads(results[1][2].decode("utf-8")))

        self.assertEqual(acc1.handle, acc2.handle)
        self.assertEqual(acc1.request_digest, acc2.request_digest)
        self.assertEqual(acc1.status, JobExecutionStatus.PENDING.value)
        self.assertEqual(acc2.status, JobExecutionStatus.PENDING.value)

        # Assert exactly one inference ran and no duplicate billable work occurred
        self.assertEqual(slow_worker.infer_invocations, 1)
        self.assertEqual(slow_worker.inference_count, 1)

        # Verify job completed and valid result can be downloaded
        code, _, status_body = gateway.handle_http_request("GET", f"/mc/v1/jobs/{acc1.handle}", req_headers, b"")
        self.assertEqual(code, 200)
        status_resp = JobStatusResponse.from_dict(json.loads(status_body.decode("utf-8")))
        self.assertEqual(status_resp.status, JobExecutionStatus.COMPLETED.value)

        code, res_headers, res_body = gateway.handle_http_request("GET", f"/mc/v1/jobs/{acc1.handle}/result", req_headers, b"")
        self.assertEqual(code, 200)
        self.assertEqual(res_headers["Content-Type"], "image/png")
        self.assertEqual(len(res_body), status_resp.result_bytes)

        # A download that stopped part way asks for the rest and gets only that.
        total = len(res_body)
        code, part_headers, rest = gateway.handle_http_request(
            "GET", f"/mc/v1/jobs/{acc1.handle}/result", {**req_headers, "Range": "bytes=7-"}, b"")
        self.assertEqual(code, 206)
        self.assertEqual(part_headers["Content-Type"], "image/png")
        self.assertEqual(part_headers["Content-Range"], f"bytes 7-{total - 1}/{total}")
        self.assertEqual(res_body[:7] + rest, res_body)
        code, past_headers, _ = gateway.handle_http_request(
            "GET", f"/mc/v1/jobs/{acc1.handle}/result", {**req_headers, "range": f"bytes={total}-"}, b"")
        self.assertEqual(code, 416)
        self.assertEqual(past_headers["Content-Range"], f"bytes */{total}")
        # Any other form of the header is ignored: the whole result, as before.
        for other in ("bytes=0-9", "bytes=-5", "items=3-", "bytes=x-"):
            code, _, whole = gateway.handle_http_request(
                "GET", f"/mc/v1/jobs/{acc1.handle}/result", {**req_headers, "Range": other}, b"")
            self.assertEqual((code, whole), (200, res_body), other)

    def test_registry_atomic_claim_execution_transitions_and_refusals(self):
        """claim_execution atomically transitions PENDING->RUNNING and refuses cancelled or terminal states."""
        registry = InMemoryHandleRegistry()
        record = registry.register_job(
            job_meta=self.job_meta,
            native_handle="native-claim-1",
            app_handle="handle-claim-1",
            provider="modal",
        )
        self.assertEqual(record.status, JobExecutionStatus.PENDING)

        # 1. First claim succeeds (PENDING -> RUNNING)
        self.assertTrue(registry.claim_execution("handle-claim-1"))
        self.assertEqual(record.status, JobExecutionStatus.RUNNING)
        self.assertIsNotNone(record.started_at)

        # 2. Duplicate claim fails (already RUNNING)
        self.assertFalse(registry.claim_execution("handle-claim-1"))

        # 3. Claim on completed job fails
        registry.store_result("handle-claim-1", b"PNG_DATA", "digest_123")
        self.assertEqual(record.status, JobExecutionStatus.COMPLETED)
        self.assertFalse(registry.claim_execution("handle-claim-1"))

        # 4. Claim on non-existent handle fails
        self.assertFalse(registry.claim_execution("handle-nonexistent"))

        # 5. Claim on cancel_requested job fails
        job_meta_2 = JobRequestMetadata.from_dict({
            **self.job_meta_dict,
            "job_id": "job-claim-2",
            "attempt_id": "att-claim-2",
            "request_digest": compute_request_digest(
                JobRequestMetadata.from_dict({**self.job_meta_dict, "job_id": "job-claim-2", "attempt_id": "att-claim-2"})
            ),
        })
        record2 = registry.register_job(
            job_meta=job_meta_2,
            native_handle="native-claim-2",
            app_handle="handle-claim-2",
            provider="modal",
        )
        registry.request_cancel("handle-claim-2")
        self.assertTrue(record2.cancel_requested)
        self.assertFalse(registry.claim_execution("handle-claim-2"))
        self.assertEqual(record2.status, JobExecutionStatus.PENDING)

    def test_cancel_races_terminal_transition_between_lookup_and_request_cancel(self):
        """Deterministic monkeypatch/barrier test for terminal transition racing between lookup and request_cancel."""
        gateway = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
            handle_registry=InMemoryHandleRegistry(),
            worker=self.worker,
            proxy_token_id="modal-key-123",
            proxy_token_secret="modal-secret-456",
            auto_execute=False,
        )
        ct_header, multipart_body = build_multipart_body(
            self.job_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        # Submit job to get handle in PENDING state
        code, _, body = gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, multipart_body)
        self.assertEqual(code, 202)
        accepted = JobAcceptedResponse.from_dict(json.loads(body.decode("utf-8")))
        handle = accepted.handle

        # Synchronize cancel request between lookup and request_cancel
        barrier_entered_cancel = threading.Barrier(2)
        barrier_transition_done = threading.Barrier(2)
        orig_request_cancel = gateway.handle_registry.request_cancel

        def monkeypatched_request_cancel(app_handle):
            # 1. At this point, get_by_app_handle lookup and pre-check in _route_job_cancel have completed
            barrier_entered_cancel.wait(timeout=5.0)
            # 2. Wait for concurrent worker to transition the job to COMPLETED
            barrier_transition_done.wait(timeout=5.0)
            # 3. Proceed with atomic request_cancel under lock, which should return None
            return orig_request_cancel(app_handle)

        gateway.handle_registry.request_cancel = monkeypatched_request_cancel

        cancel_response = [None]

        def cancel_caller():
            cancel_response[0] = gateway.handle_http_request(
                "POST", f"/mc/v1/jobs/{handle}/cancel", self.modal_auth_headers, b""
            )

        def worker_completer():
            # Wait until cancel handler passes lookup and enters request_cancel hook
            barrier_entered_cancel.wait(timeout=5.0)
            # Simulate worker completing job before request_cancel lock is acquired
            gateway.handle_registry.update_status(handle, JobExecutionStatus.COMPLETED)
            # Signal cancel thread to resume
            barrier_transition_done.wait(timeout=5.0)

        run_concurrently(self, cancel_caller, worker_completer)

        self.assertIsNotNone(cancel_response[0])
        code, _, resp_body = cancel_response[0]

        # Must return typed 409 Conflict with job_terminal error code
        self.assertEqual(code, 409)
        err = json.loads(resp_body.decode("utf-8"))
        self.assertEqual(err["error_code"], "job_terminal")
        self.assertIn("terminal state 'completed'", err["message"])

        # Invariant: cancel was not accepted, acknowledged was never True
        self.assertNotIn("acknowledged", err)
        record = gateway.handle_registry.get_by_app_handle(handle)
        self.assertIsNotNone(record)
        self.assertEqual(record.status, JobExecutionStatus.COMPLETED)
        self.assertFalse(record.cancel_requested)

    def test_cancel_races_failed_transition_monkeypatch_returns_409_job_terminal(self):
        """Deterministic monkeypatch test: transition to FAILED between lookup and request_cancel returns typed 409."""
        gateway = CloudGateway(
            provider=CloudProvider.MODAL,
            limits=self.limits,
            handle_registry=InMemoryHandleRegistry(),
            worker=self.worker,
            proxy_token_id="modal-key-123",
            proxy_token_secret="modal-secret-456",
            auto_execute=False,
        )
        ct_header, multipart_body = build_multipart_body(
            self.job_meta_json, self.tiny_image_bytes, self.tiny_hint_bytes
        )
        req_headers = dict(self.modal_auth_headers)
        req_headers["Content-Type"] = ct_header

        code, _, body = gateway.handle_http_request("POST", "/mc/v1/jobs", req_headers, multipart_body)
        self.assertEqual(code, 202)
        handle = JobAcceptedResponse.from_dict(json.loads(body.decode("utf-8"))).handle

        orig_request_cancel = gateway.handle_registry.request_cancel

        def monkeypatched_request_cancel(app_handle):
            gateway.handle_registry.update_status(app_handle, JobExecutionStatus.FAILED)
            return orig_request_cancel(app_handle)

        gateway.handle_registry.request_cancel = monkeypatched_request_cancel

        code, _, resp_body = gateway.handle_http_request(
            "POST", f"/mc/v1/jobs/{handle}/cancel", self.modal_auth_headers, b""
        )
        self.assertEqual(code, 409)
        err = json.loads(resp_body.decode("utf-8"))
        self.assertEqual(err["error_code"], "job_terminal")
        self.assertIn("terminal state 'failed'", err["message"])

        record = gateway.handle_registry.get_by_app_handle(handle)
        self.assertEqual(record.status, JobExecutionStatus.FAILED)
        self.assertFalse(record.cancel_requested)


if __name__ == "__main__":
    unittest.main()
