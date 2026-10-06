"""Provider-neutral cloud wire contract schemas, validation, and canonical digest.

This module defines the offline wire protocol (/mc/v1) for Manga Cleaner cloud diffusion
inference across both Modal and Beam backends.
"""

from __future__ import annotations

import dataclasses
import base64
import binascii
import hashlib
import json
import math
import struct
import zlib
from dataclasses import dataclass
from enum import Enum
from typing import Any, Dict, List, Optional, Set, Union

PROTOCOL_VERSION: str = "1.0.0"
LATENT_STRIDE: int = 16

# Forbidden keys in crop request payloads that would violate crop-only isolation
# or leak page-level coordinates, region IDs, or filesystem paths.
FORBIDDEN_PAYLOAD_FIELDS: Set[str] = {
    "x",
    "y",
    "page_x",
    "page_y",
    "strip_rect",
    "on_page",
    "region_id",
    "region_ids",
    "file_path",
    "filepath",
    "path",
    "page_path",
    "project_path",
    "canvas_x",
    "canvas_y",
    "bounds",
}

ALLOWLISTED_REJECTION_CODES: Set[str] = {
    "unsupported_recipe",
    "unsupported_model",
    "invalid_dimensions",
    "payload_too_large",
    "invalid_digest",
    "unknown_protocol_version",
    "forbidden_fields",
    "invalid_image",
    "weights_missing",
    "weights_corrupt",
    "rate_limited_pre_queue",
    "service_unavailable_pre_queue",
}

RETRYABLE_PRE_ENQUEUE_CODES: Set[str] = {
    "rate_limited_pre_queue",
    "service_unavailable_pre_queue",
}


class CloudProvider(str, Enum):
    BEAM = "beam"
    MODAL = "modal"


class JobExecutionStatus(str, Enum):
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class ClientDispatchState(str, Enum):
    """Client-side state machine tracking remote dispatch lifecycle."""
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    CANCEL_REQUESTED = "cancel_requested"
    SUBMISSION_UNKNOWN = "submission_unknown"


class ContractValidationError(Exception):
    """Raised when wire validation fails closed."""
    pass


def _check_exact_str(val: Any, name: str, min_len: int = 1, max_len: int = 128, allow_whitespace: bool = False) -> str:
    if type(val) is not str:
        raise ContractValidationError(f"Field '{name}' must be a str, got {type(val).__name__}")
    if not (min_len <= len(val) <= max_len):
        raise ContractValidationError(f"Field '{name}' length {len(val)} out of range [{min_len}..{max_len}]")
    if allow_whitespace:
        if not (val.isascii() and not any(ord(c) < 32 or ord(c) == 127 for c in val)):
            raise ContractValidationError(f"Field '{name}' contains non-ASCII or control characters: '{val}'")
    else:
        if not all(33 <= ord(c) <= 126 for c in val):
            raise ContractValidationError(f"Field '{name}' contains non-ASCII, control, or whitespace characters: '{val}'")
    return val


def _validate_lowercase_hex_hash(val: Any, name: str, expected_len: int = 64) -> str:
    if type(val) is not str:
        raise ContractValidationError(f"Field '{name}' must be a str, got {type(val).__name__}")
    if len(val) != expected_len or not all(c in "0123456789abcdef" for c in val):
        raise ContractValidationError(f"Field '{name}' must be a canonical lowercase {expected_len}-hex hash, got '{val}'")
    return val


def _check_exact_int(val: Any, name: str, min_val: Optional[int] = 0, max_val: Optional[int] = None) -> int:
    if type(val) is not int:  # type(True) is bool, not int
        raise ContractValidationError(f"Field '{name}' must be an int, got {type(val).__name__}")
    if min_val is not None and val < min_val:
        raise ContractValidationError(f"Field '{name}' value {val} below minimum {min_val}")
    if max_val is not None and val > max_val:
        raise ContractValidationError(f"Field '{name}' value {val} above maximum {max_val}")
    return val


def _check_exact_bool(val: Any, name: str) -> bool:
    if type(val) is not bool:
        raise ContractValidationError(f"Field '{name}' must be a bool, got {type(val).__name__}")
    return val


def _check_optional_float(val: Any, name: str) -> Optional[float]:
    if val is None:
        return None
    if type(val) not in (float, int) or type(val) is bool:
        raise ContractValidationError(f"Field '{name}' must be a float, got {type(val).__name__}")
    fval = float(val)
    if not math.isfinite(fval) or fval < 0.0:
        raise ContractValidationError(f"Field '{name}' must be finite non-negative float, got {fval}")
    return fval


def _validate_revision_identity(rev: str) -> None:
    if type(rev) is not str or len(rev) not in (40, 64) or not all(c in "0123456789abcdef" for c in rev):
        raise ContractValidationError(
            f"Revision '{rev}' is invalid; revision must be an explicit immutable 40 or 64 lowercase hex hash"
        )


def _assert_no_extra_keys(d: Dict[str, Any], allowed_keys: Set[str]) -> None:
    unknown = set(d.keys()) - allowed_keys
    if unknown:
        raise ContractValidationError(f"Unknown or unexpected fields present: {sorted(unknown)}")


def _assert_no_forbidden_fields(d: Any) -> None:
    if isinstance(d, dict):
        for k, v in d.items():
            if k in FORBIDDEN_PAYLOAD_FIELDS:
                raise ContractValidationError(f"Forbidden coordinate/path field detected in crop payload: '{k}'")
            _assert_no_forbidden_fields(v)
    elif isinstance(d, list):
        for item in d:
            _assert_no_forbidden_fields(item)


@dataclass(frozen=True)
class QwenEdit:
    target: str = "auto"
    description: str = ""

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> QwenEdit:
        if type(d) is not dict:
            raise ContractValidationError("qwen_edit must be a dict")
        _assert_no_extra_keys(d, {"target", "description"})
        target = _check_exact_str(d.get("target"), "target")
        description = d.get("description")
        if type(description) is not str:
            raise ContractValidationError("Qwen description must be text")
        if target not in {"auto", "dialogue", "sound_effect", "other"}:
            raise ContractValidationError("Invalid Qwen target")
        if len(description) > 500 or any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in description):
            raise ContractValidationError("Invalid Qwen description")
        return cls(target=target, description=description)


@dataclass(frozen=True)
class RenderRecipe:
    recipe_id: str
    preprocessing_version: str
    model_id: str
    model_revision: str
    native_mask_conditioning: bool
    qwen_edit: Optional[QwenEdit] = None

    def to_dict(self) -> Dict[str, Any]:
        result = {
            "recipe_id": self.recipe_id,
            "preprocessing_version": self.preprocessing_version,
            "model_id": self.model_id,
            "model_revision": self.model_revision,
            "native_mask_conditioning": self.native_mask_conditioning,
        }

        if self.qwen_edit is not None:
            result["qwen_edit"] = dataclasses.asdict(self.qwen_edit)
        return result

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> RenderRecipe:
        if type(d) is not dict:
            raise ContractValidationError("RenderRecipe must be a dict")
        _assert_no_extra_keys(d, {"recipe_id", "preprocessing_version", "model_id", "model_revision", "native_mask_conditioning", "qwen_edit"})
        recipe_id = _check_exact_str(d.get("recipe_id"), "recipe_id")
        preprocessing_version = _check_exact_str(d.get("preprocessing_version"), "preprocessing_version")
        model_id = _check_exact_str(d.get("model_id"), "model_id")
        model_revision = _check_exact_str(d.get("model_revision"), "model_revision")
        _validate_revision_identity(model_revision)
        native_mask = _check_exact_bool(d.get("native_mask_conditioning"), "native_mask_conditioning")
        edit = QwenEdit.from_dict(d["qwen_edit"]) if "qwen_edit" in d else None
        if edit is not None and (not model_id.startswith("Disty0/Qwen-Image-Edit") or recipe_id != "mc-qwen-image-edit-2511-v4"):
            raise ContractValidationError("This recipe does not support Qwen guidance")
        return cls(
            qwen_edit=edit,
            recipe_id=recipe_id,
            preprocessing_version=preprocessing_version,
            model_id=model_id,
            model_revision=model_revision,
            native_mask_conditioning=native_mask,
        )


@dataclass(frozen=True)
class ServiceLimits:
    max_width: int
    max_height: int
    max_pixels: int
    max_png_bytes: int
    max_multipart_bytes: int
    worker_timeout_seconds: int

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> ServiceLimits:
        if type(d) is not dict:
            raise ContractValidationError("ServiceLimits must be a dict")
        _assert_no_extra_keys(d, {"max_width", "max_height", "max_pixels", "max_png_bytes", "max_multipart_bytes", "worker_timeout_seconds"})
        return cls(
            max_width=_check_exact_int(d.get("max_width"), "max_width", min_val=1),
            max_height=_check_exact_int(d.get("max_height"), "max_height", min_val=1),
            max_pixels=_check_exact_int(d.get("max_pixels"), "max_pixels", min_val=1),
            max_png_bytes=_check_exact_int(d.get("max_png_bytes"), "max_png_bytes", min_val=1),
            max_multipart_bytes=_check_exact_int(d.get("max_multipart_bytes"), "max_multipart_bytes", min_val=1),
            worker_timeout_seconds=_check_exact_int(d.get("worker_timeout_seconds"), "worker_timeout_seconds", min_val=1),
        )


def provisional_fixture_limits() -> ServiceLimits:
    """Provisional test-labelled fixture limits. (Production limits unmeasured until P5 GPU benchmarks)."""
    return ServiceLimits(
        max_width=2048,
        max_height=2048,
        max_pixels=4194304,
        max_png_bytes=16777216,
        max_multipart_bytes=33554432,
        worker_timeout_seconds=120,
    )


