"""Credential and secret redaction engine for cloud provisioner.

Ensures no setup credentials, runtime secrets, API keys, or authorization tokens
leak into logs, journal state, error messages, or wire responses.
"""

import re
from typing import Any, Dict, List, Optional, Set, Union

REDACTED_PLACEHOLDER = "[REDACTED]"
REDACTED_CREDENTIAL = "[REDACTED_CREDENTIAL]"

# Exact sensitive key names (case-insensitive)
EXACT_SENSITIVE_KEYS = {
    "token",
    "secret",
    "password",
    "api_key",
    "apikey",
    "api_token",
    "access_token",
    "refresh_token",
    "setup_token",
    "setup_credential",
    "token_secret",
    "token_id",
    "client_secret",
    "proxy_token_secret",
    "private_key",
    "authorization",
    "auth_token",
    "beam_token",
    "modal_token_secret",
    "modal_token_id",
}

PARTIAL_SENSITIVE_PATTERNS = [
    re.compile(r"secret", re.IGNORECASE),
    re.compile(r"password", re.IGNORECASE),
    re.compile(r"credential", re.IGNORECASE),
    re.compile(r"private_key", re.IGNORECASE),
    re.compile(r"token", re.IGNORECASE),
]

# Keys ending with these suffixes are safe metadata, NOT sensitive credentials.
# Note: '_id' is intentionally omitted because token/secret/credential IDs (e.g. custom_token_id)
# are sensitive authentication identifiers requiring full redaction, while benign IDs like
# profile_id, installation_id, or resource_id match no sensitive patterns and remain safe.
SAFE_KEY_SUFFIXES = (
    "_forgotten",
    "_exists",
    "_present",
    "_count",
    "_type",
    "_status",
    "_name",
    "_version",
    "_url",
    "_uri",
    "_ref",
)

# A token prefix only starts a token when it is not the tail of a longer name:
# "studio-ws--mc-app.modal.run" is a Modal host for the workspace "studio-ws".
_TOKEN_START = r"(?<![\w.-])"

# Patterns for known token structures in strings
TOKEN_REGEX_PATTERNS = [
    # Modal token secret: as- followed by alphanumeric/underscore/hyphen
    (re.compile(_TOKEN_START + r"(as-[a-zA-Z0-9_-]{8,})\b"), "as-[REDACTED]"),
    # Modal token ID: ak- followed by alphanumeric/underscore/hyphen
    (re.compile(_TOKEN_START + r"(ak-[a-zA-Z0-9_-]{8,})\b"), "ak-[REDACTED]"),
    # Modal proxy token secret. The matching wk- token ID is not a secret and stays
    # readable: the journal keeps it so cleanup can delete the token.
    (re.compile(_TOKEN_START + r"(ws-[a-zA-Z0-9_-]{8,})\b"), "ws-[REDACTED]"),
    # Beam token: b9_ followed by alphanumeric/underscore/hyphen
    (re.compile(_TOKEN_START + r"(b9_[a-zA-Z0-9_-]{8,})\b"), "b9_[REDACTED]"),
    # Beam token variant: beam_ followed by alphanumeric/underscore/hyphen
    (re.compile(_TOKEN_START + r"(beam_[a-zA-Z0-9_-]{8,})\b"), "beam_[REDACTED]"),
    # HTTP Authorization: Bearer <token>
    (re.compile(r"(Bearer\s+)[a-zA-Z0-9_\-\.]{8,}", re.IGNORECASE), r"\1[REDACTED]"),
    # Key-value secret assignments (e.g. token=xyz or "secret": "xyz")
    (
        re.compile(
            r'((?:token|secret|password|api_key|access_token|api_token|auth_token|beam_token|modal_token_secret)["\']?\s*[:=]\s*["\']?)[a-zA-Z0-9_\-\.]{6,}(["\']?)',
            re.IGNORECASE,
        ),
        r"\1[REDACTED]\2",
    ),
    # URL query parameter credentials
    (
        re.compile(r'([?&](?:token|secret|key|sig|signature|auth|apikey|api_key)=)[^&\s"\']+', re.IGNORECASE),
        r"\1[REDACTED]",
    ),
]


