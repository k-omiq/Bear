"""Strict versioned allowlisted helper message protocol.

Enforces:
1. Pinned protocol version (1.0.0).
2. Strict allowlisted operations and providers.
3. Envelope schema validation with denial of unknown fields.
4. Input bounds and rejection of path traversal, null bytes, or oversized payloads.
5. Structured errors with actionable missing-permission guidance.
6. Automatic credential redaction on all serialized responses. The one exception is
   the IC-1 carve-out: a successful apply or resume returns the runtime credential and
   endpoint URL it just issued, inserted after redaction (see `make_success_response`).
"""

from dataclasses import asdict, dataclass, field
import json
import re
from typing import Any, Dict, List, Optional, Set, Union
from urllib.parse import urlparse

from provisioner.redaction import redact_data, redact_string

HELPER_PROTOCOL_VERSION = "1.0.0"
MAX_REQUEST_BYTES = 64 * 1024  # 64 KiB envelope limit

# Allowlisted Operations
OP_INSPECT = "inspect"
OP_PLAN = "plan"
OP_APPLY = "apply"
OP_RESUME = "resume"
OP_CLEANUP_PLAN = "cleanup_plan"
OP_CLEANUP_APPLY = "cleanup_apply"
OP_FORGET_CREDENTIAL = "forget_credential"
OP_PROBE_COMPATIBILITY = "probe_compatibility"

ALLOWLISTED_OPERATIONS: Set[str] = {
    OP_INSPECT,
    OP_PLAN,
    OP_APPLY,
    OP_RESUME,
    OP_CLEANUP_PLAN,
    OP_CLEANUP_APPLY,
    OP_FORGET_CREDENTIAL,
    OP_PROBE_COMPATIBILITY,
}

# Allowlisted Providers
PROVIDER_MODAL = "modal"
PROVIDER_BEAM = "beam"
ALLOWLISTED_PROVIDERS: Set[str] = {PROVIDER_MODAL, PROVIDER_BEAM}

# Standardized Protocol Error Codes
ERR_INVALID_VERSION = "ERR_INVALID_PROTOCOL_VERSION"
ERR_UNSUPPORTED_OP = "ERR_UNSUPPORTED_OPERATION"
ERR_UNSUPPORTED_PROVIDER = "ERR_UNSUPPORTED_PROVIDER"
ERR_INVALID_PAYLOAD = "ERR_INVALID_REQUEST_PAYLOAD"
ERR_PAYLOAD_TOO_LARGE = "ERR_PAYLOAD_TOO_LARGE"
ERR_VALIDATION = "ERR_VALIDATION_ERROR"
ERR_SECURITY_VIOLATION = "ERR_SECURITY_VIOLATION"
ERR_UNAPPROVED_PLAN = "ERR_UNAPPROVED_PLAN"
ERR_ACTIONABLE_PERMISSION = "ERR_ACTIONABLE_MISSING_PERMISSION"
ERR_PLATFORM_GATED = "ERR_PLATFORM_GATED"
ERR_PROVIDER_UNAVAILABLE = "ERR_PROVIDER_UNAVAILABLE"
ERR_EXECUTION_FAILED = "ERR_EXECUTION_FAILED"
ERR_EXECUTION_TIMEOUT = "ERR_EXECUTION_TIMEOUT"
ERR_ORPHANED_TOKEN = "ERR_ORPHANED_TOKEN"

# The only data fields a response may carry unredacted (IC-1).
VERBATIM_FIELDS = frozenset({"runtime_credential", "endpoint_url"})

IDENTIFIER_REGEX = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")
HEX_HASH_REGEX = re.compile(r"^[a-fA-F0-9]{64}$")
PATH_TRAVERSAL_REGEX = re.compile(r"(\.\./|\.\.\\|/\.\.|\\\.\.|^\.\.$|^[/\\])")


