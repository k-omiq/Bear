"""Builders and fakes shared by the offline deploy tests.

Nothing here touches the network or a provider SDK. The fakes model only the calls
the deploy code makes, with the semantics the SDK sources show (see each class).
"""

from __future__ import annotations

import asyncio
import dataclasses
import hashlib
import json
import logging
import struct
import zlib
from typing import Any, Callable, Dict, List, Optional, Tuple

from deploy.cloud.common.contract import PROTOCOL_VERSION, JobRequestMetadata, compute_request_digest
from deploy.cloud.common.flux import encode_png_rgb8
from deploy.cloud.common.manifest import PINNED_RECIPES, RECIPE_PROD_SDNQ
from deploy.cloud.common.weights import expected_marker, marker_path, snapshot_dir

_DEPLOY_LOGGER = logging.getLogger("deploy.cloud")


def silence_deploy_logs() -> Callable[[], None]:
    """Hide the expected error logs of failure-path tests; returns the undo."""
    previous = _DEPLOY_LOGGER.level
    _DEPLOY_LOGGER.setLevel(logging.CRITICAL)
    return lambda: _DEPLOY_LOGGER.setLevel(previous)


def _chunk(kind: bytes, data: bytes) -> bytes:
    crc = zlib.crc32(kind + data) & 0xFFFFFFFF
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", crc)


def encode_png_gray8(width: int, height: int, value: int = 255) -> bytes:
    """Single-frame Gray8 PNG, the shape the contract requires for hints."""
    raw = (b"\x00" + bytes([value & 0xFF]) * width) * height
    header = struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", header) + _chunk(b"IDAT", zlib.compress(raw)) + _chunk(b"IEND", b"")


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_job(
    job_id: str = "job-1",
    attempt_id: str = "attempt-1",
    width: int = 32,
    height: int = 32,
    seed: int = 1,
    steps: int = 4,
    guidance_scaled: int = 100,
    recipe_id: str = RECIPE_PROD_SDNQ,
    fill: Tuple[int, int, int] = (10, 20, 30),
) -> Tuple[JobRequestMetadata, bytes, bytes]:
    """A valid job (metadata, image, hint); defaults are the production recipe."""
    image = encode_png_rgb8(width, height, fill)
    hint = encode_png_gray8(width, height)
    draft = JobRequestMetadata(
        protocol_version=PROTOCOL_VERSION,
        job_id=job_id,
        attempt_id=attempt_id,
        recipe=PINNED_RECIPES[recipe_id],
        width=width,
        height=height,
        seed=seed,
        steps=steps,
        guidance_scaled=guidance_scaled,
        image_sha256=sha256(image),
        hint_sha256=sha256(hint),
        request_digest="0" * 64,
    )
    return dataclasses.replace(draft, request_digest=compute_request_digest(draft)), image, hint


def metadata_json(meta: JobRequestMetadata) -> str:
    return json.dumps(meta.to_dict(), sort_keys=True)


def write_marker(volume_root: Any) -> None:
    """Mark a volume as seeded without writing 5 GB of weights."""
    snapshot_dir(volume_root).mkdir(parents=True, exist_ok=True)
    marker_path(volume_root).write_text(json.dumps(expected_marker()), encoding="utf-8")


class FakeRunner:
    """Stands in for SdnqFluxRunner: returns a valid RGB PNG of the requested size."""

    def __init__(self, snapshot_path: str = "", fill: Tuple[int, int, int] = (250, 250, 250)):
        self.snapshot_path = snapshot_path
        self.fill = fill
        self.loaded = 0
        self.calls: List[Tuple[int, int, int, int, int]] = []

    def load(self) -> None:
        self.loaded += 1

    def render_png(self, image_png: bytes, hint_png: bytes, width: int, height: int, seed: int, steps: int,
                   guidance_scaled: int) -> bytes:
        self.calls.append((width, height, seed, steps, guidance_scaled))
        return encode_png_rgb8(width, height, self.fill)


class FakeModalDict:
    """modal.Dict surface used by ModalDictStore: get(key, default), put(key, value,
    skip_if_exists) returning whether it wrote, pop(key, default)."""

    def __init__(self) -> None:
        self.data: Dict[str, Any] = {}

    def get(self, key: str, default: Any = None) -> Any:
        return self.data.get(key, default)

    def put(self, key: str, value: Any, skip_if_exists: bool = False) -> bool:
        if skip_if_exists and key in self.data:
            return False
        self.data[key] = value
        return True

    def pop(self, key: str, default: Any = None) -> Any:
        return self.data.pop(key, default)