def validate_service_limits(limits: ServiceLimits) -> None:
    if limits.max_width <= 0:
        raise ContractValidationError("max_width must be positive")
    if limits.max_height <= 0:
        raise ContractValidationError("max_height must be positive")
    if limits.max_pixels <= 0:
        raise ContractValidationError("max_pixels must be positive")
    if limits.max_png_bytes <= 0:
        raise ContractValidationError("max_png_bytes must be positive")
    if limits.max_multipart_bytes <= 0:
        raise ContractValidationError("max_multipart_bytes must be positive")
    if limits.worker_timeout_seconds <= 0:
        raise ContractValidationError("worker_timeout_seconds must be positive")


@dataclass(frozen=True)
class HealthResponse:
    status: str
    provider: str
    protocol_version: str

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> HealthResponse:
        if type(d) is not dict:
            raise ContractValidationError("HealthResponse must be a dict")
        _assert_no_extra_keys(d, {"status", "provider", "protocol_version"})
        status = _check_exact_str(d.get("status"), "status")
        provider = _check_exact_str(d.get("provider"), "provider")
        if provider not in (CloudProvider.BEAM.value, CloudProvider.MODAL.value):
            raise ContractValidationError(f"Unknown provider '{provider}'")
        proto = _check_exact_str(d.get("protocol_version"), "protocol_version")
        if proto != PROTOCOL_VERSION:
            raise ContractValidationError(f"Protocol version mismatch '{proto}'")
        return cls(status=status, provider=provider, protocol_version=proto)


def validate_health_response(resp: HealthResponse) -> None:
    if resp.protocol_version != PROTOCOL_VERSION:
        raise ContractValidationError(f"Protocol version mismatch '{resp.protocol_version}'")
    if resp.status != "ok":
        raise ContractValidationError(f"Health status not ok: '{resp.status}'")
    if resp.provider not in (CloudProvider.BEAM.value, CloudProvider.MODAL.value):
        raise ContractValidationError(f"Unknown provider '{resp.provider}'")


@dataclass(frozen=True)
class WarmupResponse:
    status: str
    provider: str
    protocol_version: str
    worker_state: str

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> WarmupResponse:
        if type(d) is not dict:
            raise ContractValidationError("WarmupResponse must be a dict")
        _assert_no_extra_keys(d, {"status", "provider", "protocol_version", "worker_state"})
        status = _check_exact_str(d.get("status"), "status")
        provider = _check_exact_str(d.get("provider"), "provider")
        if provider not in (CloudProvider.BEAM.value, CloudProvider.MODAL.value):
            raise ContractValidationError(f"Unknown provider '{provider}'")
        proto = _check_exact_str(d.get("protocol_version"), "protocol_version")
        if proto != PROTOCOL_VERSION:
            raise ContractValidationError(f"Protocol version mismatch '{proto}'")
        worker_state = _check_exact_str(d.get("worker_state"), "worker_state")
        return cls(status=status, provider=provider, protocol_version=proto, worker_state=worker_state)


def validate_warmup_response(resp: WarmupResponse) -> None:
    if resp.protocol_version != PROTOCOL_VERSION:
        raise ContractValidationError(f"Protocol version mismatch '{resp.protocol_version}'")
    if resp.status not in ("ok", "warmed", "ready"):
        raise ContractValidationError(f"Warmup status not valid: '{resp.status}'")
    if resp.provider not in (CloudProvider.BEAM.value, CloudProvider.MODAL.value):
        raise ContractValidationError(f"Unknown provider '{resp.provider}'")


# GET /mc/v1/gpu and POST /mc/v1/gpu/stop (see docs/cloud-api.md). One container per
# role at most, so a status lists at most len(GPU_ROLES) entries.
GPU_ROLES = ("render", "analysis")
GPU_STATES = ("starting", "idle", "busy")
_GPU_CONTAINER_KEYS = {"role", "gpu", "state", "started_at", "last_active_at", "idle_seconds", "scaledown_estimate_at"}


def _check_gpu_time(val: Any, name: str, optional: bool = False) -> None:
    if val is None and optional:
        return
    if isinstance(val, bool) or not isinstance(val, (int, float)) or not math.isfinite(float(val)) or val < 0:
        raise ContractValidationError(f"'{name}' must be a finite non-negative number")


def validate_gpu_status_response(d: Any) -> None:
    if type(d) is not dict:
        raise ContractValidationError("GpuStatusResponse must be a dict")
    _assert_no_extra_keys(d, {"provider", "supported", "now", "containers", "list_price_usd_per_hour"})
    if d.get("provider") not in (CloudProvider.BEAM.value, CloudProvider.MODAL.value):
        raise ContractValidationError("Unknown provider")
    _check_exact_bool(d.get("supported"), "supported")
    _check_gpu_time(d.get("now"), "now")
    containers = d.get("containers")
    if type(containers) is not list or len(containers) > len(GPU_ROLES):
        raise ContractValidationError("'containers' must be a list of at most one entry per role")
    if not d["supported"] and containers:
        raise ContractValidationError("An unsupported deployment reports no containers")
    roles = set()
    for entry in containers:
        if type(entry) is not dict:
            raise ContractValidationError("GPU container must be a dict")
        _assert_no_extra_keys(entry, _GPU_CONTAINER_KEYS)
        if entry.get("role") not in GPU_ROLES or entry["role"] in roles:
            raise ContractValidationError("GPU container role unknown or repeated")
        roles.add(entry["role"])
        if entry.get("state") not in GPU_STATES:
            raise ContractValidationError("GPU container state unknown")
        _check_exact_str(entry.get("gpu"), "gpu", max_len=32)
        _check_gpu_time(entry.get("started_at"), "started_at")
        _check_gpu_time(entry.get("last_active_at"), "last_active_at")
        _check_exact_int(entry.get("idle_seconds"), "idle_seconds", min_val=1, max_val=86400)
        _check_gpu_time(entry.get("scaledown_estimate_at"), "scaledown_estimate_at", optional=True)
    prices = d.get("list_price_usd_per_hour")
    if type(prices) is not dict or len(prices) > 8:
        raise ContractValidationError("'list_price_usd_per_hour' must be a small dict")
    for name, price in prices.items():
        _check_exact_str(name, "gpu", max_len=32)
        _check_gpu_time(price, "list_price_usd_per_hour")


def validate_gpu_stop_request(d: Any) -> None:
    """`{}` stops both roles; `{"role": ...}` stops that one. `"idle_only": true`
    releases only an idle container and cancels nothing; absent or false is a plain stop."""
    if type(d) is not dict:
        raise ContractValidationError("GpuStopRequest must be a dict")
    _assert_no_extra_keys(d, {"role", "idle_only"})
    if "role" in d and d["role"] not in GPU_ROLES:
        raise ContractValidationError("Unknown GPU role")
    if "idle_only" in d:
        _check_exact_bool(d["idle_only"], "idle_only")


def validate_gpu_stop_response(d: Any) -> None:
    if type(d) is not dict:
        raise ContractValidationError("GpuStopResponse must be a dict")
    _assert_no_extra_keys(d, {"provider", "stopped", "cancelled_jobs"})
    if d.get("provider") not in (CloudProvider.BEAM.value, CloudProvider.MODAL.value):
        raise ContractValidationError("Unknown provider")
    stopped = d.get("stopped")
    if type(stopped) is not list or len(stopped) != len(set(stopped)) or any(role not in GPU_ROLES for role in stopped):
        raise ContractValidationError("'stopped' must list distinct known roles")
    _check_exact_int(d.get("cancelled_jobs"), "cancelled_jobs", min_val=0, max_val=10_000)



@dataclass(frozen=True)
class ModelInfoResponse:
    protocol_version: str
    provider: str
    model_id: str
    model_revision: str
    recipe_id: str
    preprocessing_version: str
    native_mask_conditioning: bool
    limits: ServiceLimits

    def to_dict(self) -> Dict[str, Any]:
        return {
            "protocol_version": self.protocol_version,
            "provider": self.provider,
            "model_id": self.model_id,
            "model_revision": self.model_revision,
            "recipe_id": self.recipe_id,
            "preprocessing_version": self.preprocessing_version,
            "native_mask_conditioning": self.native_mask_conditioning,
            "limits": self.limits.to_dict(),
        }

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> ModelInfoResponse:
        if type(d) is not dict:
            raise ContractValidationError("ModelInfoResponse must be a dict")
        _assert_no_extra_keys(d, {
            "protocol_version", "provider", "model_id", "model_revision",
            "recipe_id", "preprocessing_version", "native_mask_conditioning", "limits"
        })
        proto = _check_exact_str(d.get("protocol_version"), "protocol_version")
        if proto != PROTOCOL_VERSION:
            raise ContractValidationError(f"Unsupported protocol version: '{proto}'")
        provider = _check_exact_str(d.get("provider"), "provider")
        if provider not in (CloudProvider.BEAM.value, CloudProvider.MODAL.value):
            raise ContractValidationError(f"Unknown provider '{provider}'")
        model_id = _check_exact_str(d.get("model_id"), "model_id")
        model_revision = _check_exact_str(d.get("model_revision"), "model_revision")
        _validate_revision_identity(model_revision)
        recipe_id = _check_exact_str(d.get("recipe_id"), "recipe_id")
        preprocessing_version = _check_exact_str(d.get("preprocessing_version"), "preprocessing_version")
        native_mask = _check_exact_bool(d.get("native_mask_conditioning"), "native_mask_conditioning")
        limits = ServiceLimits.from_dict(d.get("limits"))
        return cls(
            protocol_version=proto,
            provider=provider,
            model_id=model_id,
            model_revision=model_revision,
            recipe_id=recipe_id,
            preprocessing_version=preprocessing_version,
            native_mask_conditioning=native_mask,
            limits=limits,
        )