class ProtocolError(Exception):
    """Protocol-level validation or processing error."""

    def __init__(
        self,
        code: str,
        message: str,
        actionable_guidance: Optional[str] = None,
        remedy_steps: Optional[List[str]] = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.actionable_guidance = actionable_guidance
        self.remedy_steps = remedy_steps or []


@dataclass(frozen=True)
class HelperRequest:
    protocol_version: str
    request_id: str
    op: str
    provider: str
    params: Dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class HelperResponse:
    protocol_version: str
    request_id: str
    success: bool
    data: Optional[Dict[str, Any]] = None
    error: Optional[Dict[str, Any]] = None
    # IC-1 carve-out, merged into `data` after redaction. Never part of to_dict().
    verbatim: Optional[Dict[str, Any]] = None

    def to_dict(self) -> Dict[str, Any]:
        res: Dict[str, Any] = {
            "protocol_version": self.protocol_version,
            "request_id": self.request_id,
            "success": self.success,
        }
        if self.data is not None:
            res["data"] = self.data
        if self.error is not None:
            res["error"] = self.error
        return res

    def serialize(self) -> str:
        """Serialize response to JSON with automatic credential redaction."""
        raw_dict = self.to_dict()
        sanitized = redact_data(raw_dict)
        if self.success and self.verbatim and isinstance(sanitized.get("data"), dict):
            sanitized["data"].update(self.verbatim)
        return json.dumps(sanitized, indent=2, sort_keys=True)


def validate_identifier(name: str, value: str) -> None:
    """Ensure identifier matches safe bounded format and has no path traversal."""
    if not isinstance(value, str):
        raise ProtocolError(
            ERR_VALIDATION,
            f"Field '{name}' must be a string, got {type(value).__name__}",
        )
    if "\0" in value:
        raise ProtocolError(
            ERR_SECURITY_VIOLATION,
            f"Null byte detected in field '{name}'",
        )
    if not IDENTIFIER_REGEX.match(value):
        raise ProtocolError(
            ERR_VALIDATION,
            f"Field '{name}' contains invalid characters or invalid length (must match ^[a-zA-Z0-9_-]{{1,64}}$)",
        )
    if ".." in value or "/" in value or "\\" in value:
        raise ProtocolError(
            ERR_SECURITY_VIOLATION,
            f"Path traversal sequence detected in field '{name}'",
        )


def validate_hash(name: str, value: str) -> None:
    """Ensure hash matches standard 64-character hex format."""
    if not isinstance(value, str) or not HEX_HASH_REGEX.match(value):
        raise ProtocolError(
            ERR_VALIDATION,
            f"Field '{name}' must be a 64-character hexadecimal SHA-256 hash",
        )


def validate_https_url(name: str, value: Any) -> str:
    """Ensure endpoint_url is a non-empty, valid HTTPS URL; fail closed."""
    if not isinstance(value, str):
        raise ProtocolError(
            ERR_VALIDATION,
            f"Field '{name}' must be a string, got {type(value).__name__}",
        )
    trimmed = value.strip()
    if not trimmed:
        raise ProtocolError(
            ERR_VALIDATION,
            f"Field '{name}' must not be empty",
        )
    if "\0" in trimmed:
        raise ProtocolError(
            ERR_SECURITY_VIOLATION,
            f"Null byte detected in field '{name}'",
        )
    if any(ord(c) < 32 or ord(c) == 127 for c in trimmed):
        raise ProtocolError(
            ERR_SECURITY_VIOLATION,
            f"Control character detected in field '{name}'",
        )
    if any(c.isspace() for c in trimmed):
        raise ProtocolError(
            ERR_SECURITY_VIOLATION,
            f"Whitespace character detected in field '{name}'",
        )
    if not trimmed.lower().startswith("https://"):
        raise ProtocolError(
            ERR_VALIDATION,
            f"Field '{name}' must be a secure HTTPS URL, got '{trimmed}'",
        )
    parsed = urlparse(trimmed)
    if parsed.scheme.lower() != "https" or not parsed.netloc:
        raise ProtocolError(
            ERR_VALIDATION,
            f"Field '{name}' must be a valid HTTPS URL with hostname: '{trimmed}'",
        )
    return trimmed


def parse_request(raw_input: Union[str, bytes, Dict[str, Any]]) -> HelperRequest:
    """Parse and strictly validate a helper protocol request."""
    if isinstance(raw_input, bytes):
        if len(raw_input) > MAX_REQUEST_BYTES:
            raise ProtocolError(
                ERR_PAYLOAD_TOO_LARGE,
                f"Request payload size ({len(raw_input)} bytes) exceeds maximum limit of {MAX_REQUEST_BYTES} bytes",
            )
        try:
            raw_text = raw_input.decode("utf-8")
        except UnicodeDecodeError as e:
            raise ProtocolError(
                ERR_INVALID_PAYLOAD,
                f"Payload is not valid UTF-8: {e}",
            )
        try:
            data = json.loads(raw_text)
        except json.JSONDecodeError as e:
            raise ProtocolError(
                ERR_INVALID_PAYLOAD,
                f"Malformed JSON payload: {e}",
            )
    elif isinstance(raw_input, str):
        if len(raw_input.encode("utf-8")) > MAX_REQUEST_BYTES:
            raise ProtocolError(
                ERR_PAYLOAD_TOO_LARGE,
                f"Request payload string exceeds maximum limit of {MAX_REQUEST_BYTES} bytes",
            )
        try:
            data = json.loads(raw_input)
        except json.JSONDecodeError as e:
            raise ProtocolError(
                ERR_INVALID_PAYLOAD,
                f"Malformed JSON payload: {e}",
            )
    elif isinstance(raw_input, dict):
        data = raw_input
    else:
        raise ProtocolError(
            ERR_INVALID_PAYLOAD,
            f"Expected string, bytes, or dict for request, got {type(raw_input).__name__}",
        )

    if not isinstance(data, dict):
        raise ProtocolError(
            ERR_INVALID_PAYLOAD,
            "Request envelope must be a JSON object",
        )

    # Deny unknown envelope fields
    allowed_fields = {"protocol_version", "request_id", "op", "provider", "params"}
    extra_fields = set(data.keys()) - allowed_fields
    if extra_fields:
        raise ProtocolError(
            ERR_VALIDATION,
            f"Envelope contains unknown fields: {sorted(list(extra_fields))}",
        )

    # 1. Validate protocol version
    version = data.get("protocol_version")
    if not version or version != HELPER_PROTOCOL_VERSION:
        raise ProtocolError(
            ERR_INVALID_VERSION,
            f"Unsupported protocol version: '{version}'. Expected '{HELPER_PROTOCOL_VERSION}'",
            actionable_guidance="Upgrade helper client to match the expected protocol version.",
        )

    # 2. Validate request ID
    request_id = data.get("request_id")
    if not request_id:
        raise ProtocolError(ERR_VALIDATION, "Missing required field: 'request_id'")
    validate_identifier("request_id", request_id)

    # 3. Validate operation
    op = data.get("op")
    if not op or op not in ALLOWLISTED_OPERATIONS:
        raise ProtocolError(
            ERR_UNSUPPORTED_OP,
            f"Unsupported operation '{op}'. Allowlisted operations: {sorted(list(ALLOWLISTED_OPERATIONS))}",
            actionable_guidance=f"Operation must be one of {sorted(list(ALLOWLISTED_OPERATIONS))}",
        )

    # 4. Validate provider
    provider = data.get("provider")
    if not provider or provider not in ALLOWLISTED_PROVIDERS:
        raise ProtocolError(
            ERR_UNSUPPORTED_PROVIDER,
            f"Unsupported provider '{provider}'. Allowlisted providers: {sorted(list(ALLOWLISTED_PROVIDERS))}",
            actionable_guidance=f"Provider must be one of {sorted(list(ALLOWLISTED_PROVIDERS))}",
        )

    # 5. Validate params
    params = data.get("params", {})
    if not isinstance(params, dict):
        raise ProtocolError(ERR_VALIDATION, "Field 'params' must be a dictionary")

    # Deep scan strings in params for path traversal or null bytes
    _validate_param_values(params)

    return HelperRequest(
        protocol_version=version,
        request_id=request_id,
        op=op,
        provider=provider,
        params=params,
    )


def _validate_param_values(val: Any, path: str = "params") -> None:
    """Recursively validate that parameter values are safe."""
    if isinstance(val, str):
        if "\0" in val:
            raise ProtocolError(
                ERR_SECURITY_VIOLATION,
                f"Null byte detected in parameter '{path}'",
            )
        if PATH_TRAVERSAL_REGEX.search(val):
            raise ProtocolError(
                ERR_SECURITY_VIOLATION,
                f"Path traversal sequence detected in parameter '{path}'",
            )
        if path.endswith("installation_id"):
            validate_identifier("installation_id", val)
    elif isinstance(val, dict):
        for k, v in val.items():
            if not isinstance(k, str):
                raise ProtocolError(
                    ERR_VALIDATION,
                    f"Dictionary key in '{path}' must be string",
                )
            if "\0" in k or PATH_TRAVERSAL_REGEX.search(k):
                raise ProtocolError(
                    ERR_SECURITY_VIOLATION,
                    f"Illegal characters in parameter key '{k}'",
                )
            _validate_param_values(v, f"{path}.{k}")
    elif isinstance(val, list):
        for idx, item in enumerate(val):
            _validate_param_values(item, f"{path}[{idx}]")


def make_success_response(
    request_id: str,
    data: Dict[str, Any],
    verbatim: Optional[Dict[str, Any]] = None,
) -> HelperResponse:
    """Create a standardized success response.

    `verbatim` is the IC-1 carve-out: the runtime credential and endpoint URL that a
    successful apply or resume hands to the desktop. Everything else in `data` is
    redacted as usual.
    """
    if verbatim is not None and not set(verbatim) <= VERBATIM_FIELDS:
        raise ValueError(f"only {sorted(VERBATIM_FIELDS)} may skip redaction")
    return HelperResponse(
        protocol_version=HELPER_PROTOCOL_VERSION,
        request_id=request_id,
        success=True,
        data=data,
        error=None,
        verbatim=dict(verbatim) if verbatim else None,
    )


def make_error_response(
    request_id: str,
    code: str,
    message: str,
    actionable_guidance: Optional[str] = None,
    remedy_steps: Optional[List[str]] = None,
) -> HelperResponse:
    """Create a standardized error response with actionable guidance."""
    return HelperResponse(
        protocol_version=HELPER_PROTOCOL_VERSION,
        request_id=request_id,
        success=False,
        data=None,
        error={
            "code": code,
            "message": redact_string(message),
            "actionable_guidance": redact_string(actionable_guidance) if actionable_guidance else None,
            "remedy_steps": [redact_string(s) for s in remedy_steps] if remedy_steps else [],
        },
    )