class ModalErrors:
    """The modal.exception names ModalWorkerDispatcher looks up, same hierarchy shape."""

    class Error(Exception):
        pass

    class OutputExpiredError(Error):
        pass

    class FunctionTimeoutError(Error):
        pass

    class RemoteError(Error):
        pass

    class ConnectionError(Error):
        pass

    class InternalError(Error):
        pass


class FakeFunctionCall:
    """modal.FunctionCall: get(timeout=0) raises the builtin TimeoutError until done."""

    def __init__(self, object_id: str):
        self.object_id = object_id
        self.result: Any = None
        self.error: Optional[BaseException] = TimeoutError()
        self.cancels: List[Dict[str, Any]] = []

    def get(self, timeout: Optional[float] = None) -> Any:
        if self.error is not None:
            raise self.error
        return self.result

    def cancel(self, **kwargs: Any) -> None:
        self.cancels.append(kwargs)


class FakeBeamMap:
    """beam.Map surface used by BeamMapStore: set(key, value, ttl), get, del."""

    def __init__(self) -> None:
        self.data: Dict[str, Any] = {}
        self.ttls: Dict[str, Optional[int]] = {}
        self.fail_set = False

    def set(self, key: str, value: Any, ttl: Optional[int] = None) -> bool:
        if self.fail_set:
            raise ConnectionError("map unavailable")
        self.data[key] = value
        self.ttls[key] = ttl
        return True

    def get(self, key: str) -> Any:
        return self.data.get(key)

    def __delitem__(self, key: str) -> None:
        del self.data[key]


def asgi_request(
    app: Callable,
    method: str,
    path: str,
    headers: Optional[Dict[str, str]] = None,
    body: bytes = b"",
) -> Tuple[int, Dict[str, str], bytes]:
    """Call an ASGI app once over an in-memory channel and collect the response."""
    raw_headers = [(k.lower().encode("latin1"), v.encode("latin1")) for k, v in (headers or {}).items()]
    if body:
        raw_headers.append((b"content-length", str(len(body)).encode("latin1")))
    scope = {
        "type": "http",
        "asgi": {"version": "3.0"},
        "http_version": "1.1",
        "method": method,
        "scheme": "https",
        "path": path,
        "raw_path": path.encode("latin1"),
        "root_path": "",
        "query_string": b"",
        "headers": raw_headers,
        "client": ("127.0.0.1", 50000),
        "server": ("127.0.0.1", 443),
    }
    sent: List[Dict[str, Any]] = []
    delivered = False

    async def receive() -> Dict[str, Any]:
        nonlocal delivered
        if not delivered:
            delivered = True
            return {"type": "http.request", "body": body, "more_body": False}
        return {"type": "http.disconnect"}

    async def send(message: Dict[str, Any]) -> None:
        sent.append(message)

    asyncio.run(app(scope, receive, send))
    start = next(m for m in sent if m["type"] == "http.response.start")
    payload = b"".join(m.get("body", b"") for m in sent if m["type"] == "http.response.body")
    response_headers = {k.decode("latin1").lower(): v.decode("latin1") for k, v in start.get("headers", [])}
    return start["status"], response_headers, payload


def multipart_body(metadata: str, image: bytes, hint: bytes, boundary: str = "mc-test-boundary") -> Tuple[str, bytes]:
    """The /jobs request body the desktop client sends: metadata, image, hint."""
    parts = [
        ("metadata", None, "application/json", metadata.encode("utf-8")),
        ("image", "image.png", "image/png", image),
        ("hint", "hint.png", "image/png", hint),
    ]
    body = b""
    for name, filename, content_type, data in parts:
        disposition = f'form-data; name="{name}"' + (f'; filename="{filename}"' if filename else "")
        body += (
            f"--{boundary}\r\nContent-Disposition: {disposition}\r\nContent-Type: {content_type}\r\n\r\n".encode("utf-8")
            + data
            + b"\r\n"
        )
    body += f"--{boundary}--\r\n".encode("utf-8")
    return f"multipart/form-data; boundary={boundary}", body
