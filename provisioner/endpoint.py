"""The installed endpoint as the desktop will see it.

Setup ends with GET /health and GET /model-info through the provider's edge with the
runtime credential the desktop will store, so a finished install is known to answer.
Both routes are served by the CPU gateway from the job store and never start a GPU.
"""

from __future__ import annotations

import http.client
import ipaddress
import json
import time
import urllib.error
import urllib.request
from typing import Any, Callable, Dict, Optional, Tuple
from urllib.parse import urlsplit

from provisioner.driver_base import Deadline, production_model
from provisioner.protocol import (
    ERR_EXECUTION_FAILED,
    ERR_EXECUTION_TIMEOUT,
    ERR_SECURITY_VIOLATION,
    ERR_VALIDATION,
    ProtocolError,
)

from deploy.cloud.common.contract import (
    ContractValidationError,
    HealthResponse,
    ModelInfoResponse,
    validate_health_response,
    validate_model_info_response,
)
from deploy.cloud.common.manifest import PRODUCTION_MODELS

API_PATH = "/mc/v1"
MAX_BODY_BYTES = 64 * 1024
REQUEST_TIMEOUT_SECONDS = 30.0
RETRY_DELAY_SECONDS = 3.0
# A new credential or a new deployment can take a little while to be accepted at the edge.
AUTH_GRACE_SECONDS = 90.0
MAX_TOKEN_CHARS = 4096

_BLOCKED_SUFFIXES = (".local", ".internal", ".arpa", ".localhost")

# (url, headers, timeout) -> (HTTP status or None when there was no HTTP answer, body)
HttpGet = Callable[[str, Dict[str, str], float], Tuple[Optional[int], bytes]]


def _clean_secret(value: Any, name: str, limit: int = MAX_TOKEN_CHARS) -> str:
    if not isinstance(value, str) or not value or len(value) > limit:
        raise ProtocolError(ERR_VALIDATION, f"Runtime credential field '{name}' is missing or too long")
    if any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in value):
        raise ProtocolError(ERR_SECURITY_VIOLATION, f"Runtime credential field '{name}' has whitespace or control characters")
    return value


def runtime_credential(kind: str, **fields: Any) -> Dict[str, str]:
    """The IC-1 runtime credential, checked the way the desktop checks it."""
    if kind == "modal_proxy":
        return {
            "kind": kind,
            "token_id": _clean_secret(fields.get("token_id"), "token_id", 256),
            "token_secret": _clean_secret(fields.get("token_secret"), "token_secret", 256),
        }
    if kind == "beam_bearer":
        return {"kind": kind, "token": _clean_secret(fields.get("token"), "token")}
    raise ProtocolError(ERR_VALIDATION, f"Unsupported runtime credential kind: '{kind}'")


def parse_runtime_credential(value: Any) -> Dict[str, str]:
    if not isinstance(value, dict):
        raise ProtocolError(ERR_VALIDATION, "Parameter 'runtime_credential' must be an object")
    fields = {k: v for k, v in value.items() if k != "kind"}
    return runtime_credential(str(value.get("kind", "")), **fields)


def auth_headers(credential: Dict[str, str]) -> Dict[str, str]:
    if credential["kind"] == "modal_proxy":
        return {"Modal-Key": credential["token_id"], "Modal-Secret": credential["token_secret"]}
    return {"Authorization": f"Bearer {credential['token']}"}


def validate_endpoint_url(url: Any) -> str:
    """The exact https://<host>/mc/v1 base the desktop accepts."""
    if not isinstance(url, str) or not url or len(url) > 2048:
        raise ProtocolError(ERR_VALIDATION, "Endpoint URL is missing or too long")
    if any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in url):
        raise ProtocolError(ERR_SECURITY_VIOLATION, "Endpoint URL has whitespace or control characters")
    try:
        parts = urlsplit(url)
        host = (parts.hostname or "").lower()
        parts.port  # noqa: B018 - raises ValueError for a malformed port
    except ValueError:
        raise ProtocolError(ERR_VALIDATION, f"Endpoint URL is not a valid URL: '{url}'") from None
    if parts.scheme != "https" or not host:
        raise ProtocolError(ERR_VALIDATION, f"Endpoint URL must be https with a host name: '{url}'")
    if parts.username or parts.password or parts.query or parts.fragment or "@" in parts.netloc:
        raise ProtocolError(ERR_SECURITY_VIOLATION, "Endpoint URL must not carry user info, a query or a fragment")
    try:
        ipaddress.ip_address(host.strip("[]"))
        is_ip = True
    except ValueError:
        is_ip = False
    if is_ip or "." not in host or host == "localhost" or host.endswith(_BLOCKED_SUFFIXES):
        raise ProtocolError(ERR_VALIDATION, f"Endpoint URL must use a public domain name: '{url}'")
    if parts.path.rstrip("/") != API_PATH:
        raise ProtocolError(ERR_VALIDATION, f"Endpoint URL path must be {API_PATH}: '{url}'")
    return f"https://{parts.netloc}{API_PATH}"


