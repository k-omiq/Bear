"""Provider-Neutral CPU Control Gateway and API Implementation.

Implements the application-owned `/mc/v1` wire contract routes, bounded multipart
payload parsing, authenticated control plane dispatch, and secure result streaming.
"""

from __future__ import annotations

import asyncio
import base64
import email
import email.policy
import hmac
import json
import logging
import time
from typing import Any, Callable, Dict, List, Optional, Set, Tuple, Union

from deploy.cloud.common.contract import (
    ANALYSIS_BATCH_MAX_BODY_BYTES,
    ANALYSIS_BATCH_MAX_RESPONSE_BYTES,
    ANALYSIS_MAX_RESPONSE_BYTES,
    ANALYSIS_VERSION,
    GPU_JOB_OPERATIONS,
    PROTOCOL_VERSION,
    CloudProvider,
    ContractValidationError,
    HealthResponse,
    JobAcceptedResponse,
    JobCancelResponse,
    JobExecutionStatus,
    JobRequestMetadata,
    JobStatusResponse,
    ModelInfoResponse,
    PreEnqueueRejectionResponse,
    RenderRecipe,
    ResultMetadata,
    ServiceLimits,
    TypedError,
    WarmupResponse,
    provisional_fixture_limits,
    validate_crop_payload,
    validate_analysis_batch,
    validate_analysis_request,
    validate_gpu_status_response,
    validate_gpu_stop_request,
    validate_gpu_stop_response,
    validate_recipe_compatibility,
    validate_worker_result,
    valid_gpu_job_attempt,
    valid_gpu_job_handle,
)
from deploy.cloud.common.flux import FluxWorker, sampling_mismatch
from deploy.cloud.common.analysis_seed import AnalysisUnavailable
from deploy.cloud.common.handle_mapping import InMemoryHandleRegistry, JobRecord
from deploy.cloud.common.jobs import (
    GpuJobConflict,
    GpuWaitExceeded,
    LocalJobBackend,
    PreEnqueueError,
    SubmissionUnknownError,
    StopUncertainError,
    ResultUnavailableError,
    execute_worker_job,
)
from deploy.cloud.common.manifest import get_default_model_info
from deploy.cloud.common.redact import redact_text

logger = logging.getLogger("deploy.cloud.gateway")


def parse_multipart_payload(
    content_type_header: str,
    body_bytes: bytes,
    max_multipart_bytes: int,
) -> Tuple[Optional[str], Optional[bytes], Optional[bytes], Optional[str]]:
    """Parse multipart/form-data payload into (metadata_json_str, image_bytes, hint_bytes, error_msg)."""
    if len(body_bytes) > max_multipart_bytes:
        return None, None, None, f"Payload size {len(body_bytes)} exceeds maximum limit {max_multipart_bytes}"

    # Extract boundary
    ct_lower = content_type_header.lower()
    if not ct_lower.startswith("multipart/form-data"):
        return None, None, None, f"Expected Content-Type multipart/form-data, got '{content_type_header}'"

    try:
        header_line = f"Content-Type: {content_type_header}\r\n\r\n".encode("utf-8")
        msg = email.message_from_bytes(header_line + body_bytes, policy=email.policy.default)

        metadata_str: Optional[str] = None
        image_bytes: Optional[bytes] = None
        hint_bytes: Optional[bytes] = None
        seen_fields: Set[str] = set()

        for part in msg.iter_parts():
            disp = part.get("content-disposition", "")
            if not disp or "form-data" not in disp.lower():
                return None, None, None, "Malformed multipart part: missing or invalid Content-Disposition"

            name = part.get_param("name", header="content-disposition")
            if not name:
                return None, None, None, "Multipart part missing 'name' in Content-Disposition"
            name = str(name).strip().strip('"').strip("'")

            if name not in {"metadata", "image", "hint"}:
                return None, None, None, f"Unknown multipart field '{name}' (only metadata, image, hint allowed)"

            if name in seen_fields:
                return None, None, None, f"Duplicate multipart field '{name}' rejected"
            seen_fields.add(name)

            part_ct = part.get_content_type()
            payload = part.get_payload(decode=True)
            if payload is None:
                return None, None, None, f"Empty or undecodable payload for field '{name}'"

            if name == "metadata":
                if part_ct and part_ct not in ("application/json", "text/plain", "application/octet-stream"):
                    return None, None, None, f"Invalid Content-Type '{part_ct}' for metadata, expected application/json"
                try:
                    metadata_str = payload.decode("utf-8")
                except UnicodeDecodeError:
                    return None, None, None, "Metadata payload is not valid UTF-8"
            elif name == "image":
                if part_ct and part_ct not in ("image/png", "application/octet-stream"):
                    return None, None, None, f"Invalid Content-Type '{part_ct}' for image, expected image/png"
                image_bytes = payload
            elif name == "hint":
                if part_ct and part_ct not in ("image/png", "application/octet-stream"):
                    return None, None, None, f"Invalid Content-Type '{part_ct}' for hint, expected image/png"
                hint_bytes = payload

        missing = {"metadata", "image", "hint"} - seen_fields
        if missing:
            return None, None, None, f"Missing required multipart field(s): {sorted(missing)}"

        return metadata_str, image_bytes, hint_bytes, None
    except Exception as exc:
        return None, None, None, f"Malformed multipart body: {exc}"