class RedactionRegistry:
    """Session registry for explicitly tracking and redacting known credential values."""

    def __init__(self) -> None:
        self._registered_secrets: Set[str] = set()

    def register(self, secret: Optional[str]) -> None:
        """Register a secret string for exact redaction."""
        if secret and isinstance(secret, str) and len(secret.strip()) >= 4:
            self._registered_secrets.add(secret.strip())

    def unregister(self, secret: Optional[str]) -> None:
        """Unregister a secret if explicitly forgotten."""
        if secret in self._registered_secrets:
            self._registered_secrets.remove(secret)

    def clear(self) -> None:
        """Clear all registered secrets."""
        self._registered_secrets.clear()

    @property
    def registered_secrets(self) -> Set[str]:
        return set(self._registered_secrets)


# Global default registry for helper session
GLOBAL_REGISTRY = RedactionRegistry()


def is_sensitive_key(key: str) -> bool:
    """Check if a dictionary key name indicates sensitive credential data."""
    if not isinstance(key, str):
        return False
    norm_key = key.lower().strip()

    # Exact sensitive keys always take precedence over safe suffix matching
    if norm_key in EXACT_SENSITIVE_KEYS:
        return True

    # If it ends with a safe metadata suffix, it's not a sensitive credential key
    if norm_key.endswith(SAFE_KEY_SUFFIXES):
        return False

    return any(pattern.search(norm_key) for pattern in PARTIAL_SENSITIVE_PATTERNS)


def redact_string(
    text: str,
    additional_secrets: Optional[Set[str]] = None,
    patterns: bool = True,
) -> str:
    """Redact registered secrets and, unless `patterns` is False, token-shaped text."""
    if not isinstance(text, str) or not text:
        return text

    result = text

    # Redact explicitly registered secrets first
    all_secrets = set(GLOBAL_REGISTRY.registered_secrets)
    if additional_secrets:
        all_secrets.update(additional_secrets)

    for secret in sorted(all_secrets, key=len, reverse=True):
        if secret in result:
            result = result.replace(secret, REDACTED_CREDENTIAL)

    if patterns:
        for pattern, replacement in TOKEN_REGEX_PATTERNS:
            result = pattern.sub(replacement, result)

    return result


def redact_data(
    data: Any,
    additional_secrets: Optional[Set[str]] = None,
    patterns: bool = True,
) -> Any:
    """Recursively redact secret-named keys, registered secrets and (with `patterns`) token-shaped text.

    Records that must round-trip exactly, like the journal, pass patterns=False: a
    user-chosen workspace or environment name can look like a token.
    """
    if data is None:
        return None

    # Preserve booleans and numbers exactly (do not redact True/False or integers)
    if isinstance(data, bool) or isinstance(data, (int, float)):
        return data

    if isinstance(data, str):
        return redact_string(data, additional_secrets, patterns)

    if isinstance(data, dict):
        redacted_dict: Dict[str, Any] = {}
        for k, v in data.items():
            str_key = str(k)

            # Preserve None, booleans and numbers even under sensitive-looking keys (e.g. status flags)
            if v is None:
                redacted_dict[k] = None
                continue

            if isinstance(v, bool) or isinstance(v, (int, float)):
                redacted_dict[k] = v
                continue

            if is_sensitive_key(str_key):
                if isinstance(v, str):
                    redacted_dict[k] = REDACTED_CREDENTIAL
                elif isinstance(v, dict):
                    redacted_dict[k] = redact_data(v, additional_secrets, patterns)
                elif isinstance(v, list):
                    redacted_dict[k] = [
                        REDACTED_CREDENTIAL if isinstance(item, str) else redact_data(item, additional_secrets, patterns)
                        for item in v
                    ]
                else:
                    redacted_dict[k] = REDACTED_CREDENTIAL
            else:
                redacted_dict[k] = redact_data(v, additional_secrets, patterns)
        return redacted_dict

    if isinstance(data, (list, tuple, set)):
        items = [redact_data(item, additional_secrets, patterns) for item in data]
        if isinstance(data, tuple):
            return tuple(items)
        if isinstance(data, set):
            return set(items)
        return items

    if isinstance(data, Exception):
        return redact_string(str(data), additional_secrets, patterns)

    return data