def validate_model_info_response(resp: ModelInfoResponse) -> None:
    if resp.protocol_version != PROTOCOL_VERSION:
        raise ContractValidationError(f"Unsupported protocol version: '{resp.protocol_version}'")
    _validate_revision_identity(resp.model_revision)
    if resp.native_mask_conditioning is not False:
        raise ContractValidationError("Worker capability error: FLUX worker cannot claim native_mask_conditioning=true")
    validate_service_limits(resp.limits)


@dataclass(frozen=True)
class JobRequestMetadata:
    protocol_version: str
    job_id: str
    attempt_id: str
    recipe: RenderRecipe
    width: int
    height: int
    seed: int
    steps: int
    guidance_scaled: int
    image_sha256: str
    hint_sha256: str
    request_digest: str

    def to_dict(self) -> Dict[str, Any]:
        return {
            "protocol_version": self.protocol_version,
            "job_id": self.job_id,
            "attempt_id": self.attempt_id,
            "recipe": self.recipe.to_dict(),
            "width": self.width,
            "height": self.height,
            "seed": self.seed,
            "steps": self.steps,
            "guidance_scaled": self.guidance_scaled,
            "image_sha256": self.image_sha256,
            "hint_sha256": self.hint_sha256,
            "request_digest": self.request_digest,
        }

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> JobRequestMetadata:
        if type(d) is not dict:
            raise ContractValidationError("JobRequestMetadata must be a dict")
        _assert_no_forbidden_fields(d)
        _assert_no_extra_keys(d, {
            "protocol_version", "job_id", "attempt_id", "recipe",
            "width", "height", "seed", "steps", "guidance_scaled",
            "image_sha256", "hint_sha256", "request_digest"
        })
        proto = _check_exact_str(d.get("protocol_version"), "protocol_version")
        if proto != PROTOCOL_VERSION:
            raise ContractValidationError(f"Unsupported protocol version '{proto}'")
        job_id = _check_exact_str(d.get("job_id"), "job_id")
        attempt_id = _check_exact_str(d.get("attempt_id"), "attempt_id")
        recipe = RenderRecipe.from_dict(d.get("recipe"))
        width = _check_exact_int(d.get("width"), "width", min_val=1)
        height = _check_exact_int(d.get("height"), "height", min_val=1)
        seed = _check_exact_int(d.get("seed"), "seed", min_val=0)
        steps = _check_exact_int(d.get("steps"), "steps", min_val=1, max_val=100)
        guidance_scaled = _check_exact_int(d.get("guidance_scaled"), "guidance_scaled", min_val=0, max_val=5000)
        image_sha256 = _validate_lowercase_hex_hash(d.get("image_sha256"), "image_sha256", 64)
        hint_sha256 = _validate_lowercase_hex_hash(d.get("hint_sha256"), "hint_sha256", 64)
        request_digest = _validate_lowercase_hex_hash(d.get("request_digest"), "request_digest", 64)
        result = cls(
            protocol_version=proto,
            job_id=job_id,
            attempt_id=attempt_id,
            recipe=recipe,
            width=width,
            height=height,
            seed=seed,
            steps=steps,
            guidance_scaled=guidance_scaled,
            image_sha256=image_sha256,
            hint_sha256=hint_sha256,
            request_digest=request_digest,
        )

        validate_job_request_metadata(result)
        return result


def validate_job_request_metadata(meta: JobRequestMetadata) -> None:
    RenderRecipe.from_dict(meta.recipe.to_dict())
    if meta.protocol_version != PROTOCOL_VERSION:
        raise ContractValidationError(f"Unsupported protocol version '{meta.protocol_version}'")
    _check_exact_str(meta.job_id, "job_id")
    _check_exact_str(meta.attempt_id, "attempt_id")
    _check_exact_str(meta.recipe.recipe_id, "recipe_id")
    _check_exact_str(meta.recipe.preprocessing_version, "preprocessing_version")
    _check_exact_str(meta.recipe.model_id, "model_id")
    _validate_revision_identity(meta.recipe.model_revision)
    if meta.recipe.native_mask_conditioning:
        raise ContractValidationError("Unsupported capability: native_mask_conditioning is unsupported for FLUX SDNQ recipes")
    _validate_lowercase_hex_hash(meta.image_sha256, "image_sha256", 64)
    _validate_lowercase_hex_hash(meta.hint_sha256, "hint_sha256", 64)
    _validate_lowercase_hex_hash(meta.request_digest, "request_digest", 64)
    if not (1 <= meta.steps <= 100):
        raise ContractValidationError(f"Steps {meta.steps} out of range [1..100]")
    if not (0 <= meta.guidance_scaled <= 5000):
        raise ContractValidationError(f"Guidance {meta.guidance_scaled} out of range [0..5000]")


@dataclass(frozen=True)
class JobAcceptedResponse:
    handle: str
    status: str
    job_id: str
    attempt_id: str
    request_digest: str
    recipe_id: str
    preprocessing_version: str
    model_id: str
    model_revision: str
    native_mask_conditioning: bool

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> JobAcceptedResponse:
        if type(d) is not dict:
            raise ContractValidationError("JobAcceptedResponse must be a dict")
        _assert_no_extra_keys(d, {
            "handle", "status", "job_id", "attempt_id", "request_digest",
            "recipe_id", "preprocessing_version", "model_id", "model_revision",
            "native_mask_conditioning"
        })
        status = _check_exact_str(d.get("status"), "status")
        if status != JobExecutionStatus.PENDING.value:
            raise ContractValidationError(f"JobAcceptedResponse status must be 'pending', got '{status}'")
        rev = _check_exact_str(d.get("model_revision"), "model_revision")
        _validate_revision_identity(rev)
        native_mask = _check_exact_bool(d.get("native_mask_conditioning"), "native_mask_conditioning")
        if native_mask:
            raise ContractValidationError("Worker capability error: FLUX worker cannot claim native_mask_conditioning=true")
        return cls(
            handle=_check_exact_str(d.get("handle"), "handle"),
            status=status,
            job_id=_check_exact_str(d.get("job_id"), "job_id"),
            attempt_id=_check_exact_str(d.get("attempt_id"), "attempt_id"),
            request_digest=_validate_lowercase_hex_hash(d.get("request_digest"), "request_digest", 64),
            recipe_id=_check_exact_str(d.get("recipe_id"), "recipe_id"),
            preprocessing_version=_check_exact_str(d.get("preprocessing_version"), "preprocessing_version"),
            model_id=_check_exact_str(d.get("model_id"), "model_id"),
            model_revision=rev,
            native_mask_conditioning=native_mask,
        )


def validate_job_accepted_response(resp: JobAcceptedResponse) -> None:
    if resp.status != JobExecutionStatus.PENDING.value:
        raise ContractValidationError(f"JobAcceptedResponse status must be 'pending', got '{resp.status}'")
    _check_exact_str(resp.handle, "handle")
    _check_exact_str(resp.job_id, "job_id")
    _check_exact_str(resp.attempt_id, "attempt_id")
    _check_exact_str(resp.recipe_id, "recipe_id")
    _check_exact_str(resp.preprocessing_version, "preprocessing_version")
    _check_exact_str(resp.model_id, "model_id")
    _validate_revision_identity(resp.model_revision)
    _validate_lowercase_hex_hash(resp.request_digest, "request_digest", 64)
    if resp.native_mask_conditioning:
        raise ContractValidationError("Worker capability error: FLUX worker cannot claim native_mask_conditioning=true")


@dataclass(frozen=True)
class TypedError:
    error_code: str
    message: str

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> TypedError:
        if type(d) is not dict:
            raise ContractValidationError("TypedError must be a dict")
        _assert_no_extra_keys(d, {"error_code", "message"})
        return cls(
            error_code=_check_exact_str(d.get("error_code"), "error_code"),
            message=_check_exact_str(d.get("message"), "message", min_len=1, max_len=1024, allow_whitespace=True),
        )


@dataclass(frozen=True)
class JobStatusResponse:
    handle: str
    job_id: str
    attempt_id: str
    request_digest: str
    recipe_id: str
    preprocessing_version: str
    model_id: str
    model_revision: str
    native_mask_conditioning: bool
    status: str
    reported_cost_usd: Optional[float] = None
    error: Optional[TypedError] = None
    result_digest: Optional[str] = None
    result_bytes: Optional[int] = None

    def to_dict(self) -> Dict[str, Any]:
        return {
            "handle": self.handle,
            "job_id": self.job_id,
            "attempt_id": self.attempt_id,
            "request_digest": self.request_digest,
            "recipe_id": self.recipe_id,
            "preprocessing_version": self.preprocessing_version,
            "model_id": self.model_id,
            "model_revision": self.model_revision,
            "native_mask_conditioning": self.native_mask_conditioning,
            "status": self.status,
            "reported_cost_usd": self.reported_cost_usd,
            "error": self.error.to_dict() if self.error else None,
            "result_digest": self.result_digest,
            "result_bytes": self.result_bytes,
        }

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> JobStatusResponse:
        if type(d) is not dict:
            raise ContractValidationError("JobStatusResponse must be a dict")
        _assert_no_extra_keys(d, {
            "handle", "job_id", "attempt_id", "request_digest",
            "recipe_id", "preprocessing_version", "model_id", "model_revision",
            "native_mask_conditioning", "status", "reported_cost_usd", "error",
            "result_digest", "result_bytes"
        })
        status = _check_exact_str(d.get("status"), "status")
        valid_statuses = {s.value for s in JobExecutionStatus}
        if status not in valid_statuses:
            raise ContractValidationError(f"Invalid job execution status '{status}'")

        rev = _check_exact_str(d.get("model_revision"), "model_revision")
        _validate_revision_identity(rev)

        err = TypedError.from_dict(d["error"]) if d.get("error") is not None else None
        res_digest = _validate_lowercase_hex_hash(d["result_digest"], "result_digest", 64) if d.get("result_digest") is not None else None
        res_bytes = _check_exact_int(d["result_bytes"], "result_bytes", min_val=1) if d.get("result_bytes") is not None else None
        native_mask = _check_exact_bool(d.get("native_mask_conditioning"), "native_mask_conditioning")
        if native_mask:
            raise ContractValidationError("Worker capability error: FLUX worker cannot claim native_mask_conditioning=true")

        return cls(
            handle=_check_exact_str(d.get("handle"), "handle"),
            job_id=_check_exact_str(d.get("job_id"), "job_id"),
            attempt_id=_check_exact_str(d.get("attempt_id"), "attempt_id"),
            request_digest=_validate_lowercase_hex_hash(d.get("request_digest"), "request_digest", 64),
            recipe_id=_check_exact_str(d.get("recipe_id"), "recipe_id"),
            preprocessing_version=_check_exact_str(d.get("preprocessing_version"), "preprocessing_version"),
            model_id=_check_exact_str(d.get("model_id"), "model_id"),
            model_revision=rev,
            native_mask_conditioning=native_mask,
            status=status,
            reported_cost_usd=_check_optional_float(d.get("reported_cost_usd"), "reported_cost_usd"),
            error=err,
            result_digest=res_digest,
            result_bytes=res_bytes,
        )


