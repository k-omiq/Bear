"""Unit tests for Python cloud wire contract, JSON fixtures, and invariants."""

import json
import math
import os
import unittest
from pathlib import Path

# Adjust path so test can run from any working directory
FIXTURES_DIR = Path(__file__).resolve().parent.parent / "fixtures"

import sys
REPO_ROOT = Path(__file__).resolve().parent.parent.parent.parent
sys.path.insert(0, str(REPO_ROOT))

from deploy.cloud.common.contract import (
    PROTOCOL_VERSION,
    LATENT_STRIDE,
    FORBIDDEN_PAYLOAD_FIELDS,
    CloudProvider,
    JobExecutionStatus,
    ClientDispatchState,
    ContractValidationError,
    RenderRecipe,
    ServiceLimits,
    provisional_fixture_limits,
    HealthResponse,
    ModelInfoResponse,
    JobRequestMetadata,
    JobAcceptedResponse,
    TypedError,
    JobStatusResponse,
    JobCancelResponse,
    PreEnqueueRejectionResponse,
    ResultMetadata,
    WarmupResponse,
    compute_canonical_json_array,
    compute_request_digest,
    validate_health_response,
    validate_model_info_response,
    validate_warmup_response,
    validate_job_request_metadata,
    validate_job_accepted_response,
    validate_job_status_response,
    validate_crop_payload,
    validate_recipe_compatibility,
    validate_response_binding,
    validate_cancel_response,
    is_authoritative_safe_to_retry,
    validate_result_metadata,
    validate_result_bytes,
    validate_png_header,
    validate_worker_result,
)