def endpoint_from_base(base_url: Any, what: str) -> str:
    """https://<host> (with at most a trailing slash) plus /mc/v1."""
    if not isinstance(base_url, str) or not base_url:
        raise ProtocolError(ERR_EXECUTION_FAILED, f"The provider returned no URL for {what}")
    try:
        parts = urlsplit(base_url.strip())
    except ValueError:
        raise ProtocolError(ERR_EXECUTION_FAILED, f"The provider returned a malformed URL for {what}: '{base_url}'") from None
    if parts.path not in ("", "/"):
        raise ProtocolError(
            ERR_EXECUTION_FAILED,
            f"The provider returned a URL with a path for {what}: '{base_url}'. The app needs a host of its own.",
        )
    # The scheme is kept, so a plain http URL fails validation instead of being upgraded.
    return validate_endpoint_url(f"{parts.scheme}://{parts.netloc}{API_PATH}")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect could carry the credential headers to another host."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: D102
        return None


def urllib_get(url: str, headers: Dict[str, str], timeout: float) -> Tuple[Optional[int], bytes]:
    opener = urllib.request.build_opener(_NoRedirect())
    request = urllib.request.Request(
        url,
        headers={**headers, "Accept": "application/json", "User-Agent": "manga-cleaner-provisioner"},
        method="GET",
    )
    try:
        with opener.open(request, timeout=timeout) as response:
            return response.status, response.read(MAX_BODY_BYTES + 1)
    except urllib.error.HTTPError as exc:
        try:
            body = exc.read(MAX_BODY_BYTES + 1)
        except Exception:
            body = b""
        finally:
            exc.close()
        return exc.code, body
    except (urllib.error.URLError, http.client.HTTPException, OSError, ValueError):
        return None, b""


def _json(body: bytes) -> Any:
    if len(body) > MAX_BODY_BYTES:
        raise ValueError("response too large")
    return json.loads(body.decode("utf-8"))


def check_endpoint(
    endpoint_url: str,
    credential: Dict[str, str],
    *,
    provider: str,
    deadline: Deadline,
    http: Optional[HttpGet] = None,
    sleep: Callable[[float], None] = time.sleep,
    clock: Callable[[], float] = time.monotonic,
    auth_grace_seconds: float = AUTH_GRACE_SECONDS,
    model_id: Optional[str] = None,
) -> Dict[str, Any]:
    """Wait until /health answers ok through the edge, then check /model-info.

    `model_id` is the model the installation was set up with, which the endpoint
    must serve. Without one (a probe of an endpoint whose choices are not known)
    any production model passes.

    Returns the health summary the desktop shows. Raises ERR_EXECUTION_TIMEOUT when
    the endpoint never answered, ERR_EXECUTION_FAILED when it answered but refused the
    credential or serves a different model or protocol.
    """
    get = http or urllib_get
    base = validate_endpoint_url(endpoint_url)
    headers = auth_headers(credential)
    allowed = [production_model(model_id)] if model_id else [production_model(known) for known in PRODUCTION_MODELS]
    refused_since: Optional[float] = None
    last = "no answer yet"
    while True:
        started = clock()
        status, body = get(base + "/health", headers, max(1.0, min(REQUEST_TIMEOUT_SECONDS, deadline.remaining())))
        latency_ms = round((clock() - started) * 1000.0, 1)
        if status == 200:
            try:
                health = HealthResponse.from_dict(_json(body))
                validate_health_response(health)
                if health.provider != provider:
                    raise ContractValidationError(f"endpoint reports provider '{health.provider}'")
            except (ContractValidationError, ValueError) as exc:
                raise ProtocolError(ERR_EXECUTION_FAILED, f"The endpoint answered /health with an unexpected body: {exc}") from None
            info_status, info_body = get(base + "/model-info", headers, max(1.0, min(REQUEST_TIMEOUT_SECONDS, deadline.remaining())))
            if info_status == 200:
                try:
                    info = ModelInfoResponse.from_dict(_json(info_body))
                    validate_model_info_response(info)
                except (ContractValidationError, ValueError) as exc:
                    raise ProtocolError(ERR_EXECUTION_FAILED, f"The endpoint answered /model-info with an unexpected body: {exc}") from None
                served = {key: getattr(info, key) for key in allowed[0]}
                if served not in allowed:
                    raise ProtocolError(
                        ERR_EXECUTION_FAILED,
                        f"The endpoint serves a different model or recipe: {served}",
                        actionable_guidance="Clean up this installation and set it up again.",
                    )
                return {
                    "ok": True,
                    "status": "compatible",
                    "http_status": 200,
                    "latency_ms": latency_ms,
                    "provider": health.provider,
                    "protocol_version": health.protocol_version,
                    "model": served,
                }
            last = f"/model-info answered HTTP {info_status}" if info_status else "/model-info did not answer"
        elif status in (401, 403):
            refused_since = started if refused_since is None else refused_since
            last = f"HTTP {status} from the provider edge"
            if clock() - refused_since >= auth_grace_seconds:
                raise ProtocolError(
                    ERR_EXECUTION_FAILED,
                    f"The endpoint refused the runtime credential ({last})",
                    actionable_guidance="Run Resume to issue a new access token.",
                )
        else:
            last = f"HTTP {status}" if status else "no answer yet"
        if deadline.remaining() <= RETRY_DELAY_SECONDS:
            raise ProtocolError(
                ERR_EXECUTION_TIMEOUT,
                f"The endpoint did not answer in time ({last})",
                actionable_guidance="The installation keeps starting in the cloud. Run Resume in a few minutes.",
            )
        sleep(RETRY_DELAY_SECONDS)