class CloudGateway:
    """Provider-agnostic CPU Control Gateway implementing `/mc/v1`."""

    def __init__(
        self,
        provider: Union[CloudProvider, str],
        model_info: Optional[ModelInfoResponse] = None,
        limits: Optional[ServiceLimits] = None,
        handle_registry: Optional[InMemoryHandleRegistry] = None,
        worker: Optional[FluxWorker] = None,
        auth_validator: Optional[Callable[[Dict[str, str]], bool]] = None,
        proxy_token_id: Optional[str] = None,
        proxy_token_secret: Optional[str] = None,
        bearer_token: Optional[str] = None,
        auto_execute: bool = True,
        backend: Optional[Any] = None,
        trust_edge_auth: bool = False,
        analysis_worker: Optional[Any] = None,
        gpu_control: Optional[Any] = None,
        denoise_worker: Optional[Any] = None,
        code_digest: Optional[str] = None,
    ):
        if isinstance(provider, str):
            provider = CloudProvider(provider.lower())
        self.provider = provider
        self.analysis_worker = analysis_worker
        # Page denoise (common/denoise.py). None: this deployment has no denoise models.
        self.denoise_worker = denoise_worker
        # Which cloud code this deployment runs (common/release.py). None: it cannot say.
        self.code_digest = code_digest
        # GPU presence and stop (common/gpu.py). None: this deployment cannot say, and
        # GET /gpu answers supported=false rather than guessing from CPU health.
        self.gpu_control = gpu_control
        self.limits = limits or provisional_fixture_limits()
        self.model_info = model_info or get_default_model_info(self.provider.value, limits=self.limits)
        self.handle_registry = handle_registry or InMemoryHandleRegistry()
        # The in-process fake worker only exists for the local backend; a deployed
        # gateway dispatches to the provider and must never render a stand-in result.
        if worker is None and backend is None:
            worker = FluxWorker(provider=self.provider.value, limits=self.limits)
        self.worker = worker
        self.auth_validator = auth_validator
        self.proxy_token_id = proxy_token_id
        self.proxy_token_secret = proxy_token_secret
        self.bearer_token = bearer_token
        self.auto_execute = auto_execute
        # Deployed gateways sit behind provider edge auth (Modal proxy auth, Beam
        # authorized endpoints): requests without valid credentials never reach this
        # code, and the gateway holds no copy of those credentials to compare against.
        self.trust_edge_auth = trust_edge_auth
        self.backend = backend or LocalJobBackend(
            provider=self.provider.value,
            registry=self.handle_registry,
            worker=self.worker,
            limits=self.limits,
            auto_execute=auto_execute,
        )

    def check_auth(self, headers: Dict[str, str]) -> bool:
        """Authenticate incoming request according to provider configuration.

        Fails closed when credentials / auth validator are absent.
        """
        if self.trust_edge_auth:
            return True

        # Custom validator takes precedence if provided
        if self.auth_validator is not None:
            return self.auth_validator(headers)

        # Normalize header keys to lowercase
        norm_headers = {k.lower(): v for k, v in headers.items()}

        if self.provider == CloudProvider.MODAL:
            # Must fail closed if no proxy credentials configured
            if not self.proxy_token_id or not self.proxy_token_secret:
                return False
            req_key = norm_headers.get("modal-key", "")
            req_secret = norm_headers.get("modal-secret", "")
            if not req_key or not req_secret:
                return False
            return hmac.compare_digest(req_key, self.proxy_token_id) and hmac.compare_digest(req_secret, self.proxy_token_secret)

        elif self.provider == CloudProvider.BEAM:
            # Must fail closed if no bearer token configured
            if not self.bearer_token:
                return False
            auth_val = norm_headers.get("authorization", "")
            if not auth_val:
                return False
            expected = f"Bearer {self.bearer_token}"
            return hmac.compare_digest(auth_val, expected)

        return False

    def handle_http_request(
        self,
        method: str,
        raw_path: str,
        headers: Dict[str, str],
        body: bytes,
    ) -> Tuple[int, Dict[str, str], bytes]:
        """Dispatch HTTP request and return (status_code, response_headers, body_bytes)."""
        method = method.upper()
        # Strip query string and normalize path
        path = raw_path.split("?", 1)[0].rstrip("/")
        if not path:
            path = "/"

        # Normalize path prefixes: strip leading /mc/v1 if present
        norm_path = path
        if norm_path.startswith("/mc/v1"):
            norm_path = norm_path[6:]
            if not norm_path:
                norm_path = "/"

        # 1. Enforce Authentication on all /mc/v1 routes
        if not self.check_auth(headers):
            if path.startswith("/mc/analysis/v1/"):
                return self._analysis_error(401, "unauthorized", "Authentication failed: missing or invalid credentials")
            if path.startswith("/mc/denoise/v1/"):
                return self._denoise_error(401, "unauthorized", "Authentication failed: missing or invalid credentials")
            err_body = {
                "error_code": "unauthorized",
                "message": "Authentication failed: missing or invalid credentials",
            }
            return (
                401,
                {"Content-Type": "application/json", "X-Content-Type-Options": "nosniff"},
                json.dumps(err_body).encode("utf-8"),
            )

        # 2. Route Dispatch
        try:
            if path == "/mc/analysis/v1/capabilities" and method == "GET":
                if self.analysis_worker is None:
                    return self._analysis_error(503, "capability_unavailable", "Analysis is not configured")
                try:
                    capabilities = self.analysis_worker.capabilities()
                except OSError:
                    return self._analysis_error(503, "capability_unavailable", "Analysis graphs unavailable")
                return 200, {"Content-Type": "application/json"}, json.dumps(capabilities).encode("utf-8")

            if path == "/mc/analysis/v1/analyze" and method == "POST":
                return self._route_analysis(headers, body)

            if path == "/mc/analysis/v1/jobs" and method == "POST":
                return self._route_gpu_job_submit("analysis", headers, body)

            if path.startswith("/mc/analysis/v1/jobs/"):
                return self._route_gpu_job("analysis", method, path[len("/mc/analysis/v1/jobs/"):])

            if path == "/mc/analysis/v1/batches" and method == "POST":
                return self._route_gpu_job_submit("analysis_batch", headers, body)

            if path.startswith("/mc/analysis/v1/batches/"):
                return self._route_gpu_job("analysis_batch", method, path[len("/mc/analysis/v1/batches/"):])

            if path == "/mc/denoise/v1/capabilities" and method == "GET":
                if self.denoise_worker is None:
                    return self._denoise_error(503, "capability_unavailable", "Denoise is not configured")
                try:
                    capabilities = self.denoise_worker.capabilities()
                except OSError:
                    return self._denoise_error(503, "capability_unavailable", "Denoise models unavailable")
                return 200, {"Content-Type": "application/json"}, json.dumps(capabilities).encode("utf-8")

            if path == "/mc/denoise/v1/page" and method == "POST":
                return self._route_denoise(headers, body)

            if path == "/mc/denoise/v1/jobs" and method == "POST":
                return self._route_gpu_job_submit("denoise", headers, body)

            if path.startswith("/mc/denoise/v1/jobs/"):
                return self._route_gpu_job("denoise", method, path[len("/mc/denoise/v1/jobs/"):])

            if method == "GET" and norm_path == "/health":
                return self._route_health()

            if method == "GET" and norm_path == "/capabilities":
                operations = ["jobs.submit", "jobs.status", "jobs.result", "jobs.cancel"]
                if self.gpu_control is not None:
                    operations.extend(["gpu.status", "gpu.stop", "gpu.release_idle"])
                # Submit-then-poll tiles and pages (contract.py GPU_JOB_OPERATIONS). An
                # app that finds them missing keeps the synchronous routes.
                for kind, worker in (("analysis", self.analysis_worker), ("analysis_batch", self.analysis_worker),
                                     ("denoise", self.denoise_worker)):
                    if self._serves_gpu_jobs(worker, kind):
                        operations.append(GPU_JOB_OPERATIONS[kind])
                advertised: Dict[str, Any] = {
                    "protocol_version": PROTOCOL_VERSION,
                    "provider": self.provider.value,
                    "operations": operations,
                }
                if self.code_digest:
                    advertised["code_digest"] = self.code_digest
                return 200, {"Content-Type": "application/json"}, json.dumps(advertised).encode("utf-8")

            if method == "GET" and norm_path == "/model-info":
                return self._route_model_info()

            if method == "POST" and norm_path == "/jobs":
                return self._route_submit_job(headers, body)

            if method == "GET" and norm_path.startswith("/jobs/"):
                segments = [s for s in norm_path[6:].split("/") if s]
                if len(segments) == 1:
                    handle = segments[0]
                    return self._route_job_status(handle)
                elif len(segments) == 2 and segments[1] == "result":
                    handle = segments[0]
                    return self._route_job_result(handle, headers)

            if method == "POST" and norm_path.startswith("/jobs/") and norm_path.endswith("/cancel"):
                segments = [s for s in norm_path[6:].split("/") if s]
                if len(segments) == 2 and segments[1] == "cancel":
                    handle = segments[0]
                    return self._route_job_cancel(handle)

            if method == "POST" and norm_path == "/warmup":
                return self._route_warmup()

            if method == "GET" and norm_path == "/gpu":
                return self._route_gpu_status()

            if method == "POST" and norm_path == "/gpu/stop":
                return self._route_gpu_stop(body)

            # 404 Route Not Found
            not_found = {
                "error_code": "not_found",
                "message": f"Route '{path}' with method '{method}' not found",
            }
            return (
                404,
                {"Content-Type": "application/json"},
                json.dumps(not_found).encode("utf-8"),
            )

        except Exception as exc:
            if path.startswith("/mc/analysis/v1/"):
                logger.error("Internal analysis gateway error: %s", redact_text(str(exc)))
                return self._analysis_error(500, "inference_failed", "Analysis request failed")
            if path.startswith("/mc/denoise/v1/"):
                logger.error("Internal denoise gateway error: %s", redact_text(str(exc)))
                return self._denoise_error(500, "inference_failed", "Denoise request failed")
            err_msg = redact_text(str(exc))
            logger.error("Internal gateway error: %s", err_msg)
            err_payload = {
                "error_code": "internal_error",
                "message": f"Internal gateway error: {err_msg}",
            }
            return (
                500,
                {"Content-Type": "application/json"},
                json.dumps(err_payload).encode("utf-8"),
            )

    def _analysis_error(self, status: int, code: str, message: str, digest: Optional[str] = None) -> Tuple[int, Dict[str, str], bytes]:
        payload = {"protocol_version": ANALYSIS_VERSION, "error_code": code, "message": message,
                   "enqueued": False, "request_digest": digest}
        return status, {"Content-Type": "application/json"}, json.dumps(payload).encode("utf-8")

    @staticmethod
    def _analysis_digest(metadata: Any) -> Optional[str]:
        digest = metadata.get("request_digest") if type(metadata) is dict else None
        if type(digest) is str and len(digest) == 64 and all(c in "0123456789abcdef" for c in digest):
            return digest
        return None

    def _parse_analysis(
        self, headers: Dict[str, str], body: bytes, with_attempt: bool,
    ) -> Union[Tuple[int, Dict[str, str], bytes], Tuple[Dict[str, Any], bytes, Optional[str]]]:
        """An analysis body as (metadata, tile PNG, attempt id), or the error answer.

        The synchronous route takes exactly {metadata, tile_png_b64}; the job route
        adds the client's `attempt_id` (contract.py `valid_gpu_job_attempt`)."""
        content_type = {key.lower(): value for key, value in headers.items()}.get("content-type", "")
        if len(body) > 8_000_000:
            return self._analysis_error(413, "payload_too_large", "Analysis JSON must be bounded")
        if content_type.split(";", 1)[0].strip().lower() != "application/json":
            return self._analysis_error(400, "invalid_request", "Analysis Content-Type must be application/json")
        metadata = None
        keys = {"metadata", "tile_png_b64", "attempt_id"} if with_attempt else {"metadata", "tile_png_b64"}
        try:
            payload = json.loads(body)
            if type(payload) is not dict or set(payload) != keys:
                raise ContractValidationError("invalid analysis envelope")
            metadata = payload["metadata"]
            if type(payload["tile_png_b64"]) is not str:
                raise ContractValidationError("invalid analysis tile encoding")
            attempt_id = payload.get("attempt_id")
            if with_attempt and not valid_gpu_job_attempt(attempt_id):
                raise ContractValidationError("invalid attempt id")
            tile_png = base64.b64decode(payload["tile_png_b64"], validate=True)
            validate_analysis_request(metadata, tile_png)
        except (ValueError, TypeError, ContractValidationError):
            return self._analysis_error(400, "invalid_request", "Invalid analysis request", self._analysis_digest(metadata))
        except Exception as exc:
            logger.error("Analysis request failed: %s", redact_text(str(exc)))
            return self._analysis_error(500, "inference_failed", "Analysis inference failed", self._analysis_digest(metadata))
        return metadata, tile_png, attempt_id

    def _parse_analysis_batch(
        self, headers: Dict[str, str], body: bytes,
    ) -> Union[Tuple[int, Dict[str, str], bytes], Tuple[Dict[str, Any], list, str]]:
        """A batch body {metadata, tiles_png_b64, attempt_id} as (metadata, its
        (request, tile PNG) pairs, attempt id), or the error answer."""
        content_type = {key.lower(): value for key, value in headers.items()}.get("content-type", "")
        if len(body) > ANALYSIS_BATCH_MAX_BODY_BYTES:
            return self._analysis_error(413, "payload_too_large", "Analysis JSON must be bounded")
        if content_type.split(";", 1)[0].strip().lower() != "application/json":
            return self._analysis_error(400, "invalid_request", "Analysis Content-Type must be application/json")
        metadata = None
        try:
            payload = json.loads(body)
            if type(payload) is not dict or set(payload) != {"metadata", "tiles_png_b64", "attempt_id"}:
                raise ContractValidationError("invalid analysis batch envelope")
            metadata = payload["metadata"]
            encoded = payload["tiles_png_b64"]
            if type(encoded) is not list or any(type(tile) is not str for tile in encoded):
                raise ContractValidationError("invalid analysis tile encoding")
            attempt_id = payload["attempt_id"]
            if not valid_gpu_job_attempt(attempt_id):
                raise ContractValidationError("invalid attempt id")
            pairs = validate_analysis_batch(metadata, [base64.b64decode(tile, validate=True) for tile in encoded])
        except (ValueError, TypeError, ContractValidationError):
            return self._analysis_error(400, "invalid_request", "Invalid analysis request", self._analysis_digest(metadata))
        except Exception as exc:
            logger.error("Analysis batch request failed: %s", redact_text(str(exc)))
            return self._analysis_error(500, "inference_failed", "Analysis inference failed", self._analysis_digest(metadata))
        return metadata, pairs, attempt_id

    def _route_analysis(self, headers: Dict[str, str], body: bytes) -> Tuple[int, Dict[str, str], bytes]:
        if self.analysis_worker is None:
            return self._analysis_error(503, "capability_unavailable", "Analysis is not configured")
        parsed = self._parse_analysis(headers, body, with_attempt=False)
        if type(parsed[0]) is int:
            return parsed  # type: ignore[return-value]
        metadata, tile_png, _ = parsed  # type: ignore[misc]

        if not self._gpu_may_dispatch("analysis"):
            return self._analysis_error(503, "capability_unavailable", "The GPU was just stopped", self._analysis_digest(metadata))
        try:
            from deploy.cloud.common.jobs import execute_analysis_job
            result = execute_analysis_job(self.analysis_worker, metadata, tile_png)
            response = self._analysis_result_json(result)
            if len(response) > ANALYSIS_MAX_RESPONSE_BYTES:
                return self._analysis_error(500, "inference_failed", "Analysis response exceeded limit", metadata["request_digest"])
            return 200, {"Content-Type": "application/json"}, response
        except AnalysisUnavailable:
            return self._analysis_error(503, "capability_unavailable", "Selected analysis graphs are unavailable", self._analysis_digest(metadata))
        except GpuWaitExceeded:
            return self._analysis_error(504, "inference_failed", "No answer within the synchronous wait; poll the job routes", metadata["request_digest"])
        except Exception as exc:
            logger.error("Analysis inference failed: %s", redact_text(str(exc)))
            return self._analysis_error(500, "inference_failed", "Analysis inference failed", self._analysis_digest(metadata))

    def _denoise_error(self, status: int, code: str, message: str, digest: Optional[str] = None) -> Tuple[int, Dict[str, str], bytes]:
        from deploy.cloud.common.denoise import DENOISE_VERSION
        payload = {"protocol_version": DENOISE_VERSION, "error_code": code, "message": message,
                   "request_digest": digest}
        return status, {"Content-Type": "application/json"}, json.dumps(payload).encode("utf-8")

    def _parse_denoise(
        self, headers: Dict[str, str], body: bytes, with_attempt: bool,
    ) -> Union[Tuple[int, Dict[str, str], bytes], Tuple[Dict[str, Any], bytes, Optional[str]]]:
        """A denoise body as (metadata, page PNG, attempt id), or the error answer.

        The synchronous route takes exactly {metadata, page_png_b64}; the job route
        adds the client's `attempt_id` (contract.py `valid_gpu_job_attempt`)."""
        from deploy.cloud.common.denoise import DENOISE_MAX_BODY_BYTES, DenoiseError, validate_denoise_request
        content_type = {key.lower(): value for key, value in headers.items()}.get("content-type", "")
        if len(body) > DENOISE_MAX_BODY_BYTES:
            return self._denoise_error(413, "payload_too_large", "Denoise JSON must be bounded")
        if content_type.split(";", 1)[0].strip().lower() != "application/json":
            return self._denoise_error(400, "invalid_request", "Denoise Content-Type must be application/json")
        metadata = None
        keys = {"metadata", "page_png_b64", "attempt_id"} if with_attempt else {"metadata", "page_png_b64"}
        try:
            payload = json.loads(body)
            if type(payload) is not dict or set(payload) != keys:
                raise DenoiseError("invalid denoise envelope")
            metadata = payload["metadata"]
            if type(payload["page_png_b64"]) is not str:
                raise DenoiseError("invalid denoise page encoding")
            attempt_id = payload.get("attempt_id")
            if with_attempt and not valid_gpu_job_attempt(attempt_id):
                raise DenoiseError("invalid attempt id")
            page_png = base64.b64decode(payload["page_png_b64"], validate=True)
            validate_denoise_request(metadata, page_png)
        except DenoiseError as exc:
            code = exc.error_code if exc.error_code == "page_too_large" else "invalid_request"
            status = 413 if code == "page_too_large" else 400
            return self._denoise_error(status, code, str(exc), self._analysis_digest(metadata))
        except (ValueError, TypeError):
            return self._denoise_error(400, "invalid_request", "Invalid denoise request", self._analysis_digest(metadata))
        return metadata, page_png, attempt_id

    def _route_denoise(self, headers: Dict[str, str], body: bytes) -> Tuple[int, Dict[str, str], bytes]:
        """One whole page through a recipe, synchronously, on the analysis GPU.

        Body: {"metadata": {protocol_version, request_digest, recipe}, "page_png_b64"}.
        Answer: {protocol_version, request_digest, page_png_b64, info}.
        """
        from deploy.cloud.common.denoise import DENOISE_MAX_RESPONSE_BYTES, DenoiseError
        if self.denoise_worker is None:
            return self._denoise_error(503, "capability_unavailable", "Denoise is not configured")
        parsed = self._parse_denoise(headers, body, with_attempt=False)
        if type(parsed[0]) is int:
            return parsed  # type: ignore[return-value]
        metadata, page_png, _ = parsed  # type: ignore[misc]

        if not self._gpu_may_dispatch("analysis"):
            return self._denoise_error(503, "capability_unavailable", "The GPU was just stopped", metadata["request_digest"])
        try:
            result = self.denoise_worker.denoise(metadata, page_png)
            response = self._denoise_result_json(result)
            if len(response) > DENOISE_MAX_RESPONSE_BYTES:
                return self._denoise_error(500, "inference_failed", "Denoise response exceeded limit", metadata["request_digest"])
            return 200, {"Content-Type": "application/json"}, response
        except GpuWaitExceeded:
            return self._denoise_error(504, "inference_failed", "No answer within the synchronous wait; poll the job routes", metadata["request_digest"])
        except DenoiseError as exc:
            status = 503 if exc.error_code == "weights_missing" else 422 if exc.error_code == "unsupported_mode" else 500
            code = {"weights_missing": "capability_unavailable", "unsupported_mode": "unsupported_page"}.get(exc.error_code, "inference_failed")
            return self._denoise_error(status, code, str(exc), metadata["request_digest"])
        except Exception as exc:
            logger.error("Denoise inference failed: %s", redact_text(str(exc)))
            return self._denoise_error(500, "inference_failed", "Denoise inference failed", metadata["request_digest"])

    @staticmethod
    def _analysis_result_json(result: Dict[str, Any]) -> bytes:
        """A worker's analysis dict as the wire JSON: the mask PNG as base64."""
        result = dict(result)
        mask = result.pop("mask_png")
        result["mask_png_b64"] = base64.b64encode(mask).decode("ascii") if mask is not None else None
        return json.dumps(result, separators=(",", ":")).encode("utf-8")

    @staticmethod
    def _denoise_result_json(result: Dict[str, Any]) -> bytes:
        """A worker's denoise dict as the wire JSON: the page PNG as base64."""
        result = dict(result)
        result["page_png_b64"] = base64.b64encode(result.pop("page_png")).decode("ascii")
        return json.dumps(result, separators=(",", ":")).encode("utf-8")

    @staticmethod
    def _analysis_batch_result_json(result: Dict[str, Any]) -> bytes:
        """A worker's batch dict as the wire JSON: each result as a tile's answer."""
        head = json.dumps({"protocol_version": result["protocol_version"], "request_digest": result["request_digest"]},
                          separators=(",", ":")).encode("utf-8")
        results = b",".join(CloudGateway._analysis_result_json(item) for item in result["results"])
        return head[:-1] + b',"results":[' + results + b"]}"

    @staticmethod
    def _serves_gpu_jobs(worker: Any, kind: str = "") -> bool:
        submit = "submit_batch_job" if kind == "analysis_batch" else "submit_job"
        return worker is not None and all(
            callable(getattr(worker, name, None)) for name in (submit, "job_status", "cancel_job"))

    def _gpu_job_parts(self, kind: str) -> Tuple[Any, Callable[..., Tuple[int, Dict[str, str], bytes]], str]:
        if kind in ("analysis", "analysis_batch"):
            return self.analysis_worker, self._analysis_error, ANALYSIS_VERSION
        from deploy.cloud.common.denoise import DENOISE_VERSION
        return self.denoise_worker, self._denoise_error, DENOISE_VERSION

    @staticmethod
    def _gpu_job_answer(status: int, version: str, view: Dict[str, Any], result_json: Optional[bytes] = None) -> Tuple[int, Dict[str, str], bytes]:
        """The one envelope every job route answers with. `result` is the synchronous
        route's own answer, present only on a completed status."""
        head = json.dumps({
            "protocol_version": version,
            "request_digest": view["request_digest"],
            "handle": view["handle"],
            "status": view["status"],
            "error": view.get("error"),
        }, separators=(",", ":")).encode("utf-8")
        # Spliced rather than re-encoded, so a large page answer is not copied through
        # json.dumps twice.
        body = head[:-1] + b',"result":' + (result_json if result_json is not None else b"null") + b"}"
        return status, {"Content-Type": "application/json"}, body

    def _route_gpu_job_submit(self, kind: str, headers: Dict[str, str], body: bytes) -> Tuple[int, Dict[str, str], bytes]:
        """POST /mc/{analysis,denoise}/v1/jobs - spawn one tile or page and answer at once;
        POST /mc/analysis/v1/batches - the same for a batch of one page's tiles.

        Body: the synchronous route's body plus `attempt_id` (a batch: {metadata,
        tiles_png_b64, attempt_id}, contract.py `validate_analysis_batch`). Answer 202 with the job
        envelope (`_gpu_job_answer`, `result` null). The same attempt submitted again
        answers the job it already started and spawns nothing."""
        worker, error, version = self._gpu_job_parts(kind)
        if not self._serves_gpu_jobs(worker, kind):
            return error(503, "capability_unavailable", f"{kind.replace('_', ' ').capitalize()} jobs are not configured")
        if kind == "analysis_batch":
            parsed = self._parse_analysis_batch(headers, body)
        elif kind == "analysis":
            parsed = self._parse_analysis(headers, body, True)
        else:
            parsed = self._parse_denoise(headers, body, True)
        if type(parsed[0]) is int:
            return parsed  # type: ignore[return-value]
        metadata, payload, attempt_id = parsed  # type: ignore[misc]
        digest = metadata["request_digest"]
        if not self._gpu_may_dispatch("analysis"):
            return error(503, "capability_unavailable", "The GPU was just stopped", digest)
        try:
            if kind == "analysis_batch":
                view = worker.submit_batch_job(metadata, payload, attempt_id)
            else:
                view = worker.submit_job(metadata, payload, attempt_id)
        except AnalysisUnavailable:
            return error(503, "capability_unavailable", "The analysis GPU is not taking work now", digest)
        except GpuJobConflict:
            return error(400, "invalid_request", "This attempt already names another request", digest)
        except Exception as exc:
            logger.error("%s job submit failed: %s", kind, redact_text(str(exc)))
            return error(500, "inference_failed", f"{kind.replace('_', ' ').capitalize()} job submit failed", digest)
        return self._gpu_job_answer(202, version, view)

    def _route_gpu_job(self, kind: str, method: str, rest: str) -> Tuple[int, Dict[str, str], bytes]:
        """GET .../jobs/{handle} (state; the result once completed) and
        POST .../jobs/{handle}/cancel. Neither waits on the GPU."""
        worker, error, version = self._gpu_job_parts(kind)
        segments = [segment for segment in rest.split("/") if segment]
        cancel = method == "POST" and len(segments) == 2 and segments[1] == "cancel"
        status_read = method == "GET" and len(segments) == 1
        if not (cancel or status_read) or not valid_gpu_job_handle(kind, segments[0]):
            return error(404, "invalid_request", "No such job route")
        if not self._serves_gpu_jobs(worker, kind):
            return error(503, "capability_unavailable", f"{kind.replace('_', ' ').capitalize()} jobs are not configured")
        handle = segments[0]
        try:
            view = worker.cancel_job(handle) if cancel else worker.job_status(handle)
        except Exception as exc:
            # A store hiccup: the app asks again.
            logger.error("%s job %s failed: %s", kind, "cancel" if cancel else "status", redact_text(str(exc)))
            return error(503, "capability_unavailable", "Job state unavailable; ask again")
        if view is None:
            return error(404, "invalid_request", "No such job")
        result = view.get("result")
        if view["status"] != "completed" or result is None:
            return self._gpu_job_answer(200, version, view)
        if kind == "analysis":
            result_json = self._analysis_result_json(result)
            limit = ANALYSIS_MAX_RESPONSE_BYTES
        elif kind == "analysis_batch":
            result_json = self._analysis_batch_result_json(result)
            limit = ANALYSIS_BATCH_MAX_RESPONSE_BYTES
        else:
            from deploy.cloud.common.denoise import DENOISE_MAX_RESPONSE_BYTES
            result_json = self._denoise_result_json(result)
            limit = DENOISE_MAX_RESPONSE_BYTES
        if len(result_json) > limit:
            failed = {**view, "status": "failed",
                      "error": {"error_code": "inference_failed", "message": "Result exceeded the response limit"}}
            return self._gpu_job_answer(200, version, failed)
        return self._gpu_job_answer(200, version, view, result_json)

    def _route_health(self) -> Tuple[int, Dict[str, str], bytes]:
        """GET /health - Authenticated control reachability."""
        resp = HealthResponse(
            status="ok",
            provider=self.provider.value,
            protocol_version=PROTOCOL_VERSION,
        )
        return (
            200,
            {"Content-Type": "application/json"},
            json.dumps(resp.to_dict()).encode("utf-8"),
        )

    def _route_model_info(self) -> Tuple[int, Dict[str, str], bytes]:
        """GET /model-info - Pinned recipe and limits."""
        return (
            200,
            {"Content-Type": "application/json"},
            json.dumps(self.model_info.to_dict()).encode("utf-8"),
        )

    def _route_submit_job(
        self,
        headers: Dict[str, str],
        body: bytes,
    ) -> Tuple[int, Dict[str, str], bytes]:
        """POST /jobs - Multipart crop submission with structured validation."""
        norm_headers = {k.lower(): v for k, v in headers.items()}
        ct_header = norm_headers.get("content-type", "")

        # 1. Parse multipart form
        meta_str, img_bytes, hint_bytes, parse_err = parse_multipart_payload(
            ct_header, body, self.limits.max_multipart_bytes
        )

        if parse_err is not None or meta_str is None or img_bytes is None or hint_bytes is None:
            err_msg = parse_err or "Malformed multipart payload"
            if parse_err and "exceeds" in parse_err.lower():
                rejection = PreEnqueueRejectionResponse(
                    enqueued=False,
                    error_code="payload_too_large",
                    message=redact_text(err_msg),
                    job_id="unknown",
                    attempt_id="unknown",
                    request_digest="0000000000000000000000000000000000000000000000000000000000000000",
                    retryable=False,
                )
                return 413, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")

            err_code = "forbidden_fields" if (parse_err and ("unknown" in parse_err.lower() or "duplicate" in parse_err.lower())) else "invalid_image"
            rejection = PreEnqueueRejectionResponse(
                enqueued=False,
                error_code=err_code,
                message=redact_text(err_msg),
                job_id="unknown",
                attempt_id="unknown",
                request_digest="0000000000000000000000000000000000000000000000000000000000000000",
                retryable=False,
            )
            return 400, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")

        # 2. Parse request metadata
        try:
            meta_dict = json.loads(meta_str)
            job_meta = JobRequestMetadata.from_dict(meta_dict)
        except Exception as exc:
            rejection = PreEnqueueRejectionResponse(
                enqueued=False,
                error_code="forbidden_fields" if "forbidden" in str(exc).lower() else "unknown_protocol_version",
                message=redact_text(str(exc)),
                job_id="unknown",
                attempt_id="unknown",
                request_digest="0000000000000000000000000000000000000000000000000000000000000000",
                retryable=False,
            )
            return 400, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")

        # 3. Validate crop payload and geometry bounds
        try:
            validate_crop_payload(job_meta, img_bytes, hint_bytes, self.limits)
        except ContractValidationError as exc:
            err_msg = str(exc)
            err_msg_lower = err_msg.lower()
            if "sha-256" in err_msg_lower or "sha256" in err_msg_lower or "digest" in err_msg_lower or "hash" in err_msg_lower:
                err_code = "invalid_digest"
            elif "stride" in err_msg_lower or "dimension" in err_msg_lower or "width" in err_msg_lower or "height" in err_msg_lower:
                err_code = "invalid_dimensions"
            elif "pixel" in err_msg_lower or "limit" in err_msg_lower or "exceeds" in err_msg_lower or "too large" in err_msg_lower:
                err_code = "payload_too_large"
            elif "png" in err_msg_lower or "checksum" in err_msg_lower or "ihdr" in err_msg_lower or "idat" in err_msg_lower or "iend" in err_msg_lower or "filter" in err_msg_lower:
                err_code = "invalid_image"
            else:
                err_code = "invalid_image"

            rejection = PreEnqueueRejectionResponse(
                enqueued=False,
                error_code=err_code,
                message=redact_text(err_msg),
                job_id=job_meta.job_id,
                attempt_id=job_meta.attempt_id,
                request_digest=job_meta.request_digest,
                retryable=False,
            )
            return 400, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")

        # 4. Validate recipe compatibility against advertised model info
        try:
            validate_recipe_compatibility(job_meta, self.model_info)
        except ContractValidationError as exc:
            rejection = PreEnqueueRejectionResponse(
                enqueued=False,
                error_code="unsupported_recipe",
                message=redact_text(str(exc)),
                job_id=job_meta.job_id,
                attempt_id=job_meta.attempt_id,
                request_digest=job_meta.request_digest,
                retryable=False,
            )
            return 400, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")

        # 4b. A recipe that pins its sampling (the production recipe) only renders with
        # exactly those values; anything else would be a different image under the same id.
        mismatch = sampling_mismatch(job_meta)
        if mismatch:
            rejection = PreEnqueueRejectionResponse(
                enqueued=False,
                error_code="unsupported_recipe",
                message=redact_text(mismatch),
                job_id=job_meta.job_id,
                attempt_id=job_meta.attempt_id,
                request_digest=job_meta.request_digest,
                retryable=False,
            )
            return 400, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")

        # 5. Register (deduplicated on job_id + attempt_id) and hand to the worker
        if self.gpu_control is not None and not hasattr(self.gpu_control, "submit_render") and not self._gpu_may_dispatch("render"):
            rejection = PreEnqueueRejectionResponse(
                enqueued=False, error_code="service_unavailable_pre_queue",
                message="The GPU was just stopped", job_id=job_meta.job_id,
                attempt_id=job_meta.attempt_id, request_digest=job_meta.request_digest,
                retryable=True,
            )
            return 503, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")
        try:
            record = (self.gpu_control.submit_render(self.backend, job_meta, img_bytes, hint_bytes)
                      if self.gpu_control is not None and hasattr(self.gpu_control, "submit_render")
                      else self.backend.submit(job_meta, img_bytes, hint_bytes))
        except ContractValidationError as exc:
            rejection = PreEnqueueRejectionResponse(
                enqueued=False,
                error_code="invalid_digest",
                message=redact_text(str(exc)),
                job_id=job_meta.job_id,
                attempt_id=job_meta.attempt_id,
                request_digest=job_meta.request_digest,
                retryable=False,
            )
            return 409, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")
        except PreEnqueueError as exc:
            rejection = PreEnqueueRejectionResponse(
                enqueued=False,
                error_code=exc.error_code,
                message=redact_text(str(exc)) or exc.error_code,
                job_id=job_meta.job_id,
                attempt_id=job_meta.attempt_id,
                request_digest=job_meta.request_digest,
                retryable=exc.retryable,
            )
            return exc.http_status, {"Content-Type": "application/json"}, json.dumps(rejection.to_dict()).encode("utf-8")
        except SubmissionUnknownError:
            err = TypedError(error_code="submission_unknown", message="The GPU may have accepted this attempt; do not retry it")
            return 503, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")

        # 6. Return 202 Accepted reporting pending per contract (status endpoint holds actual state)
        accepted = JobAcceptedResponse(
            handle=record.app_handle,
            status=JobExecutionStatus.PENDING.value,
            job_id=job_meta.job_id,
            attempt_id=job_meta.attempt_id,
            request_digest=job_meta.request_digest,
            recipe_id=job_meta.recipe.recipe_id,
            preprocessing_version=job_meta.recipe.preprocessing_version,
            model_id=job_meta.recipe.model_id,
            model_revision=job_meta.recipe.model_revision,
            native_mask_conditioning=False,
        )
        return 202, {"Content-Type": "application/json"}, json.dumps(accepted.to_dict()).encode("utf-8")

    def _route_job_status(self, handle: str) -> Tuple[int, Dict[str, str], bytes]:
        """GET /jobs/{handle} - Status polling accepting only opaque app handles."""
        record = self.backend.get(handle)
        if not record:
            err = TypedError(error_code="not_found", message=f"Job handle '{handle}' not found")
            return 404, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")

        status_resp = JobStatusResponse(
            handle=record.app_handle,
            job_id=record.job_id,
            attempt_id=record.attempt_id,
            request_digest=record.request_digest,
            recipe_id=record.recipe.recipe_id,
            preprocessing_version=record.recipe.preprocessing_version,
            model_id=record.recipe.model_id,
            model_revision=record.recipe.model_revision,
            native_mask_conditioning=False,
            status=record.status.value,
            reported_cost_usd=record.reported_cost_usd,
            error=record.error,
            result_digest=record.result_digest,
            result_bytes=record.result_bytes,
        )
        return 200, {"Content-Type": "application/json"}, json.dumps(status_resp.to_dict()).encode("utf-8")

    @staticmethod
    def _range_start(headers: Optional[Dict[str, str]]) -> Optional[int]:
        """The N of a `Range: bytes=N-` header, the one form the app sends; else None."""
        for name, value in (headers or {}).items():
            if name.lower() == "range":
                first = value.strip().removeprefix("bytes=").removesuffix("-")
                if value.strip() == f"bytes={first}-" and first.isascii() and first.isdigit() and len(first) <= 12:
                    return int(first)
        return None

    def _route_job_result(self, handle: str, headers: Optional[Dict[str, str]] = None) -> Tuple[int, Dict[str, str], bytes]:
        """GET /jobs/{handle}/result - Validated result download accepting only opaque app handles.

        With `Range: bytes=N-` it answers 206 and the bytes from N on, so an app
        whose download stopped on a slow link fetches only what is missing.
        """
        try:
            record, result_png = self.backend.result_png(handle)
        except ResultUnavailableError as exc:
            err = TypedError(
                error_code="result_unavailable",
                message=redact_text(f"Result for '{handle}' is not readable right now: {exc}")[:1024],
            )
            return 503, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")
        if not record:
            err = TypedError(error_code="not_found", message=f"Job handle '{handle}' not found")
            return 404, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")

        if record.status != JobExecutionStatus.COMPLETED or result_png is None:
            err = TypedError(
                error_code="job_not_ready",
                message=f"Job '{handle}' is not completed (status: {record.status.value})",
            )
            return 409, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")

        resp_headers: Dict[str, str] = {
            "Content-Type": "image/png",
            "X-MC-Result-Digest": record.result_digest or "",
        }
        if record.reported_cost_usd is not None:
            resp_headers["X-MC-Reported-Cost-Usd"] = str(record.reported_cost_usd)
        start = self._range_start(headers)
        if start is None:
            return 200, resp_headers, result_png
        total = len(result_png)
        if start >= total:
            resp_headers["Content-Range"] = f"bytes */{total}"
            err = TypedError(error_code="invalid_request", message="Range starts past the end of the result")
            return 416, {**resp_headers, "Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")
        resp_headers["Content-Range"] = f"bytes {start}-{total - 1}/{total}"
        return 206, resp_headers, result_png[start:]

    def _route_job_cancel(self, handle: str) -> Tuple[int, Dict[str, str], bytes]:
        """POST /jobs/{handle}/cancel - Request cancellation accepting only opaque app handles."""
        # The backend flags cancel atomically and refuses terminal jobs without mutating them.
        updated_record, accepted = self.backend.cancel(handle)
        if updated_record is None:
            err = TypedError(error_code="not_found", message=f"Job handle '{handle}' not found")
            return 404, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")

        if not accepted:
            err = TypedError(
                error_code="job_terminal",
                message=f"Cannot cancel job '{handle}' in terminal state '{updated_record.status.value}'",
            )
            return 409, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")

        cancel_resp = JobCancelResponse(
            handle=updated_record.app_handle,
            job_id=updated_record.job_id,
            attempt_id=updated_record.attempt_id,
            request_digest=updated_record.request_digest,
            status="cancel_requested",
            acknowledged=True,
        )
        return 200, {"Content-Type": "application/json"}, json.dumps(cancel_resp.to_dict()).encode("utf-8")

    def _gpu_may_dispatch(self, role: str) -> bool:
        if self.gpu_control is None:
            return True
        try:
            return bool(self.gpu_control.before_dispatch(role))
        except Exception as exc:
            logger.error("GPU dispatch check failed: %s", redact_text(str(exc)))
            return False

    def _route_gpu_status(self) -> Tuple[int, Dict[str, str], bytes]:
        """GET /gpu - Which GPU containers are up, read from their heartbeats. Never starts one."""
        if self.gpu_control is None:
            payload: Dict[str, Any] = {"provider": self.provider.value, "supported": False, "now": time.time(),
                                       "containers": [], "list_price_usd_per_hour": {}}
        else:
            payload = self.gpu_control.status()
        validate_gpu_status_response(payload)
        return 200, {"Content-Type": "application/json"}, json.dumps(payload).encode("utf-8")

    def _route_gpu_stop(self, body: bytes) -> Tuple[int, Dict[str, str], bytes]:
        """POST /gpu/stop - Stop one role's GPU container, or both. Idempotent.

        The body is empty or `{}` for both, or `{"role": "render"|"analysis"}`, with an
        optional `"idle_only": true` that only releases an idle container.
        """
        if self.gpu_control is None:
            err = TypedError(error_code="unsupported", message="This deployment cannot stop its GPU")
            return 501, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")
        try:
            request = json.loads(body) if body.strip() else {}
            validate_gpu_stop_request(request)
        except (ValueError, ContractValidationError):
            err = TypedError(error_code="invalid_request", message="Stop takes an empty body or a known role")
            return 400, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")
        try:
            payload = self.gpu_control.stop(request.get("role"), idle_only=request.get("idle_only") is True)
        except StopUncertainError:
            err = TypedError(error_code="stop_uncertain", message="GPU stop could not be confirmed; retry stop")
            return 503, {"Content-Type": "application/json"}, json.dumps(err.to_dict()).encode("utf-8")
        validate_gpu_stop_response(payload)
        return 200, {"Content-Type": "application/json"}, json.dumps(payload).encode("utf-8")

    def _route_warmup(self) -> Tuple[int, Dict[str, str], bytes]:
        """POST /warmup - Explicit worker readiness probe."""
        resp = WarmupResponse(
            status="ok",
            provider=self.provider.value,
            protocol_version=PROTOCOL_VERSION,
            worker_state=self.backend.warmup(),
        )
        return 200, {"Content-Type": "application/json"}, json.dumps(resp.to_dict()).encode("utf-8")

    def execute_worker_job(
        self,
        record: JobRecord,
        img_bytes: bytes,
        hint_bytes: bytes,
        internal_storage_ref: Optional[str] = None,
    ) -> bool:
        """Execute worker inference, validate result buffer against contract, and persist result or typed failure."""
        return execute_worker_job(
            handle_registry=self.handle_registry,
            worker=self.worker,
            record=record,
            img_bytes=img_bytes,
            hint_bytes=hint_bytes,
            limits=self.limits,
            internal_storage_ref=internal_storage_ref,
            auto_execute=self.auto_execute,
        )

    def as_wsgi_app(self) -> Callable:
        """Return a WSGI application callable for serving with standard WSGI servers."""
        def wsgi_app(environ: Dict[str, Any], start_response: Callable) -> List[bytes]:
            method = environ.get("REQUEST_METHOD", "GET")
            path = environ.get("PATH_INFO", "/")
            query = environ.get("QUERY_STRING", "")
            if query:
                path = f"{path}?{query}"

            headers: Dict[str, str] = {}
            for k, v in environ.items():
                if k.startswith("HTTP_"):
                    header_name = k[5:].replace("_", "-").title()
                    headers[header_name] = str(v)
                elif k in ("CONTENT_TYPE", "CONTENT_LENGTH"):
                    header_name = k.replace("_", "-").title()
                    headers[header_name] = str(v)

            content_length_str = environ.get("CONTENT_LENGTH", "")
            content_length = int(content_length_str) if content_length_str.isdigit() else 0
            if content_length > self.limits.max_multipart_bytes:
                err_payload = json.dumps({
                    "error_code": "payload_too_large",
                    "message": f"Payload size {content_length} exceeds maximum limit {self.limits.max_multipart_bytes}",
                }).encode("utf-8")
                start_response("413 Payload Too Large", [("Content-Type", "application/json"), ("X-Content-Type-Options", "nosniff")])
                return [err_payload]

            body = bytearray()
            input_stream = environ.get("wsgi.input")
            if input_stream:
                if content_length > 0:
                    remaining = content_length
                    chunk_size = 65536
                    while remaining > 0:
                        read_len = min(remaining, chunk_size)
                        chunk = input_stream.read(read_len)
                        if not chunk:
                            break
                        body.extend(chunk)
                        remaining -= len(chunk)
                        if len(body) > self.limits.max_multipart_bytes:
                            err_payload = json.dumps({
                                "error_code": "payload_too_large",
                                "message": f"Payload size exceeds maximum limit {self.limits.max_multipart_bytes}",
                            }).encode("utf-8")
                            start_response("413 Payload Too Large", [("Content-Type", "application/json"), ("X-Content-Type-Options", "nosniff")])
                            return [err_payload]
                elif method in ("POST", "PUT", "PATCH") and not content_length_str:
                    chunk_size = 65536
                    while True:
                        chunk = input_stream.read(chunk_size)
                        if not chunk:
                            break
                        body.extend(chunk)
                        if len(body) > self.limits.max_multipart_bytes:
                            err_payload = json.dumps({
                                "error_code": "payload_too_large",
                                "message": f"Payload size exceeds maximum limit {self.limits.max_multipart_bytes}",
                            }).encode("utf-8")
                            start_response("413 Payload Too Large", [("Content-Type", "application/json"), ("X-Content-Type-Options", "nosniff")])
                            return [err_payload]

            status_code, resp_headers, resp_body = self.handle_http_request(
                method=method,
                raw_path=path,
                headers=headers,
                body=bytes(body),
            )

            status_line = f"{status_code} OK"
            if status_code == 202:
                status_line = "202 Accepted"
            elif status_code == 206:
                status_line = "206 Partial Content"
            elif status_code == 416:
                status_line = "416 Range Not Satisfiable"
            elif status_code == 400:
                status_line = "400 Bad Request"
            elif status_code == 401:
                status_line = "401 Unauthorized"
            elif status_code == 404:
                status_line = "404 Not Found"
            elif status_code == 409:
                status_line = "409 Conflict"
            elif status_code == 413:
                status_line = "413 Payload Too Large"
            elif status_code == 500:
                status_line = "500 Internal Server Error"
            elif status_code == 501:
                status_line = "501 Not Implemented"
            elif status_code == 503:
                status_line = "503 Service Unavailable"
            elif status_code == 504:
                status_line = "504 Gateway Timeout"

            start_response(status_line, list(resp_headers.items()))
            return [resp_body]

        return wsgi_app

    def as_asgi_app(self) -> Callable:
        """Return an ASGI 3.0 application callable for serving with ASGI servers."""
        async def asgi_app(scope: Dict[str, Any], receive: Callable, send: Callable) -> None:
            if scope["type"] == "http":
                method = scope["method"]
                path = scope["path"]
                query_string = scope.get("query_string", b"").decode("utf-8")
                if query_string:
                    path = f"{path}?{query_string}"

                headers = {}
                content_length = None
                for raw_k, raw_v in scope.get("headers", []):
                    k_str = raw_k.decode("latin1").title()
                    v_str = raw_v.decode("latin1")
                    headers[k_str] = v_str
                    if k_str.lower() == "content-length" and v_str.isdigit():
                        content_length = int(v_str)

                if content_length is not None and content_length > self.limits.max_multipart_bytes:
                    if path.startswith("/mc/analysis/v1/"):
                        status, _, err_payload = self._analysis_error(413, "payload_too_large", "Analysis JSON must be bounded")
                        await send({"type": "http.response.start", "status": status,
                                    "headers": [(b"content-type", b"application/json"), (b"x-content-type-options", b"nosniff")]})
                        await send({"type": "http.response.body", "body": err_payload})
                        return
                    err_payload = json.dumps({
                        "error_code": "payload_too_large",
                        "message": f"Payload size {content_length} exceeds maximum limit {self.limits.max_multipart_bytes}",
                    }).encode("utf-8")
                    await send({
                        "type": "http.response.start",
                        "status": 413,
                        "headers": [(b"content-type", b"application/json"), (b"x-content-type-options", b"nosniff")],
                    })
                    await send({
                        "type": "http.response.body",
                        "body": err_payload,
                    })
                    return

                body = bytearray()
                more_body = True
                while more_body:
                    message = await receive()
                    chunk = message.get("body", b"")
                    body.extend(chunk)
                    if len(body) > self.limits.max_multipart_bytes:
                        if path.startswith("/mc/analysis/v1/"):
                            status, _, err_payload = self._analysis_error(413, "payload_too_large", "Analysis JSON must be bounded")
                            await send({"type": "http.response.start", "status": status,
                                        "headers": [(b"content-type", b"application/json"), (b"x-content-type-options", b"nosniff")]})
                            await send({"type": "http.response.body", "body": err_payload})
                            return
                        err_payload = json.dumps({
                            "error_code": "payload_too_large",
                            "message": f"Payload size exceeds maximum limit of {self.limits.max_multipart_bytes} bytes",
                        }).encode("utf-8")
                        await send({
                            "type": "http.response.start",
                            "status": 413,
                            "headers": [(b"content-type", b"application/json"), (b"x-content-type-options", b"nosniff")],
                        })
                        await send({
                            "type": "http.response.body",
                            "body": err_payload,
                        })
                        return
                    more_body = message.get("more_body", False)

                # Backends make blocking provider calls; a worker thread keeps the event
                # loop free for concurrent status polls.
                status_code, resp_headers, resp_body = await asyncio.to_thread(
                    self.handle_http_request,
                    method,
                    path,
                    headers,
                    bytes(body),
                )

                asgi_headers = [
                    (k.encode("latin1"), v.encode("latin1")) for k, v in resp_headers.items()
                ]

                await send({
                    "type": "http.response.start",
                    "status": status_code,
                    "headers": asgi_headers,
                })
                await send({
                    "type": "http.response.body",
                    "body": resp_body,
                })
            elif scope["type"] == "lifespan":
                while True:
                    message = await receive()
                    if message["type"] == "lifespan.startup":
                        await send({"type": "lifespan.startup.complete"})
                    elif message["type"] == "lifespan.shutdown":
                        await send({"type": "lifespan.shutdown.complete"})
                        return
        return asgi_app