class TestCloudContract(unittest.TestCase):
    def setUp(self):
        with open(FIXTURES_DIR / "tiny_image.png", "rb") as f:
            self.tiny_image_bytes = f.read()
        with open(FIXTURES_DIR / "tiny_hint.png", "rb") as f:
            self.tiny_hint_bytes = f.read()
        self.limits = provisional_fixture_limits()

    def test_load_fixtures_health(self):
        with open(FIXTURES_DIR / "health_response.json", "r", encoding="utf-8") as f:
            data = json.load(f)
        resp = HealthResponse.from_dict(data)
        validate_health_response(resp)
        self.assertEqual(resp.status, "ok")
        self.assertEqual(resp.provider, "modal")
        self.assertEqual(resp.protocol_version, PROTOCOL_VERSION)

    def test_load_fixtures_model_info(self):
        with open(FIXTURES_DIR / "model_info_response.json", "r", encoding="utf-8") as f:
            data = json.load(f)
        resp = ModelInfoResponse.from_dict(data)
        validate_model_info_response(resp)
        self.assertEqual(resp.protocol_version, PROTOCOL_VERSION)
        self.assertEqual(resp.model_id, "test-flux-schnell")
        self.assertEqual(resp.model_revision, "0123456789abcdef0123456789abcdef01234567")
        self.assertEqual(resp.recipe_id, "test-sdnq-v1")
        self.assertEqual(resp.preprocessing_version, "1.0.0")
        # Native mask conditioning MUST be false honestly
        self.assertFalse(resp.native_mask_conditioning)
        self.assertEqual(resp.limits.max_width, 2048)
        self.assertEqual(resp.limits.max_height, 2048)
        self.assertEqual(resp.limits.max_pixels, 4194304)

    def test_load_fixtures_job_request_and_digest_vector(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            data = json.load(f)
        meta = JobRequestMetadata.from_dict(data)
        validate_job_request_metadata(meta)
        self.assertEqual(meta.protocol_version, PROTOCOL_VERSION)
        self.assertEqual(meta.job_id, "job-test-001")
        self.assertEqual(meta.attempt_id, "attempt-test-001")
        self.assertEqual(meta.width, 16)
        self.assertEqual(meta.height, 16)
        self.assertEqual(meta.seed, 42)
        self.assertEqual(meta.steps, 4)
        self.assertEqual(meta.guidance_scaled, 350)
        self.assertFalse(meta.recipe.native_mask_conditioning)

        with open(FIXTURES_DIR / "canonical_digest_vector.json", "r", encoding="utf-8") as f:
            vector = json.load(f)
        canonical_str = compute_canonical_json_array(meta)
        self.assertEqual(canonical_str, vector["canonical_json"])
        computed_digest = compute_request_digest(meta)
        self.assertEqual(computed_digest, vector["expected_sha256"])
        self.assertEqual(computed_digest, meta.request_digest)

    def test_canonical_digest_collision_resistance(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            base_data = json.load(f)

        data_a = dict(base_data)
        data_a["job_id"] = "a|b"
        data_a["attempt_id"] = "c"
        meta_a = JobRequestMetadata.from_dict(data_a)

        data_b = dict(base_data)
        data_b["job_id"] = "a"
        data_b["attempt_id"] = "b|c"
        meta_b = JobRequestMetadata.from_dict(data_b)

        digest_a = compute_request_digest(meta_a)
        digest_b = compute_request_digest(meta_b)
        self.assertNotEqual(digest_a, digest_b)

        with open(FIXTURES_DIR / "canonical_digest_vector.json", "r", encoding="utf-8") as f:
            vector = json.load(f)
        self.assertEqual(digest_a, vector["collision_case_a"]["expected_sha256"])
        self.assertEqual(digest_b, vector["collision_case_b"]["expected_sha256"])

    def test_strict_type_coercion_rejection(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            base_data = json.load(f)

        # bool passed for int
        bad_data = dict(base_data)
        bad_data["width"] = True
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(bad_data)

        # string passed for bool
        bad_data = dict(base_data)
        bad_recipe = dict(bad_data["recipe"])
        bad_recipe["native_mask_conditioning"] = "false"
        bad_data["recipe"] = bad_recipe
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(bad_data)

        # non-hex or mutable revisions
        for mutable_rev in ["main", "master", "latest", "head", "dev", "staging", "not-a-40-hex-hash"]:
            bad_data = dict(base_data)
            bad_recipe = dict(bad_data["recipe"])
            bad_recipe["model_revision"] = mutable_rev
            bad_data["recipe"] = bad_recipe
            with self.assertRaises(ContractValidationError):
                JobRequestMetadata.from_dict(bad_data)

        # out of range parameters
        bad_data = dict(base_data)
        bad_data["steps"] = 0
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(bad_data)

        bad_data = dict(base_data)
        bad_data["steps"] = 101
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(bad_data)

        bad_data = dict(base_data)
        bad_data["guidance_scaled"] = -1
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(bad_data)

        bad_data = dict(base_data)
        bad_data["guidance_scaled"] = 5001
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(bad_data)

    def test_job_status_cost_validation(self):
        with open(FIXTURES_DIR / "job_status_completed.json", "r", encoding="utf-8") as f:
            data = json.load(f)

        # Negative cost
        bad_data = dict(data)
        bad_data["reported_cost_usd"] = -0.5
        with self.assertRaises(ContractValidationError):
            JobStatusResponse.from_dict(bad_data)

        # Non-finite cost (inf)
        bad_data = dict(data)
        bad_data["reported_cost_usd"] = float("inf")
        with self.assertRaises(ContractValidationError):
            JobStatusResponse.from_dict(bad_data)

        # Non-finite cost (nan)
        bad_data = dict(data)
        bad_data["reported_cost_usd"] = float("nan")
        with self.assertRaises(ContractValidationError):
            JobStatusResponse.from_dict(bad_data)

    def test_load_fixtures_job_accepted(self):
        with open(FIXTURES_DIR / "job_accepted_response.json", "r", encoding="utf-8") as f:
            data = json.load(f)
        resp = JobAcceptedResponse.from_dict(data)
        validate_job_accepted_response(resp)
        self.assertEqual(resp.handle, "handle-test-modal-999")
        self.assertEqual(resp.status, "pending")
        self.assertEqual(resp.job_id, "job-test-001")
        self.assertEqual(resp.attempt_id, "attempt-test-001")
        self.assertEqual(resp.recipe_id, "test-sdnq-v1")
        self.assertEqual(resp.model_id, "test-flux-schnell")
        self.assertEqual(resp.model_revision, "0123456789abcdef0123456789abcdef01234567")
        self.assertEqual(resp.preprocessing_version, "1.0.0")
        self.assertFalse(resp.native_mask_conditioning)

    def test_load_fixtures_job_status_variants(self):
        for name, expected_status in [
            ("job_status_pending.json", "pending"),
            ("job_status_running.json", "running"),
            ("job_status_completed.json", "completed"),
            ("job_status_failed.json", "failed"),
        ]:
            with open(FIXTURES_DIR / name, "r", encoding="utf-8") as f:
                data = json.load(f)
            resp = JobStatusResponse.from_dict(data)
            validate_job_status_response(resp)
            self.assertEqual(resp.status, expected_status)
            if expected_status == "completed":
                self.assertIsNotNone(resp.reported_cost_usd)
                self.assertIsNotNone(resp.result_digest)
                self.assertIsNotNone(resp.result_bytes)
            elif expected_status == "failed":
                self.assertIsNotNone(resp.error)
                self.assertEqual(resp.error.error_code, "execution_failed")

    def test_load_fixtures_job_cancel(self):
        with open(FIXTURES_DIR / "job_cancel_response.json", "r", encoding="utf-8") as f:
            data = json.load(f)
        resp = JobCancelResponse.from_dict(data)
        self.assertEqual(resp.handle, "handle-test-modal-999")
        self.assertEqual(resp.status, "cancel_requested")
        self.assertTrue(resp.acknowledged)

        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            meta = JobRequestMetadata.from_dict(json.load(f))
        validate_cancel_response(meta, resp, expected_handle="handle-test-modal-999")

    def test_load_fixtures_rejection_and_retry_gating(self):
        with open(FIXTURES_DIR / "pre_enqueue_rejection.json", "r", encoding="utf-8") as f:
            data = json.load(f)
        resp = PreEnqueueRejectionResponse.from_dict(data)
        self.assertFalse(resp.enqueued)
        self.assertEqual(resp.error_code, "unsupported_recipe")
        self.assertFalse(resp.retryable)

        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            meta = JobRequestMetadata.from_dict(json.load(f))

        # Unsupported recipe is not safe to retry
        self.assertFalse(is_authoritative_safe_to_retry(resp, meta))

        # Allowlisted retryable code (e.g. rate limit pre-queue) with enqueued=False is safe to retry
        retryable_rejection = PreEnqueueRejectionResponse(
            enqueued=False,
            error_code="rate_limited_pre_queue",
            message="Temporarily rate limited before queuing",
            job_id=meta.job_id,
            attempt_id=meta.attempt_id,
            request_digest=meta.request_digest,
            retryable=True,
        )
        self.assertTrue(is_authoritative_safe_to_retry(retryable_rejection, meta))

        for code in ("weights_missing", "weights_corrupt"):
            rejection = PreEnqueueRejectionResponse.from_dict({
                **data, "error_code": code, "retryable": False,
                "job_id": meta.job_id, "attempt_id": meta.attempt_id,
                "request_digest": meta.request_digest,
            })
            self.assertFalse(is_authoritative_safe_to_retry(rejection, meta))

    def test_payload_validation_valid(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            meta = JobRequestMetadata.from_dict(json.load(f))
        validate_crop_payload(meta, self.tiny_image_bytes, self.tiny_hint_bytes, self.limits)

    def test_payload_validation_png_header_corruption(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            meta = JobRequestMetadata.from_dict(json.load(f))
        corrupted_png = b"NOT_A_PNG_HEADER_BUFFER_TOO_SHORT"
        with self.assertRaises(ContractValidationError):
            validate_crop_payload(meta, corrupted_png, self.tiny_hint_bytes, self.limits)

    def test_payload_validation_forbids_page_coordinates(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            raw = json.load(f)
        for forbidden in ["x", "y", "page_x", "strip_rect", "on_page", "region_id", "file_path"]:
            bad_dict = dict(raw)
            bad_dict[forbidden] = 100
            with self.assertRaises(ContractValidationError):
                JobRequestMetadata.from_dict(bad_dict)

    def test_payload_validation_rejects_unknown_fields(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            raw = json.load(f)
        raw["arbitrary_custom_field"] = "malicious_or_unexpected"
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(raw)

    def test_payload_validation_stride_alignment(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            raw = json.load(f)
        raw["width"] = 15  # not divisible by LATENT_STRIDE 16
        meta = JobRequestMetadata.from_dict(raw)
        with self.assertRaises(ContractValidationError):
            validate_crop_payload(meta, self.tiny_image_bytes, self.tiny_hint_bytes, self.limits)

    def test_payload_validation_max_bounds_and_pixels(self):
        limits = ServiceLimits(max_width=64, max_height=64, max_pixels=1024, max_png_bytes=10000, max_multipart_bytes=20000, worker_timeout_seconds=60)
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            raw = json.load(f)
        raw["width"] = 64
        raw["height"] = 64  # 64*64 = 4096 > 1024
        meta = JobRequestMetadata.from_dict(raw)
        with self.assertRaises(ContractValidationError):
            validate_crop_payload(meta, self.tiny_image_bytes, self.tiny_hint_bytes, limits=limits)

    def test_payload_validation_checksum_mismatch(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            raw = json.load(f)
        meta = JobRequestMetadata.from_dict(raw)
        # Construct slightly altered PNG bytes with matching header but mismatched hash
        corrupted_bytes = self.tiny_image_bytes[:30] + b"\x01" + self.tiny_image_bytes[31:]
        with self.assertRaises(ContractValidationError):
            validate_crop_payload(meta, corrupted_bytes, self.tiny_hint_bytes, self.limits)

    def test_recipe_compatibility_validation(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            meta = JobRequestMetadata.from_dict(json.load(f))
        with open(FIXTURES_DIR / "model_info_response.json", "r", encoding="utf-8") as f:
            model_info = ModelInfoResponse.from_dict(json.load(f))

        # Matches exactly
        validate_recipe_compatibility(meta, model_info)

        # Mismatched model
        bad_info = ModelInfoResponse(
            protocol_version=model_info.protocol_version,
            provider=model_info.provider,
            model_id="other-flux-model",
            model_revision=model_info.model_revision,
            recipe_id=model_info.recipe_id,
            preprocessing_version=model_info.preprocessing_version,
            native_mask_conditioning=model_info.native_mask_conditioning,
            limits=model_info.limits,
        )
        with self.assertRaises(ContractValidationError):
            validate_recipe_compatibility(meta, bad_info)

    def test_response_binding_validation(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            meta = JobRequestMetadata.from_dict(json.load(f))
        with open(FIXTURES_DIR / "job_accepted_response.json", "r", encoding="utf-8") as f:
            accepted = JobAcceptedResponse.from_dict(json.load(f))

        validate_response_binding(meta, accepted, expected_handle="handle-test-modal-999")

        # Tampered attempt_id
        bad_accepted = JobAcceptedResponse(
            handle=accepted.handle,
            status=accepted.status,
            job_id=accepted.job_id,
            attempt_id="wrong-attempt",
            request_digest=accepted.request_digest,
            recipe_id=accepted.recipe_id,
            preprocessing_version=accepted.preprocessing_version,
            model_id=accepted.model_id,
            model_revision=accepted.model_revision,
            native_mask_conditioning=accepted.native_mask_conditioning,
        )
        with self.assertRaises(ContractValidationError):
            validate_response_binding(meta, bad_accepted, expected_handle="handle-test-modal-999")

    def test_result_bytes_validation_and_request_binding(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            request_meta = JobRequestMetadata.from_dict(json.load(f))

        result_meta = ResultMetadata(
            handle="handle-test-modal-999",
            job_id="job-test-001",
            attempt_id="attempt-test-001",
            request_digest="42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
            recipe_id="test-sdnq-v1",
            preprocessing_version="1.0.0",
            model_id="test-flux-schnell",
            model_revision="0123456789abcdef0123456789abcdef01234567",
            native_mask_conditioning=False,
            result_digest="2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f",
            reported_cost_usd=0.0012,
            width=16,
            height=16,
            byte_length=len(self.tiny_image_bytes),
        )
        validate_result_bytes(self.tiny_image_bytes, result_meta, request_meta, self.limits, expected_handle="handle-test-modal-999")

        # Test geometry mismatch with request
        bad_result_meta = ResultMetadata(
            handle="handle-test-modal-999",
            job_id="job-test-001",
            attempt_id="attempt-test-001",
            request_digest="42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
            recipe_id="test-sdnq-v1",
            preprocessing_version="1.0.0",
            model_id="test-flux-schnell",
            model_revision="0123456789abcdef0123456789abcdef01234567",
            native_mask_conditioning=False,
            result_digest="2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f",
            reported_cost_usd=0.0012,
            width=32, # differs from request width 16
            height=16,
            byte_length=len(self.tiny_image_bytes),
        )
        with self.assertRaises(ContractValidationError):
            validate_result_bytes(self.tiny_image_bytes, bad_result_meta, request_meta, self.limits, expected_handle="handle-test-modal-999")

    def test_uppercase_hex_hash_rejection(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            base_data = json.load(f)
        bad_data = dict(base_data)
        bad_data["request_digest"] = base_data["request_digest"].upper()
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(bad_data)

    def test_ascii_id_whitespace_and_control_rejection(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            base_data = json.load(f)
        for bad_id in ["job test", "job\ntest", "job\x00test", ""]:
            bad_data = dict(base_data)
            bad_data["job_id"] = bad_id
            with self.assertRaises(ContractValidationError):
                JobRequestMetadata.from_dict(bad_data)

    def test_native_mask_conditioning_honesty_rejection(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            base_data = json.load(f)
        bad_data = dict(base_data)
        bad_recipe = dict(bad_data["recipe"])
        bad_recipe["native_mask_conditioning"] = True
        bad_data["recipe"] = bad_recipe
        with self.assertRaises(ContractValidationError):
            JobRequestMetadata.from_dict(bad_data)

    def test_response_binding_validates_semantics(self):
        with open(FIXTURES_DIR / "job_request_metadata.json", "r", encoding="utf-8") as f:
            meta = JobRequestMetadata.from_dict(json.load(f))
        with open(FIXTURES_DIR / "job_accepted_response.json", "r", encoding="utf-8") as f:
            accepted = JobAcceptedResponse.from_dict(json.load(f))

        # Accepted response with valid pending status succeeds response binding
        validate_response_binding(meta, accepted, expected_handle="handle-test-modal-999")

        # Non-pending status in JobAcceptedResponse must fail closed
        for invalid_status in ["completed", "running", "failed", "cancelled", "invalid_unknown_state"]:
            bad_dict = {
                "handle": accepted.handle,
                "status": invalid_status,
                "job_id": accepted.job_id,
                "attempt_id": accepted.attempt_id,
                "request_digest": accepted.request_digest,
                "recipe_id": accepted.recipe_id,
                "preprocessing_version": accepted.preprocessing_version,
                "model_id": accepted.model_id,
                "model_revision": accepted.model_revision,
                "native_mask_conditioning": accepted.native_mask_conditioning,
            }
            with self.assertRaises(ContractValidationError):
                JobAcceptedResponse.from_dict(bad_dict)

    def test_warmup_response_contract(self):
        warmup_data = {
            "status": "ok",
            "provider": "modal",
            "protocol_version": PROTOCOL_VERSION,
            "worker_state": "warm",
        }
        resp = WarmupResponse.from_dict(warmup_data)
        validate_warmup_response(resp)
        self.assertEqual(resp.status, "ok")
        self.assertEqual(resp.provider, "modal")
        self.assertEqual(resp.worker_state, "warm")

        # Unknown provider fails
        bad_data = dict(warmup_data)
        bad_data["provider"] = "unknown_provider"
        with self.assertRaises(ContractValidationError):
            WarmupResponse.from_dict(bad_data)

        # Protocol version mismatch fails
        bad_data = dict(warmup_data)
        bad_data["protocol_version"] = "9.9.9"
        with self.assertRaises(ContractValidationError):
            WarmupResponse.from_dict(bad_data)

    def test_validate_worker_result_valid(self):
        """Verify validate_worker_result accepts conforming PNG buffer and metadata."""
        import hashlib
        digest = hashlib.sha256(self.tiny_image_bytes).hexdigest()
        validate_worker_result(
            result_bytes=self.tiny_image_bytes,
            result_digest=digest,
            expected_width=16,
            expected_height=16,
            limits=self.limits,
            reported_cost_usd=0.0012,
        )

    def test_validate_worker_result_corrupted_output(self):
        """Verify validate_worker_result rejects corrupted PNG output."""
        import hashlib
        # 1. Truncated / malformed chunk
        corrupted_bytes = b"\x89PNG\r\n\x1a\nCORRUPTED_IDAT_BYTES_TRUNCATED"
        digest = hashlib.sha256(corrupted_bytes).hexdigest()
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=corrupted_bytes,
                result_digest=digest,
                expected_width=16,
                expected_height=16,
                limits=self.limits,
            )

        # 2. Corrupted PNG signature
        corrupted_sig = b"NOT_PNG_SIGNATURE" + self.tiny_image_bytes[17:]
        digest_sig = hashlib.sha256(corrupted_sig).hexdigest()
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=corrupted_sig,
                result_digest=digest_sig,
                expected_width=16,
                expected_height=16,
                limits=self.limits,
            )

        # 3. Empty buffer
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=b"",
                result_digest="0" * 64,
                expected_width=16,
                expected_height=16,
                limits=self.limits,
            )

    def test_validate_worker_result_wrong_dimensions(self):
        """Verify validate_worker_result rejects PNG buffers with dimensions differing from request."""
        import hashlib
        # tiny_image.png is 16x16
        digest = hashlib.sha256(self.tiny_image_bytes).hexdigest()

        # Expected width 32, but PNG contains 16
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=self.tiny_image_bytes,
                result_digest=digest,
                expected_width=32,
                expected_height=16,
                limits=self.limits,
            )

        # Expected height 32, but PNG contains 16
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=self.tiny_image_bytes,
                result_digest=digest,
                expected_width=16,
                expected_height=32,
                limits=self.limits,
            )

    def test_validate_worker_result_digest_mismatch(self):
        """Verify validate_worker_result rejects mismatched SHA-256 digest."""
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=self.tiny_image_bytes,
                result_digest="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                expected_width=16,
                expected_height=16,
                limits=self.limits,
            )

    def test_validate_worker_result_limit_and_stride_violations(self):
        """Verify validate_worker_result enforces max bounds, pixel counts, and stride alignment."""
        import hashlib
        digest = hashlib.sha256(self.tiny_image_bytes).hexdigest()

        # Non-stride-aligned width (15)
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=self.tiny_image_bytes,
                result_digest=digest,
                expected_width=15,
                expected_height=16,
                limits=self.limits,
            )

        # Non-stride-aligned height (15)
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=self.tiny_image_bytes,
                result_digest=digest,
                expected_width=16,
                expected_height=15,
                limits=self.limits,
            )

        # Exceeds max_png_bytes limit
        tiny_limits = ServiceLimits(
            max_width=2048,
            max_height=2048,
            max_pixels=4194304,
            max_png_bytes=10,  # smaller than tiny_image_bytes
            max_multipart_bytes=33554432,
            worker_timeout_seconds=120,
        )
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=self.tiny_image_bytes,
                result_digest=digest,
                expected_width=16,
                expected_height=16,
                limits=tiny_limits,
            )

        # Negative cost
        with self.assertRaises(ContractValidationError):
            validate_worker_result(
                result_bytes=self.tiny_image_bytes,
                result_digest=digest,
                expected_width=16,
                expected_height=16,
                limits=self.limits,
                reported_cost_usd=-0.05,
            )

    def test_png_full_stream_validation_and_regressions(self):
        """Verify full PNG stream chunk bounds, CRC checks, trailing data rejection, and IDAT corruption regressions."""
        import struct
        import zlib

        # 1. Valid stream passes
        validate_png_header(self.tiny_image_bytes, 16, 16, 2)
        validate_png_header(self.tiny_hint_bytes, 16, 16, 0)

        # 2. Regression: Valid header but corrupt/truncated IDAT
        # Construct valid 8-byte sig + valid 13-byte IHDR + valid CRC
        ihdr_data = struct.pack(">IIBBBBB", 16, 16, 8, 2, 0, 0, 0)
        ihdr_crc = zlib.crc32(b"IHDR" + ihdr_data) & 0xFFFFFFFF
        ihdr_chunk = struct.pack(">I", 13) + b"IHDR" + ihdr_data + struct.pack(">I", ihdr_crc)
        valid_hdr = b"\x89PNG\r\n\x1a\n" + ihdr_chunk

        # Corrupt IDAT payload (invalid zlib stream)
        bad_idat_payload = b"NOT_VALID_ZLIB_DATA_12345"
        bad_idat_crc = zlib.crc32(b"IDAT" + bad_idat_payload) & 0xFFFFFFFF
        bad_idat_chunk = struct.pack(">I", len(bad_idat_payload)) + b"IDAT" + bad_idat_payload + struct.pack(">I", bad_idat_crc)
        iend_crc = zlib.crc32(b"IEND") & 0xFFFFFFFF
        iend_chunk = struct.pack(">I", 0) + b"IEND" + struct.pack(">I", iend_crc)

        corrupt_idat_png = valid_hdr + bad_idat_chunk + iend_chunk
        with self.assertRaises(ContractValidationError):
            validate_png_header(corrupt_idat_png, 16, 16, 2)

        # Truncated IDAT chunk (buffer ends prematurely inside IDAT chunk)
        truncated_idat_png = valid_hdr + struct.pack(">I", 100) + b"IDAT" + b"SHORT"
        with self.assertRaises(ContractValidationError):
            validate_png_header(truncated_idat_png, 16, 16, 2)

        # 3. Trailing data after IEND chunk
        trailing_data_png = self.tiny_image_bytes + b"MALICIOUS_TRAILING_BYTES"
        with self.assertRaises(ContractValidationError):
            validate_png_header(trailing_data_png, 16, 16, 2)

        # 4. Missing IEND chunk (stream terminates before IEND)
        no_iend_png = self.tiny_image_bytes[:-12]
        with self.assertRaises(ContractValidationError):
            validate_png_header(no_iend_png, 16, 16, 2)

        # 5. Chunk CRC-32 mismatch (tampered IEND CRC)
        tampered_crc_png = self.tiny_image_bytes[:-4] + b"\x00\x00\x00\x00"
        with self.assertRaises(ContractValidationError):
            validate_png_header(tampered_crc_png, 16, 16, 2)

        # 6. Invalid filter byte in decoded scanlines (filter byte 5)
        raw_bad_scanlines = bytearray()
        for _ in range(16):
            raw_bad_scanlines.append(5)  # Invalid filter byte (must be 0..4)
            raw_bad_scanlines.extend(b"\x00" * (16 * 3))
        bad_compressed = zlib.compress(bytes(raw_bad_scanlines))
        bad_idat_len = len(bad_compressed)
        bad_idat_crc = zlib.crc32(b"IDAT" + bad_compressed) & 0xFFFFFFFF
        bad_filter_idat = struct.pack(">I", bad_idat_len) + b"IDAT" + bad_compressed + struct.pack(">I", bad_idat_crc)
        bad_filter_png = valid_hdr + bad_filter_idat + iend_chunk
        with self.assertRaises(ContractValidationError):
            validate_png_header(bad_filter_png, 16, 16, 2)

        # 7. Non-bytes input or buffer too short
        with self.assertRaises(ContractValidationError):
            validate_png_header("not_bytes", 16, 16, 2)  # type: ignore
        with self.assertRaises(ContractValidationError):
            validate_png_header(b"\x89PNG\r\n\x1a\n", 16, 16, 2)

    def test_png_rejects_concatenated_or_trailing_idat_streams_and_truncated_streams(self):
        """Regression test: validate_png_stream must reject concatenated zlib streams in IDAT,
        trailing unused bytes, and truncated zlib streams even when decompressed size equals expected.
        """
        import struct
        import zlib

        # 1. Exact reproduction case: Append zlib.compress(b"extra") to valid IDAT payload
        # tiny_image.png has IHDR (len 13, ends at byte 33), IDAT (len 16, offset 33..61), IEND (offset 61..73)
        valid_idat_data = self.tiny_image_bytes[41:57]
        concatenated_idat_data = valid_idat_data + zlib.compress(b"extra")
        concat_idat_crc = zlib.crc32(b"IDAT" + concatenated_idat_data) & 0xFFFFFFFF
        concat_idat_chunk = (
            struct.pack(">I", len(concatenated_idat_data))
            + b"IDAT"
            + concatenated_idat_data
            + struct.pack(">I", concat_idat_crc)
        )
        concatenated_png = self.tiny_image_bytes[:33] + concat_idat_chunk + self.tiny_image_bytes[61:]
        with self.assertRaises(ContractValidationError):
            validate_png_header(concatenated_png, 16, 16, 2)

        # 2. Trailing uncompressed raw bytes inside IDAT chunk
        trailing_idat_data = valid_idat_data + b"TRAILING_UNCOMPRESSED_DATA"
        trailing_idat_crc = zlib.crc32(b"IDAT" + trailing_idat_data) & 0xFFFFFFFF
        trailing_idat_chunk = (
            struct.pack(">I", len(trailing_idat_data))
            + b"IDAT"
            + trailing_idat_data
            + struct.pack(">I", trailing_idat_crc)
        )
        trailing_idat_png = self.tiny_image_bytes[:33] + trailing_idat_chunk + self.tiny_image_bytes[61:]
        with self.assertRaises(ContractValidationError):
            validate_png_header(trailing_idat_png, 16, 16, 2)

        # 3. Truncated zlib stream where decompressed output length happens to equal expected
        # For 16x16 RGB8, expected raw scanlines size is 16 * (1 + 16 * 3) = 784 bytes.
        # Construct an uncompressed deflate block of 784 bytes missing the trailing 4-byte Adler-32 checksum.
        # Decompression yields exactly 784 bytes, but stream does not reach EOF (decompressor.eof is False).
        trunc_zlib_data = b"\x78\x01\x01" + struct.pack("<HH", 784, (~784) & 0xFFFF) + (b"\x00" * 784)
        trunc_idat_crc = zlib.crc32(b"IDAT" + trunc_zlib_data) & 0xFFFFFFFF
        trunc_idat_chunk = (
            struct.pack(">I", len(trunc_zlib_data))
            + b"IDAT"
            + trunc_zlib_data
            + struct.pack(">I", trunc_idat_crc)
        )
        trunc_png = self.tiny_image_bytes[:33] + trunc_idat_chunk + self.tiny_image_bytes[61:]
        with self.assertRaises(ContractValidationError):
            validate_png_header(trunc_png, 16, 16, 2)

        # 4. Decompressed output exceeds expected raw size
        oversized_raw = b"\x00" * 785
        oversized_compressed = zlib.compress(oversized_raw)
        oversized_idat_crc = zlib.crc32(b"IDAT" + oversized_compressed) & 0xFFFFFFFF
        oversized_idat_chunk = (
            struct.pack(">I", len(oversized_compressed))
            + b"IDAT"
            + oversized_compressed
            + struct.pack(">I", oversized_idat_crc)
        )
        oversized_png = self.tiny_image_bytes[:33] + oversized_idat_chunk + self.tiny_image_bytes[61:]
        with self.assertRaises(ContractValidationError):
            validate_png_header(oversized_png, 16, 16, 2)


if __name__ == "__main__":
    unittest.main()