def validate_job_status_response(resp: JobStatusResponse) -> None:
    _check_exact_str(resp.handle, "handle")
    _check_exact_str(resp.job_id, "job_id")
    _check_exact_str(resp.attempt_id, "attempt_id")
    _check_exact_str(resp.recipe_id, "recipe_id")
    _check_exact_str(resp.preprocessing_version, "preprocessing_version")
    _check_exact_str(resp.model_id, "model_id")
    _validate_revision_identity(resp.model_revision)
    _validate_lowercase_hex_hash(resp.request_digest, "request_digest", 64)
    if resp.native_mask_conditioning:
        raise ContractValidationError("Worker capability error: FLUX worker cannot claim native_mask_conditioning=true")
    if resp.reported_cost_usd is not None:
        if not math.isfinite(resp.reported_cost_usd) or resp.reported_cost_usd < 0.0:
            raise ContractValidationError(f"reported_cost_usd must be finite non-negative float, got {resp.reported_cost_usd}")
    if resp.status == JobExecutionStatus.COMPLETED.value:
        if resp.result_digest is not None:
            _validate_lowercase_hex_hash(resp.result_digest, "result_digest", 64)


@dataclass(frozen=True)
class JobCancelResponse:
    handle: str
    job_id: str
    attempt_id: str
    request_digest: str
    status: str = "cancel_requested"
    acknowledged: bool = True

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> JobCancelResponse:
        if type(d) is not dict:
            raise ContractValidationError("JobCancelResponse must be a dict")
        _assert_no_extra_keys(d, {"handle", "job_id", "attempt_id", "request_digest", "status", "acknowledged"})
        status = _check_exact_str(d.get("status"), "status")
        if status != "cancel_requested":
            raise ContractValidationError(f"Cancel response status must be non-terminal 'cancel_requested', got '{status}'")
        ack = _check_exact_bool(d.get("acknowledged"), "acknowledged")
        if not ack:
            raise ContractValidationError("Cancel response acknowledged must be true")
        return cls(
            handle=_check_exact_str(d.get("handle"), "handle"),
            job_id=_check_exact_str(d.get("job_id"), "job_id"),
            attempt_id=_check_exact_str(d.get("attempt_id"), "attempt_id"),
            request_digest=_validate_lowercase_hex_hash(d.get("request_digest"), "request_digest", 64),
            status=status,
            acknowledged=True,
        )


@dataclass(frozen=True)
class PreEnqueueRejectionResponse:
    """Authoritative structured rejection response when server rejects before queuing."""
    enqueued: bool
    error_code: str
    message: str
    job_id: str
    attempt_id: str
    request_digest: str
    retryable: bool
    details: Optional[Any] = None

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> PreEnqueueRejectionResponse:
        if type(d) is not dict:
            raise ContractValidationError("PreEnqueueRejectionResponse must be a dict")
        _assert_no_extra_keys(d, {"enqueued", "error_code", "message", "job_id", "attempt_id", "request_digest", "retryable", "details"})
        enqueued = _check_exact_bool(d.get("enqueued"), "enqueued")
        if enqueued is not False:
            raise ContractValidationError(f"Pre-enqueue rejection must have enqueued=false, got {enqueued}")
        err_code = _check_exact_str(d.get("error_code"), "error_code")
        if err_code not in ALLOWLISTED_REJECTION_CODES:
            raise ContractValidationError(f"Unknown rejection error code: '{err_code}'")
        return cls(
            enqueued=False,
            error_code=err_code,
            message=_check_exact_str(d.get("message"), "message", min_len=1, max_len=1024, allow_whitespace=True),
            job_id=_check_exact_str(d.get("job_id"), "job_id"),
            attempt_id=_check_exact_str(d.get("attempt_id"), "attempt_id"),
            request_digest=_validate_lowercase_hex_hash(d.get("request_digest"), "request_digest", 64),
            retryable=_check_exact_bool(d.get("retryable"), "retryable"),
            details=d.get("details"),
        )


def validate_pre_enqueue_rejection(rejection: PreEnqueueRejectionResponse) -> None:
    if rejection.enqueued is not False:
        raise ContractValidationError("Rejection enqueued must be False")
    _check_exact_str(rejection.job_id, "job_id")
    _check_exact_str(rejection.attempt_id, "attempt_id")
    _validate_lowercase_hex_hash(rejection.request_digest, "request_digest", 64)
    if rejection.error_code not in ALLOWLISTED_REJECTION_CODES:
        raise ContractValidationError(f"Unknown rejection code '{rejection.error_code}'")


@dataclass(frozen=True)
class ResultMetadata:
    handle: str
    job_id: str
    attempt_id: str
    request_digest: str
    recipe_id: str
    preprocessing_version: str
    model_id: str
    model_revision: str
    native_mask_conditioning: bool
    result_digest: str
    reported_cost_usd: Optional[float]
    width: int
    height: int
    byte_length: int

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)

    @classmethod
    def from_dict(cls, d: Dict[str, Any]) -> ResultMetadata:
        if type(d) is not dict:
            raise ContractValidationError("ResultMetadata must be a dict")
        _assert_no_extra_keys(d, {
            "handle", "job_id", "attempt_id", "request_digest",
            "recipe_id", "preprocessing_version", "model_id", "model_revision",
            "native_mask_conditioning", "result_digest", "reported_cost_usd",
            "width", "height", "byte_length"
        })
        rev = _check_exact_str(d.get("model_revision"), "model_revision")
        _validate_revision_identity(rev)
        native_mask = _check_exact_bool(d.get("native_mask_conditioning"), "native_mask_conditioning")
        if native_mask:
            raise ContractValidationError("Worker capability error: FLUX worker cannot claim native_mask_conditioning=true")
        return cls(
            handle=_check_exact_str(d.get("handle"), "handle"),
            job_id=_check_exact_str(d.get("job_id"), "job_id"),
            attempt_id=_check_exact_str(d.get("attempt_id"), "attempt_id"),
            request_digest=_validate_lowercase_hex_hash(d.get("request_digest"), "request_digest", 64),
            recipe_id=_check_exact_str(d.get("recipe_id"), "recipe_id"),
            preprocessing_version=_check_exact_str(d.get("preprocessing_version"), "preprocessing_version"),
            model_id=_check_exact_str(d.get("model_id"), "model_id"),
            model_revision=rev,
            native_mask_conditioning=native_mask,
            result_digest=_validate_lowercase_hex_hash(d.get("result_digest"), "result_digest", 64),
            reported_cost_usd=_check_optional_float(d.get("reported_cost_usd"), "reported_cost_usd"),
            width=_check_exact_int(d.get("width"), "width", min_val=1),
            height=_check_exact_int(d.get("height"), "height", min_val=1),
            byte_length=_check_exact_int(d.get("byte_length"), "byte_length", min_val=1),
        )


def validate_result_metadata(resp: ResultMetadata) -> None:
    _check_exact_str(resp.handle, "handle")
    _check_exact_str(resp.job_id, "job_id")
    _check_exact_str(resp.attempt_id, "attempt_id")
    _check_exact_str(resp.recipe_id, "recipe_id")
    _check_exact_str(resp.preprocessing_version, "preprocessing_version")
    _check_exact_str(resp.model_id, "model_id")
    _validate_revision_identity(resp.model_revision)
    _validate_lowercase_hex_hash(resp.request_digest, "request_digest", 64)
    _validate_lowercase_hex_hash(resp.result_digest, "result_digest", 64)
    if resp.native_mask_conditioning:
        raise ContractValidationError("Worker capability error: FLUX worker cannot claim native_mask_conditioning=true")
    if resp.reported_cost_usd is not None:
        if not math.isfinite(resp.reported_cost_usd) or resp.reported_cost_usd < 0.0:
            raise ContractValidationError(f"reported_cost_usd must be finite non-negative float, got {resp.reported_cost_usd}")
    if resp.width <= 0 or resp.height <= 0 or resp.byte_length <= 0:
        raise ContractValidationError("Result dimensions and byte length must be positive")


def compute_canonical_json_array(meta: JobRequestMetadata) -> str:
    """Deterministic JSON ordered array encoding for request metadata."""
    canonical_list = [
        "MC-REQ-V1",
        meta.protocol_version,
        meta.job_id,
        meta.attempt_id,
        meta.recipe.recipe_id,
        meta.recipe.preprocessing_version,
        meta.recipe.model_id,
        meta.recipe.model_revision,
        meta.recipe.native_mask_conditioning,
        meta.width,
        meta.height,
        meta.seed,
        meta.steps,
        meta.guidance_scaled,
        meta.image_sha256,
        meta.hint_sha256,
    ]
    if meta.recipe.qwen_edit is not None:
        canonical_list.append([meta.recipe.qwen_edit.target, meta.recipe.qwen_edit.description])
    return json.dumps(canonical_list, separators=(",", ":"), ensure_ascii=False)


