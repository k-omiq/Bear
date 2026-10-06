"""Secret and Presigned URL Redaction Utilities.

Enforces zero leakage of secrets, auth headers, private tokens, and presigned
storage URLs in logging, error messages, and API response payloads.
"""

from __future__ import annotations

import re
from typing import Any, Dict, List, Optional, Set, Union

REDACTED_SECRET: str = "[REDACTED]"
REDACTED_PRESIGNED_URL: str = "[REDACTED_PRESIGNED_URL]"

# Regex patterns for sensitive authorization headers and tokens
AUTH_HEADER_PATTERNS: List[re.Pattern] = [
    re.compile(r"(?i)(authorization\s*:\s*bearer\s+)([a-zA-Z0-9_\-\.~+/]+=*)"),
    re.compile(r"(?i)(bearer\s+)([a-zA-Z0-9_\-\.~+/]+=*)"),
    re.compile(r"(?i)(modal-key\s*:\s*)([^\s,;]+)"),
    re.compile(r"(?i)(modal-secret\s*:\s*)([^\s,;]+)"),
    re.compile(r"(?i)(x-beam-token\s*:\s*)([^\s,;]+)"),
    re.compile(r"(?i)(api[_-]?key\s*[:=]\s*['\"]?)([a-zA-Z0-9_\-\.~+/]{8,})(['\"]?)"),
    re.compile(r"(?i)(secret\s*[:=]\s*['\"]?)([a-zA-Z0-9_\-\.~+/]{8,})(['\"]?)"),
    re.compile(r"(?i)(token\s*[:=]\s*['\"]?)([a-zA-Z0-9_\-\.~+/]{8,})(['\"]?)"),
]

# Regex patterns for presigned storage URLs (S3, GCS X-Goog, Azure Blob, Beam/Modal internal storage)
PRESIGNED_URL_PATTERN: re.Pattern = re.compile(
    r"(?i)https?://[^\s\"'<>]+(?:\?|&)(?:X-Goog-[a-zA-Z0-9_-]+|GoogleAccessId|X-Amz-[a-zA-Z0-9_-]+|AWSAccessKeyId|Signature|sig|access_token|token|se|sp|sv|sr)=[^\s\"'<>&]+[^\s\"'<>]*"
)

# Common sensitive dictionary keys to redact
SENSITIVE_KEY_NAMES: Set[str] = {
    "token",
    "secret",
    "modal_key",
    "modal_secret",
    "bearer_token",
    "api_key",
    "password",
    "presigned_url",
    "signed_url",
    "storage_url",
    "internal_storage_ref",
    "access_token",
    "refresh_token",
    "private_key",
    "gcs_signed_url",
    "x_goog_signature",
    "x-goog-signature",
    "x_amz_signature",
    "x-amz-signature",
}


def redact_presigned_urls(text: str) -> str:
    """Replace presigned storage URLs with a safe redaction token."""
    if not isinstance(text, str):
        return text
    return PRESIGNED_URL_PATTERN.sub(REDACTED_PRESIGNED_URL, text)


def redact_text(text: str, custom_secrets: Optional[List[str]] = None) -> str:
    """Redact auth headers, tokens, presigned URLs, and custom secrets from a string."""
    if not isinstance(text, str) or not text:
        return text

    result = redact_presigned_urls(text)

    for pattern in AUTH_HEADER_PATTERNS:
        # If pattern has 3 groups (prefix, secret, suffix), preserve prefix/suffix
        if pattern.groups == 3:
            result = pattern.sub(rf"\g<1>{REDACTED_SECRET}\g<3>", result)
        elif pattern.groups == 2:
            result = pattern.sub(rf"\g<1>{REDACTED_SECRET}", result)
        else:
            result = pattern.sub(REDACTED_SECRET, result)

    if custom_secrets:
        for secret in custom_secrets:
            if secret and len(secret) >= 4:
                result = result.replace(secret, REDACTED_SECRET)

    return result


def redact_dict(
    data: Any,
    sensitive_keys: Optional[Set[str]] = None,
    custom_secrets: Optional[List[str]] = None,
) -> Any:
    """Recursively redact sensitive keys and values from dictionaries and lists."""
    keys_to_check = sensitive_keys or SENSITIVE_KEY_NAMES

    if isinstance(data, dict):
        sanitized: Dict[str, Any] = {}
        for k, v in data.items():
            if str(k).lower() in keys_to_check:
                sanitized[k] = REDACTED_SECRET
            elif isinstance(v, (dict, list)):
                sanitized[k] = redact_dict(v, keys_to_check, custom_secrets)
            elif isinstance(v, str):
                sanitized[k] = redact_text(v, custom_secrets)
            else:
                sanitized[k] = v
        return sanitized
    elif isinstance(data, list):
        return [redact_dict(item, keys_to_check, custom_secrets) for item in data]
    elif isinstance(data, str):
        return redact_text(data, custom_secrets)
    return data