def compute_request_digest(meta: JobRequestMetadata) -> str:
    """SHA-256 hex digest over deterministic canonical JSON array encoding."""
    canonical = compute_canonical_json_array(meta)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def validate_png_stream(
    png_bytes: bytes,
    expected_width: int,
    expected_height: int,
    expected_color_type: int,  # 2 for RGB8, 0 for Gray8
) -> None:
    """Strict validation of the full PNG stream.

    Walks all chunks enforcing bounds and CRC-32 integrity, requiring ordered
    IHDR -> IDAT -> IEND chunks, rejecting trailing or truncated data, and safely
    inflating IDAT payloads with a strict decoded-size bound and exact RGB8/Gray8
    scanline length and filter type (0..4) constraints.
    """
    if not isinstance(png_bytes, (bytes, bytearray)):
        raise ContractValidationError(f"PNG payload must be bytes, got {type(png_bytes).__name__}")
    if len(png_bytes) < 33:
        raise ContractValidationError("PNG buffer too short for valid PNG stream")
    if png_bytes[:8] != b"\x89PNG\r\n\x1a\n":
        raise ContractValidationError("Invalid PNG signature")

    offset = 8
    total_len = len(png_bytes)
    chunk_index = 0
    header_parsed = False
    idat_chunks: List[bytes] = []
    seen_idat = False
    seen_iend = False
    last_chunk_type: Optional[bytes] = None

    width = 0
    height = 0
    color_type_val = 0

    while offset < total_len:
        if seen_iend:
            raise ContractValidationError(f"Trailing data after IEND chunk: {total_len - offset} extra bytes")

        if offset + 8 > total_len:
            raise ContractValidationError("Truncated PNG chunk header")

        chunk_len, chunk_type = struct.unpack(">I4s", png_bytes[offset : offset + 8])
        chunk_data_end = offset + 8 + chunk_len
        chunk_crc_end = chunk_data_end + 4

        if chunk_crc_end > total_len:
            chunk_name = chunk_type.decode("latin1", errors="replace")
            raise ContractValidationError(
                f"Truncated PNG chunk '{chunk_name}': expected {chunk_len + 12} bytes from offset {offset}, "
                f"remaining buffer is {total_len - offset}"
            )

        chunk_data = png_bytes[offset + 8 : chunk_data_end]
        expected_crc = struct.unpack(">I", png_bytes[chunk_data_end : chunk_crc_end])[0]
        computed_crc = zlib.crc32(png_bytes[offset + 4 : chunk_data_end]) & 0xFFFFFFFF
        if computed_crc != expected_crc:
            chunk_name = chunk_type.decode("latin1", errors="replace")
            raise ContractValidationError(
                f"PNG chunk '{chunk_name}' CRC-32 mismatch: expected {expected_crc}, computed {computed_crc}"
            )

        if chunk_index == 0:
            if chunk_type != b"IHDR" or chunk_len != 13:
                raise ContractValidationError("First PNG chunk must be IHDR with length exactly 13")
            w, h, bit_depth, color_type, compression, filter_method, interlace = struct.unpack(
                ">IIBBBBB", chunk_data
            )
            if w != expected_width or h != expected_height:
                raise ContractValidationError(
                    f"PNG dimensions {w}x{h} mismatch expected {expected_width}x{expected_height}"
                )
            if bit_depth != 8:
                raise ContractValidationError(f"Unsupported PNG bit depth {bit_depth}, expected 8")
            if color_type != expected_color_type:
                raise ContractValidationError(
                    f"PNG color type {color_type} mismatch expected {expected_color_type}"
                )
            if compression != 0 or filter_method != 0 or interlace != 0:
                raise ContractValidationError("Unsupported PNG compression/filter/interlace method")
            header_parsed = True
            width = w
            height = h
            color_type_val = color_type
        else:
            if chunk_type == b"IHDR":
                raise ContractValidationError("Duplicate IHDR chunk")
            elif chunk_type == b"IDAT":
                if seen_idat and last_chunk_type != b"IDAT":
                    raise ContractValidationError("Non-consecutive IDAT chunk")
                idat_chunks.append(chunk_data)
                seen_idat = True
            elif chunk_type == b"IEND":
                if not seen_idat:
                    raise ContractValidationError("IEND chunk before IDAT")
                if chunk_len != 0:
                    raise ContractValidationError(f"IEND chunk length must be 0, got {chunk_len}")
                seen_iend = True
            else:
                if seen_iend:
                    chunk_name = chunk_type.decode("latin1", errors="replace")
                    raise ContractValidationError(f"Chunk '{chunk_name}' after IEND")

        last_chunk_type = chunk_type
        chunk_index += 1
        offset = chunk_crc_end

    if not header_parsed:
        raise ContractValidationError("Missing IHDR chunk")
    if not seen_idat or len(idat_chunks) == 0:
        raise ContractValidationError("Missing IDAT chunk")
    if not seen_iend:
        raise ContractValidationError("Missing IEND chunk / truncated PNG stream")

    # Safe IDAT inflation with strict decoded-size bound
    idat_combined = b"".join(idat_chunks)
    channels = 3 if color_type_val == 2 else 1
    bytes_per_row = 1 + width * channels
    expected_raw_size = height * bytes_per_row

    try:
        decompressor = zlib.decompressobj()
        decompressed = decompressor.decompress(idat_combined, max_length=expected_raw_size + 1)
        if decompressor.unconsumed_tail or len(decompressed) > expected_raw_size:
            raise ContractValidationError(
                f"Decompressed IDAT exceeds maximum expected size {expected_raw_size}"
            )
        remaining_budget = max(1, (expected_raw_size + 1) - len(decompressed))
        decompressed += decompressor.flush(remaining_budget)
        if decompressor.unconsumed_tail or len(decompressed) > expected_raw_size:
            raise ContractValidationError(
                f"Decompressed IDAT exceeds maximum expected size {expected_raw_size}"
            )
        if not decompressor.eof:
            raise ContractValidationError(
                "Truncated or incomplete IDAT zlib stream: stream did not reach EOF"
            )
        if decompressor.unused_data:
            raise ContractValidationError(
                f"Trailing uncompressed or concatenated data in IDAT chunk: {len(decompressor.unused_data)} unused bytes"
            )
        if len(decompressed) != expected_raw_size:
            raise ContractValidationError(
                f"Decompressed IDAT size {len(decompressed)} mismatch expected {expected_raw_size}"
            )
    except zlib.error as exc:
        raise ContractValidationError(f"Corrupt or truncated IDAT zlib stream: {exc}")

    # Exact RGB8 / Gray8 scanline length and filter constraints (0..4)
    for row_idx in range(height):
        filter_type = decompressed[row_idx * bytes_per_row]
        if filter_type not in (0, 1, 2, 3, 4):
            raise ContractValidationError(
                f"Invalid PNG filter type {filter_type} at row {row_idx} (must be 0..4)"
            )


# Alias for backward-compatibility
validate_png_header = validate_png_stream


def validate_crop_payload(
    meta: JobRequestMetadata,
    image_bytes: bytes,
    hint_bytes: bytes,
    limits: ServiceLimits,
) -> None:
    """Strictly validate a crop request payload using bounded arithmetic and header checks."""
    validate_job_request_metadata(meta)
    validate_service_limits(limits)

    if meta.protocol_version != PROTOCOL_VERSION:
        raise ContractValidationError(
            f"Unsupported protocol version: '{meta.protocol_version}', expected '{PROTOCOL_VERSION}'"
        )

    # Bounded geometry checks
    if meta.width <= 0 or meta.width > limits.max_width:
        raise ContractValidationError(f"Width {meta.width} out of bounds (1..{limits.max_width})")
    if meta.height <= 0 or meta.height > limits.max_height:
        raise ContractValidationError(f"Height {meta.height} out of bounds (1..{limits.max_height})")

    if meta.width % LATENT_STRIDE != 0:
        raise ContractValidationError(f"Width {meta.width} not aligned to latent stride {LATENT_STRIDE}")
    if meta.height % LATENT_STRIDE != 0:
        raise ContractValidationError(f"Height {meta.height} not aligned to latent stride {LATENT_STRIDE}")

    # Pixel count with bounded arithmetic
    pixels = meta.width * meta.height
    if pixels > limits.max_pixels:
        raise ContractValidationError(f"Pixel count {pixels} exceeds maximum {limits.max_pixels}")

    # Byte length limits
    if len(image_bytes) > limits.max_png_bytes:
        raise ContractValidationError(f"Image bytes length {len(image_bytes)} exceeds limit {limits.max_png_bytes}")
    if len(hint_bytes) > limits.max_png_bytes:
        raise ContractValidationError(f"Hint bytes length {len(hint_bytes)} exceeds limit {limits.max_png_bytes}")
    total_bytes = len(image_bytes) + len(hint_bytes)
    if total_bytes > limits.max_multipart_bytes:
        raise ContractValidationError(
            f"Total payload bytes {total_bytes} exceeds multipart limit {limits.max_multipart_bytes}"
        )

    # Bounded PNG header inspection (RGB8 for image, Gray8 for hint)
    validate_png_header(image_bytes, meta.width, meta.height, expected_color_type=2)
    validate_png_header(hint_bytes, meta.width, meta.height, expected_color_type=0)

    # PNG buffer integrity check against declared hashes
    img_hash = hashlib.sha256(image_bytes).hexdigest()
    if img_hash != meta.image_sha256:
        raise ContractValidationError(f"Image SHA-256 mismatch: declared '{meta.image_sha256}', computed '{img_hash}'")

    hint_hash = hashlib.sha256(hint_bytes).hexdigest()
    if hint_hash != meta.hint_sha256:
        raise ContractValidationError(f"Hint SHA-256 mismatch: declared '{meta.hint_sha256}', computed '{hint_hash}'")

    # Canonical request digest integrity check
    expected_digest = compute_request_digest(meta)
    if expected_digest != meta.request_digest:
        raise ContractValidationError(
            f"Request digest mismatch: declared '{meta.request_digest}', computed '{expected_digest}'"
        )


def validate_recipe_compatibility(
    meta: JobRequestMetadata,
    model_info: ModelInfoResponse,
) -> None:
    """Pre-submit verification that requested recipe/model matches endpoint capabilities."""
    validate_model_info_response(model_info)
    validate_job_request_metadata(meta)
    if model_info.protocol_version != PROTOCOL_VERSION:
        raise ContractValidationError(
            f"Model-info protocol version '{model_info.protocol_version}' unsupported"
        )
    if meta.recipe.recipe_id != model_info.recipe_id:
        raise ContractValidationError(
            f"Recipe ID mismatch: requested '{meta.recipe.recipe_id}', model-info '{model_info.recipe_id}'"
        )
    if meta.recipe.preprocessing_version != model_info.preprocessing_version:
        raise ContractValidationError(
            f"Preprocessing version mismatch: requested '{meta.recipe.preprocessing_version}', "
            f"model-info '{model_info.preprocessing_version}'"
        )
    if meta.recipe.model_id != model_info.model_id:
        raise ContractValidationError(
            f"Model ID mismatch: requested '{meta.recipe.model_id}', model-info '{model_info.model_id}'"
        )
    if meta.recipe.model_revision != model_info.model_revision:
        raise ContractValidationError(
            f"Model revision mismatch: requested '{meta.recipe.model_revision}', "
            f"model-info '{model_info.model_revision}'"
        )
    if model_info.native_mask_conditioning is not False:
        raise ContractValidationError(
            "Worker capability error: FLUX worker cannot claim native_mask_conditioning=true"
        )
    if meta.recipe.native_mask_conditioning is not False:
        raise ContractValidationError(
            "Request error: requested native_mask_conditioning=true but worker capability is false"
        )


def validate_response_binding(
    meta: JobRequestMetadata,
    resp: Union[JobAcceptedResponse, JobStatusResponse, ResultMetadata],
    expected_handle: Optional[str] = None,
) -> None:
    """Enforce exact binding across request and server responses."""
    validate_job_request_metadata(meta)
    if isinstance(resp, JobAcceptedResponse):
        validate_job_accepted_response(resp)
    elif isinstance(resp, JobStatusResponse):
        validate_job_status_response(resp)
    elif isinstance(resp, ResultMetadata):
        validate_result_metadata(resp)
    else:
        raise ContractValidationError(f"Unsupported response type for binding validation: {type(resp)}")

    if expected_handle is not None and resp.handle != expected_handle:
        raise ContractValidationError(
            f"Binding mismatch on handle: expected '{expected_handle}', got '{resp.handle}'"
        )
    if resp.job_id != meta.job_id:
        raise ContractValidationError(f"Binding mismatch on job_id: expected '{meta.job_id}', got '{resp.job_id}'")
    if resp.attempt_id != meta.attempt_id:
        raise ContractValidationError(f"Binding mismatch on attempt_id: expected '{meta.attempt_id}', got '{resp.attempt_id}'")
    if resp.request_digest != meta.request_digest:
        raise ContractValidationError(
            f"Binding mismatch on request_digest: expected '{meta.request_digest}', got '{resp.request_digest}'"
        )
    if resp.recipe_id != meta.recipe.recipe_id:
        raise ContractValidationError(f"Binding mismatch on recipe_id: expected '{meta.recipe.recipe_id}', got '{resp.recipe_id}'")
    if resp.preprocessing_version != meta.recipe.preprocessing_version:
        raise ContractValidationError(
            f"Binding mismatch on preprocessing_version: expected '{meta.recipe.preprocessing_version}', "
            f"got '{resp.preprocessing_version}'"
        )
    if resp.model_id != meta.recipe.model_id:
        raise ContractValidationError(f"Binding mismatch on model_id: expected '{meta.recipe.model_id}', got '{resp.model_id}'")
    if resp.model_revision != meta.recipe.model_revision:
        raise ContractValidationError(
            f"Binding mismatch on model_revision: expected '{meta.recipe.model_revision}', "
            f"got '{resp.model_revision}'"
        )
    if resp.native_mask_conditioning != meta.recipe.native_mask_conditioning:
        raise ContractValidationError(
            f"Binding mismatch on native_mask_conditioning: expected '{meta.recipe.native_mask_conditioning}', "
            f"got '{resp.native_mask_conditioning}'"
        )


def validate_cancel_response(
    meta: JobRequestMetadata,
    resp: JobCancelResponse,
    expected_handle: str,
) -> None:
    """Validate non-terminal cancel response, digest, and handle/identity binding."""
    validate_job_request_metadata(meta)
    _check_exact_str(resp.handle, "handle")
    _check_exact_str(resp.job_id, "job_id")
    _check_exact_str(resp.attempt_id, "attempt_id")
    _validate_lowercase_hex_hash(resp.request_digest, "request_digest", 64)

    if resp.handle != expected_handle:
        raise ContractValidationError(f"Cancel handle mismatch: expected '{expected_handle}', got '{resp.handle}'")
    if resp.job_id != meta.job_id:
        raise ContractValidationError(f"Cancel job_id mismatch: expected '{meta.job_id}', got '{resp.job_id}'")
    if resp.attempt_id != meta.attempt_id:
        raise ContractValidationError(f"Cancel attempt_id mismatch: expected '{meta.attempt_id}', got '{resp.attempt_id}'")
    if resp.request_digest != meta.request_digest:
        raise ContractValidationError(f"Cancel request_digest mismatch: expected '{meta.request_digest}', got '{resp.request_digest}'")
    if resp.status != "cancel_requested":
        raise ContractValidationError(f"Cancel status must be non-terminal 'cancel_requested', got '{resp.status}'")
    if resp.acknowledged is not True:
        raise ContractValidationError("Cancel response acknowledged must be True")


def is_authoritative_safe_to_retry(
    rejection: PreEnqueueRejectionResponse,
    meta: JobRequestMetadata,
) -> bool:
    """Authoritatively verify structured pre-enqueue rejection before safe retry."""
    try:
        validate_pre_enqueue_rejection(rejection)
        validate_job_request_metadata(meta)
    except ContractValidationError:
        return False
    if rejection.enqueued is not False:
        return False
    if (
        rejection.job_id != meta.job_id
        or rejection.attempt_id != meta.attempt_id
        or rejection.request_digest != meta.request_digest
    ):
        return False
    if rejection.error_code not in RETRYABLE_PRE_ENQUEUE_CODES:
        return False
    return rejection.retryable is True


def validate_worker_result(
    result_bytes: bytes,
    result_digest: str,
    expected_width: int,
    expected_height: int,
    limits: ServiceLimits,
    reported_cost_usd: Optional[float] = None,
) -> None:
    """Validate worker diffusion output buffer, digest, exact dimensions, and limits before persistence."""
    validate_service_limits(limits)

    if not isinstance(result_bytes, (bytes, bytearray)):
        raise ContractValidationError(f"Result bytes must be bytes, got {type(result_bytes).__name__}")
    if len(result_bytes) == 0:
        raise ContractValidationError("Result bytes cannot be empty")
    if len(result_bytes) > limits.max_png_bytes:
        raise ContractValidationError(
            f"Result byte length {len(result_bytes)} exceeds maximum limit {limits.max_png_bytes}"
        )

    if expected_width <= 0 or expected_width > limits.max_width:
        raise ContractValidationError(f"Width {expected_width} out of bounds (1..{limits.max_width})")
    if expected_height <= 0 or expected_height > limits.max_height:
        raise ContractValidationError(f"Height {expected_height} out of bounds (1..{limits.max_height})")

    if expected_width % LATENT_STRIDE != 0:
        raise ContractValidationError(f"Width {expected_width} not aligned to latent stride {LATENT_STRIDE}")
    if expected_height % LATENT_STRIDE != 0:
        raise ContractValidationError(f"Height {expected_height} not aligned to latent stride {LATENT_STRIDE}")

    if expected_width * expected_height > limits.max_pixels:
        raise ContractValidationError(
            f"Pixel count {expected_width * expected_height} exceeds maximum {limits.max_pixels}"
        )

    _validate_lowercase_hex_hash(result_digest, "result_digest", 64)

    # Validate PNG structure and exact dimensions
    validate_png_header(result_bytes, expected_width, expected_height, expected_color_type=2)

    # Validate computed digest against declared digest
    computed_digest = hashlib.sha256(result_bytes).hexdigest()
    if computed_digest != result_digest:
        raise ContractValidationError(
            f"Result digest mismatch: expected '{result_digest}', computed '{computed_digest}'"
        )

    if reported_cost_usd is not None:
        _check_optional_float(reported_cost_usd, "reported_cost_usd")


def validate_result_bytes(
    result_bytes: bytes,
    result_meta: ResultMetadata,
    request_meta: JobRequestMetadata,
    limits: ServiceLimits,
    expected_handle: str,
) -> None:
    """Validate result payload bytes against result metadata, request bounds, and service limits."""
    validate_service_limits(limits)
    validate_response_binding(request_meta, result_meta, expected_handle=expected_handle)

    if result_meta.width != request_meta.width or result_meta.height != request_meta.height:
        raise ContractValidationError(
            f"Result geometry {result_meta.width}x{result_meta.height} differs from request {request_meta.width}x{request_meta.height}"
        )
    if len(result_bytes) != result_meta.byte_length:
        raise ContractValidationError(
            f"Result byte length mismatch: declared {result_meta.byte_length}, actual {len(result_bytes)}"
        )

    validate_worker_result(
        result_bytes=result_bytes,
        result_digest=result_meta.result_digest,
        expected_width=request_meta.width,
        expected_height=request_meta.height,
        limits=limits,
        reported_cost_usd=result_meta.reported_cost_usd,
    )

# Analysis is a separate protocol. FLUX /mc/v1 schemas above stay byte-identical.
# 1.1.0: tiles overlap. Each request names its page size and the core of the
# tile the client keeps, and the tile must be on `analysis_tile_plan`.
ANALYSIS_VERSION = "1.1.0"
ANALYSIS_SAM = "text_mask_sam_ts@1"
ANALYSIS_RT = "text_regions_rt@1"
ANALYSIS_MAX_PNG_BYTES = 4_194_304
ANALYSIS_MAX_COMPONENTS = 4096
ANALYSIS_MAX_BOXES = 4096
ANALYSIS_MAX_RESPONSE_BYTES = 6_500_000


def analysis_rect(value: Any, *, tile: bool = False) -> Dict[str, int]:
    if type(value) is not dict:
        raise ContractValidationError("analysis rect must be an object")
    _assert_no_extra_keys(value, {"x", "y", "width", "height"})
    rect = {name: _check_exact_int(value.get(name), name, 1 if name in ("width", "height") else 0, 2**32 - 1)
            for name in ("x", "y", "width", "height")}
    if rect["x"] + rect["width"] > 2**32 - 1 or rect["y"] + rect["height"] > 2**32 - 1:
        raise ContractValidationError("analysis rect overflow")
    if tile and (rect["width"] > 1024 or rect["height"] > 1024):
        raise ContractValidationError("analysis tile exceeds 1024")
    return rect


ANALYSIS_TILE_SIDE = 1024
# The least context a tile keeps past each side of its core that has a
# neighbour. Mirrors `cleaner_core::cloud_tiles::TILE_CONTEXT`.
ANALYSIS_TILE_CONTEXT = 128
ANALYSIS_MAX_GRID = 1_000_000


def _analysis_axis_count(extent: int) -> int:
    """Tiles along an axis of `extent` pixels."""
    if extent <= ANALYSIS_TILE_SIDE:
        return 1
    return 1 + -(-(extent - ANALYSIS_TILE_SIDE) // (ANALYSIS_TILE_SIDE - 2 * ANALYSIS_TILE_CONTEXT))


def _analysis_check_grid(width: int, height: int) -> None:
    """Refuse an empty page, or one whose plan would exceed ANALYSIS_MAX_GRID
    tiles. The planner and the membership check both go through here, so no
    tile of a page the planner refuses is accepted. Mirrors `cloud_tiles::grid`."""
    if width < 1 or height < 1:
        raise ContractValidationError("empty analysis page")
    if _analysis_axis_count(width) * _analysis_axis_count(height) > ANALYSIS_MAX_GRID:
        raise ContractValidationError("analysis page grid too large")


def _analysis_axis(extent: int) -> List[tuple[int, int, int, int]]:
    """(tile start, tile length, core start, core length) along one axis."""
    if extent <= ANALYSIS_TILE_SIDE:
        return [(0, extent, 0, extent)]
    span = extent - ANALYSIS_TILE_SIDE
    count = _analysis_axis_count(extent)
    origins = [k * span // (count - 1) for k in range(count)]
    cuts = [0] + [(origins[k - 1] + ANALYSIS_TILE_SIDE + origins[k]) // 2 for k in range(1, count)] + [extent]
    return [(origins[k], ANALYSIS_TILE_SIDE, cuts[k], cuts[k + 1] - cuts[k]) for k in range(count)]


def analysis_tile_plan(width: int, height: int) -> List[Dict[str, Dict[str, int]]]:
    """Every tile of a page and the core it owns, row by row.

    Tiles along an axis longer than 1024 are exactly 1024, first at 0 and last
    flush with the far edge, evenly spaced so neighbours overlap by at least
    twice ANALYSIS_TILE_CONTEXT; cores cut the page at each overlap's midpoint.
    Mirrors `cleaner_core::cloud_tiles::planned`; both are checked against
    fixtures/analysis_v1/tile_plans.json.
    """
    _analysis_check_grid(width, height)
    columns, rows = _analysis_axis(width), _analysis_axis(height)
    return [
        {"tile": {"x": cx, "y": ry, "width": cw, "height": rh},
         "core": {"x": ccx, "y": rcy, "width": ccw, "height": rch}}
        for (ry, rh, rcy, rch) in rows
        for (cx, cw, ccx, ccw) in columns
    ]


def _analysis_axis_core(extent: int, start: int, length: int) -> Optional[tuple[int, int]]:
    """The core of the tile at `start` along one axis, without building the
    axis: a request names any page size, so this stays constant time."""
    if extent <= ANALYSIS_TILE_SIDE:
        return (0, extent) if (start, length) == (0, extent) else None
    if length != ANALYSIS_TILE_SIDE:
        return None
    span = extent - ANALYSIS_TILE_SIDE
    count = _analysis_axis_count(extent)

    def origin(k: int) -> int:
        return k * span // (count - 1)

    guess = start * (count - 1) // span
    for k in (guess, guess + 1):
        if 0 <= k < count and origin(k) == start:
            low = 0 if k == 0 else (origin(k - 1) + ANALYSIS_TILE_SIDE + origin(k)) // 2
            high = extent if k == count - 1 else (origin(k) + ANALYSIS_TILE_SIDE + origin(k + 1)) // 2
            return low, high - low
    return None


def analysis_tile_core(width: int, height: int, tile: Dict[str, int]) -> Optional[Dict[str, int]]:
    """The core the plan gives this tile, or None for a tile not on it.
    Raises for a page the planner refuses, whatever tile is named."""
    if width < 1 or height < 1:
        return None
    _analysis_check_grid(width, height)
    column = _analysis_axis_core(width, tile["x"], tile["width"])
    row = _analysis_axis_core(height, tile["y"], tile["height"])
    if column is None or row is None:
        return None
    return {"x": column[0], "y": row[0], "width": column[1], "height": row[1]}


def analysis_request_digest(request: Dict[str, Any]) -> tuple[bytes, str]:
    if type(request) is not dict:
        raise ContractValidationError("analysis metadata must be an object")
    _assert_no_extra_keys(request, {
        "protocol_version", "capability", "graph_sha256s", "model_revision", "tile_id",
        "tile_rect", "tile_png_sha256", "source_page_sha256", "page_width", "page_height",
        "tile_core", "request_digest",
    })
    if request.get("protocol_version") != ANALYSIS_VERSION or request.get("capability") not in (ANALYSIS_SAM, ANALYSIS_RT):
        raise ContractValidationError("unknown analysis version or capability")
    capability = request["capability"]
    graphs = request.get("graph_sha256s")
    if type(graphs) is not list or len(graphs) != (2 if capability == ANALYSIS_SAM else 1):
        raise ContractValidationError("wrong analysis graph count")
    for graph in graphs:
        _validate_lowercase_hex_hash(graph, "graph_sha256s")
    revision = request.get("model_revision")
    _validate_revision_identity(revision)
    tile_id = _check_exact_str(request.get("tile_id"), "tile_id", max_len=64)
    if not all(c.isascii() and (c.isalnum() or c in "-_") for c in tile_id):
        raise ContractValidationError("invalid tile id")
    rect = analysis_rect(request.get("tile_rect"), tile=True)
    tile_sha = _validate_lowercase_hex_hash(request.get("tile_png_sha256"), "tile_png_sha256")
    page_sha = _validate_lowercase_hex_hash(request.get("source_page_sha256"), "source_page_sha256")
    page_width = _check_exact_int(request.get("page_width"), "page_width", 1, 2**32 - 1)
    page_height = _check_exact_int(request.get("page_height"), "page_height", 1, 2**32 - 1)
    core = analysis_rect(request.get("tile_core"))
    if analysis_tile_core(page_width, page_height, rect) != core:
        raise ContractValidationError("analysis tile is not on the page's tile plan")
    canonical = json.dumps([
        "MC-ANA-V1", ANALYSIS_VERSION, capability, graphs, revision, tile_id,
        rect["x"], rect["y"], rect["width"], rect["height"], tile_sha, page_sha,
        page_width, page_height, core["x"], core["y"], core["width"], core["height"],
    ], separators=(",", ":"), ensure_ascii=True).encode("utf-8")
    return canonical, hashlib.sha256(canonical).hexdigest()


def validate_analysis_request(request: Dict[str, Any], tile_png: bytes) -> None:
    _, digest = analysis_request_digest(request)
    if _validate_lowercase_hex_hash(request.get("request_digest"), "request_digest") != digest:
        raise ContractValidationError("analysis request digest mismatch")
    if len(tile_png) > ANALYSIS_MAX_PNG_BYTES or hashlib.sha256(tile_png).hexdigest() != request["tile_png_sha256"]:
        raise ContractValidationError("analysis tile PNG digest or byte limit mismatch")
    _validate_analysis_png(tile_png, request["tile_rect"], "RGB")


def _validate_analysis_png(data: bytes, rect: Dict[str, int], mode: str) -> None:
    if len(data) > ANALYSIS_MAX_PNG_BYTES:
        raise ContractValidationError("analysis PNG too large")
    try:
        from PIL import Image
        from io import BytesIO
        with Image.open(BytesIO(data)) as image:
            if image.format != "PNG" or image.mode != mode or image.size != (rect["width"], rect["height"]):
                raise ContractValidationError("analysis PNG geometry or format mismatch")
            image.load()
    except (OSError, ValueError) as exc:
        raise ContractValidationError("invalid analysis PNG") from exc


def validate_analysis_result(result: Dict[str, Any], request: Dict[str, Any]) -> None:
    _assert_no_extra_keys(result, {
        "protocol_version", "capability", "request_digest", "tile_id", "tile_rect",
        "graph_sha256s", "model_revision", "mask_png", "components", "boxes",
        "timings", "reported_cost_usd",
    })
    for key in ("protocol_version", "capability", "request_digest", "tile_id", "tile_rect", "graph_sha256s", "model_revision"):
        if result.get(key) != request.get(key):
            raise ContractValidationError("analysis result identity mismatch")
    components, boxes = result.get("components"), result.get("boxes")
    if type(components) is not list or len(components) > ANALYSIS_MAX_COMPONENTS or type(boxes) is not list or len(boxes) > ANALYSIS_MAX_BOXES:
        raise ContractValidationError("analysis result count exceeded")
    tile = request["tile_rect"]
    def inside(rect: Any) -> None:
        r = analysis_rect(rect)
        if r["x"] + r["width"] > tile["width"] or r["y"] + r["height"] > tile["height"]:
            raise ContractValidationError("analysis result outside tile")
    for component in components:
        inside(component)
    for box in boxes:
        if type(box) is not dict:
            raise ContractValidationError("invalid analysis box")
        _assert_no_extra_keys(box, {"rect", "class", "score"})
        inside(box.get("rect"))
        _check_exact_int(box.get("class"), "class", 0, 2)
        score = box.get("score")
        if type(score) not in (float, int) or type(score) is bool or not math.isfinite(score) or not 0 <= score <= 1:
            raise ContractValidationError("invalid analysis score")
    timings = result.get("timings")
    if type(timings) is not dict:
        raise ContractValidationError("invalid analysis timings")
    _assert_no_extra_keys(timings, {"load_ms", "preprocess_ms", "inference_ms", "postprocess_ms"})
    for field in ("load_ms", "preprocess_ms", "inference_ms", "postprocess_ms"):
        _check_exact_int(timings.get(field), field, 0, 2**32 - 1)
    _check_optional_float(result.get("reported_cost_usd"), "reported_cost_usd")
    mask = result.get("mask_png")
    if request["capability"] == ANALYSIS_SAM:
        if boxes or type(mask) is not bytes:
            raise ContractValidationError("invalid SAM result")
        _validate_analysis_png(mask, tile, "L")
    elif mask is not None or components:
        raise ContractValidationError("invalid RT result")


def validate_analysis_wire_result(result: Dict[str, Any], request: Dict[str, Any]) -> bytes | None:
    if len(json.dumps(result, separators=(",", ":")).encode("utf-8")) > ANALYSIS_MAX_RESPONSE_BYTES:
        raise ContractValidationError("analysis response too large")
    if "mask_png" in result or "mask_png_b64" not in result:
        raise ContractValidationError("invalid analysis mask encoding")
    encoded = result["mask_png_b64"]
    if encoded is not None:
        if type(encoded) is not str or len(encoded) > 4 * ((ANALYSIS_MAX_PNG_BYTES + 2) // 3):
            raise ContractValidationError("analysis encoded mask too large")
        try:
            mask = base64.b64decode(encoded, validate=True)
        except (ValueError, binascii.Error) as exc:
            raise ContractValidationError("invalid analysis mask encoding") from exc
    else:
        mask = None
    decoded = dict(result)
    decoded.pop("mask_png_b64")
    decoded["mask_png"] = mask
    validate_analysis_result(decoded, request)
    return mask


# Submit-then-poll GPU jobs for analysis tiles and denoise pages.
#
# Modal answers a web request still running after 150 s with a 303 redirect, and the
# desktop follows no redirect (src-tauri inference/http.rs), so a tile or page that
# queues or runs that long cannot be answered on the request that sent it. These
# routes split it: `POST /mc/{analysis,denoise}/v1/jobs` spawns the GPU call and
# answers at once with a handle, `GET .../jobs/{handle}` reads its state (and, once
# completed, the same result the synchronous route would have answered), and
# `POST .../jobs/{handle}/cancel` cancels it. Every request returns in seconds.
#
# Negotiation is additive: the gateway lists `analysis.jobs` / `denoise.jobs` among
# the operations of `GET /mc/v1/capabilities` when it serves these routes, and an app
# that finds them absent (an older gateway, or Beam) keeps using the synchronous
# routes, which are unchanged. No protocol version moves.
#
# The handle is derived from the request digest and a client attempt id, the same
# way on both sides (`cleaner_core::cloud_job_wire::job_handle`), so a submit the
# client repeats after a lost answer lands on the job it already started and never
# spawns a second GPU call.
GPU_JOB_OPERATIONS: Dict[str, str] = {
    "analysis": "analysis.jobs", "denoise": "denoise.jobs", "analysis_batch": "analysis.batches",
}
GPU_JOB_STATUSES = ("pending", "running", "completed", "failed", "cancelled")
GPU_JOB_TERMINAL = ("completed", "failed", "cancelled")
GPU_JOB_ATTEMPT_MIN = 16
GPU_JOB_ATTEMPT_MAX = 64
_ATTEMPT_CHARS = frozenset("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_")


def valid_gpu_job_attempt(value: Any) -> bool:
    """A client attempt id: 16 to 64 of [A-Za-z0-9_-]."""
    return (type(value) is str and GPU_JOB_ATTEMPT_MIN <= len(value) <= GPU_JOB_ATTEMPT_MAX
            and all(c in _ATTEMPT_CHARS for c in value))


def gpu_job_handle(kind: str, request_digest: str, attempt_id: str) -> str:
    """`<kind>-<32 hex>`: SHA-256 over kind, NUL, request digest, NUL, attempt id."""
    if kind not in GPU_JOB_OPERATIONS:
        raise ContractValidationError(f"unknown GPU job kind '{kind}'")
    digest = hashlib.sha256(f"{kind}\x00{request_digest}\x00{attempt_id}".encode("utf-8")).hexdigest()[:32]
    return f"{kind}-{digest}"


def valid_gpu_job_handle(kind: str, handle: Any) -> bool:
    prefix = f"{kind}-"
    return (type(handle) is str and handle.startswith(prefix) and len(handle) == len(prefix) + 32
            and all(c in "0123456789abcdef" for c in handle[len(prefix):]))


# A batch: several analysis requests of one source page in one GPU job
# (`POST /mc/analysis/v1/batches`, kind `analysis_batch`, operation
# `analysis.batches`). A tile job pays the job's own cost (spawn, queue wait, the
# job store) once per tile and model, about 2.3 s on Modal against 0.05 to 0.7 s
# of GPU work (docs/findings.md, 2026-10-01); a batch pays it once. Each distinct
# tile PNG travels once, however many models read it. The answer is each request's
# own analysis result, in request order. The client cuts a page into batches
# within these bounds (`cleaner_core::cloud_analysis_wire::batches`); an app that
# finds the operation missing keeps the tile jobs.
ANALYSIS_BATCH_MAX_REQUESTS = 16
ANALYSIS_BATCH_MAX_TILES = 8
ANALYSIS_BATCH_MAX_TILE_BYTES = 16_000_000
ANALYSIS_BATCH_MAX_BODY_BYTES = 24_000_000
ANALYSIS_BATCH_MAX_RESPONSE_BYTES = 24_000_000


def analysis_batch_digest(request_digests: list) -> str:
    """SHA-256 over the requests' digests, in order, each followed by a newline."""
    return hashlib.sha256("".join(f"{digest}\n" for digest in request_digests).encode("ascii")).hexdigest()


def validate_analysis_batch(metadata: Any, tiles: list) -> list:
    """A batch's (request, tile PNG) pairs in request order, or a raise.

    `metadata` is {protocol_version, request_digest, requests}; `tiles` are the
    distinct PNGs, each named by some request's `tile_png_sha256` and each used.
    Every request is validated against its PNG as a tile request is."""
    if type(metadata) is not dict:
        raise ContractValidationError("analysis batch must be an object")
    _assert_no_extra_keys(metadata, {"protocol_version", "request_digest", "requests"})
    if metadata.get("protocol_version") != ANALYSIS_VERSION:
        raise ContractValidationError("unsupported analysis protocol version")
    requests = metadata.get("requests")
    if type(requests) is not list or not 1 <= len(requests) <= ANALYSIS_BATCH_MAX_REQUESTS:
        raise ContractValidationError("analysis batch request count out of range")
    if type(tiles) is not list or not 1 <= len(tiles) <= ANALYSIS_BATCH_MAX_TILES:
        raise ContractValidationError("analysis batch tile count out of range")
    if sum(len(png) for png in tiles) > ANALYSIS_BATCH_MAX_TILE_BYTES:
        raise ContractValidationError("analysis batch tiles too large")
    if any(type(request) is not dict for request in requests):
        raise ContractValidationError("analysis batch request must be an object")
    digests = [request.get("request_digest") for request in requests]
    if len(set(map(str, digests))) != len(digests):
        raise ContractValidationError("analysis batch repeats a request")
    if len({request.get("source_page_sha256") for request in requests}) != 1:
        raise ContractValidationError("analysis batch spans pages")
    by_sha = {hashlib.sha256(png).hexdigest(): png for png in tiles}
    if len(by_sha) != len(tiles) or set(by_sha) != {request.get("tile_png_sha256") for request in requests}:
        raise ContractValidationError("analysis batch tiles do not match its requests")
    pairs = []
    for request in requests:
        png = by_sha[request["tile_png_sha256"]]
        validate_analysis_request(request, png)
        pairs.append((request, png))
    if _validate_lowercase_hex_hash(metadata.get("request_digest"), "request_digest") != analysis_batch_digest(digests):
        raise ContractValidationError("analysis batch digest mismatch")
    return pairs
