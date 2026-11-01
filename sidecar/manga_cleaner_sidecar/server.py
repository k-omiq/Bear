"""HTTP/1.1 loopback server implementation for Manga Cleaner sidecar.

Handles wire protocol requests, authentication token verification, non-blocking
health reporting, and lifecycle coordination across model loading, rendering,
releasing, and shutdown.
"""

from __future__ import annotations

import hmac
import json
import os
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Dict, Optional, Tuple

from . import __version__
from .backend import BackendBase, get_backend, importable_backends
from .memory import build_memory_report
from .protocol import (
    PROTOCOL_VERSION,
    Basis,
    ErrorKind,
    ImageBuffer,
    SidecarError,
    State,
)


class SidecarServerContext:
    """Shared state and locking primitives across worker threads."""

    def __init__(
        self,
        token: Optional[str] = None,
        backend_name: Optional[str] = None,
        weights_dir: Optional[str] = None,
    ) -> None:
        self.token = token or os.environ.get("MC_SIDECAR_TOKEN", "")
        self.backend_name = backend_name or os.environ.get("MC_SIDECAR_BACKEND", "mflux")
        self.weights_dir = weights_dir or os.environ.get("MC_SIDECAR_WEIGHTS_DIR")

        self.backend: BackendBase = get_backend(
            name=self.backend_name,
            weights_dir=self.weights_dir,
        )

        self.state: State = State.IDLE
        self.state_lock = threading.Lock()
        self.render_lock = threading.Lock()
        self.server: Optional[ThreadingHTTPServer] = None


class SidecarRequestHandler(BaseHTTPRequestHandler):
    """HTTP request handler executing wire protocol contracts."""

    server_version = f"MangaCleanerSidecar/{__version__}"
    protocol_version = "HTTP/1.1"

    @property
    def ctx(self) -> SidecarServerContext:
        return self.server.ctx  # type: ignore

    def log_message(self, format: str, *args: Any) -> None:
        # Suppress noisy standard request logging to keep child pipe clear
        pass

    def _send_json_response(self, status_code: int, data: Dict[str, Any]) -> None:
        payload = json.dumps(data, separators=(",", ":")).encode("utf-8")
        self.send_response(status_code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            self.wfile.write(payload)
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def _send_error(self, error: SidecarError) -> None:
        # The reply holds one sentence. The traceback goes to stderr first, whose
        # last lines the parent adds to the failure (`sidecar/client.rs`). An
        # expected refusal, such as weights not there yet, has none to print.
        if error.status_code == 500 or error.__cause__ is not None:
            traceback.print_exception(error, file=sys.stderr)
        self._send_json_response(error.status_code, error.to_dict())

    def _send_unexpected(self, exc: Exception) -> None:
        error = SidecarError(500, ErrorKind.INTERNAL, str(exc))
        error.__cause__ = exc
        self._send_error(error)

    def _authenticate(self) -> None:
        """Validate the x-mc-token header against the configured shared secret."""
        if not self.ctx.token:
            # If no token configured in environment, allow local loopback access
            return

        received_token = self.headers.get("x-mc-token")
        if not received_token or not hmac.compare_digest(received_token, self.ctx.token):
            raise SidecarError(
                401,
                ErrorKind.UNAUTHORIZED,
                "Authentication failed: missing or invalid x-mc-token header.",
            )

    def _read_json_body(self) -> Dict[str, Any]:
        content_length = self.headers.get("Content-Length")
        if not content_length:
            return {}
        try:
            length = int(content_length)
            if length <= 0:
                return {}
            if length > 100 * 1024 * 1024:
                raise SidecarError(
                    422,
                    ErrorKind.BAD_REQUEST,
                    "Request body exceeds maximum allowed size.",
                )
            raw_body = self.rfile.read(length)
            if not raw_body:
                return {}
            return json.loads(raw_body.decode("utf-8"))
        except SidecarError:
            raise
        except (ValueError, UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise SidecarError(
                422,
                ErrorKind.BAD_REQUEST,
                f"Malformed JSON request body: {exc}",
            ) from exc

    def do_GET(self) -> None:
        """Handle GET requests (primarily /v1/health)."""
        try:
            if self.path == "/v1/health":
                self._handle_health()
            else:
                raise SidecarError(404, ErrorKind.BAD_REQUEST, f"Endpoint not found: {self.path}")
        except SidecarError as exc:
            self._send_error(exc)
        except Exception as exc:
            self._send_unexpected(exc)

    def do_POST(self) -> None:
        """Handle POST requests (/v1/open, /v1/render, /v1/release, /v1/shutdown)."""
        try:
            if self.path == "/v1/open":
                self._authenticate()
                self._handle_open()
            elif self.path == "/v1/render":
                self._authenticate()
                self._handle_render()
            elif self.path == "/v1/release":
                self._authenticate()
                self._handle_release()
            elif self.path == "/v1/shutdown":
                self._authenticate()
                self._handle_shutdown()
            else:
                raise SidecarError(404, ErrorKind.BAD_REQUEST, f"Endpoint not found: {self.path}")
        except SidecarError as exc:
            self._send_error(exc)
        except Exception as exc:
            self._send_unexpected(exc)

    def _handle_health(self) -> None:
        """GET /v1/health.

        Must answer immediately without taking the model lock so that liveness
        probes succeed during weight loading and long render operations.
        """
        with self.ctx.state_lock:
            current_state = self.ctx.state
            backend = self.ctx.backend

        model_meta = backend.get_model_metadata()
        weights_bytes = backend.get_weights_bytes()
        working_set_bytes = backend.get_working_set_bytes()
        basis = backend.get_basis()
        applied = backend.get_applied_guards()
        memory_report = build_memory_report()

        response_data: Dict[str, Any] = {
            "protocol": PROTOCOL_VERSION,
            "sidecar_version": __version__,
            "state": current_state.value,
            "backend": backend.backend_id,
            "model": model_meta.to_dict(),
            "weights_bytes": weights_bytes,
            "working_set_bytes": working_set_bytes,
            "basis": basis.value,
            "applied": applied,
            # Which backends this virtual environment could actually run. The
            # parent cannot see inside the venv - it found an interpreter beside
            # a `pyvenv.cfg` and nothing more - so a backend it is about to ask
            # for and that has no packages here is refused from the handshake
            # instead of failing halfway through a model load.
            "backends": importable_backends(),
            "memory": memory_report.to_dict(),
        }
        self._send_json_response(200, response_data)

    def _handle_open(self) -> None:
        """POST /v1/open.

        Loads model weights, applies memory limits, and returns applied guards.
        """
        body = self._read_json_body()
        backend_name = body.get("backend", self.ctx.backend_name)
        model_id = body.get("model", "flux2-klein-4b")
        limits = body.get("limits", {})
        require = body.get("require", [])

        # Switch backend if requested
        with self.ctx.state_lock:
            if backend_name != self.ctx.backend.backend_id:
                self.ctx.backend = get_backend(backend_name, weights_dir=self.ctx.weights_dir)
            self.ctx.state = State.LOADING

        try:
            result = self.ctx.backend.open(
                model_id=model_id,
                limits=limits,
                require=require,
            )
            with self.ctx.state_lock:
                self.ctx.state = State.READY

            result["memory"] = build_memory_report().to_dict()
            self._send_json_response(200, result)
        except Exception:
            with self.ctx.state_lock:
                self.ctx.state = State.IDLE
            raise

    def _handle_render(self) -> None:
        """POST /v1/render.

        Executes diffusion inpainting. Returns 409 immediately if a concurrent
        render is already in flight.
        """
        with self.ctx.state_lock:
            if self.ctx.state != State.READY:
                raise SidecarError(
                    412,
                    ErrorKind.NOT_OPEN,
                    f"Model is not ready for rendering (current state: {self.ctx.state.value}).",
                )

        # Enforce exactly one region in flight: return 409 immediately without queueing
        acquired = self.ctx.render_lock.acquire(blocking=False)
        if not acquired:
            raise SidecarError(
                409,
                ErrorKind.BUSY,
                "Another render operation is already in flight. Concurrent requests are not queued.",
            )

        try:
            with self.ctx.state_lock:
                self.ctx.state = State.BUSY

            body = self._read_json_body()
            region = body.get("region", "")
            raw_image = body.get("image")
            raw_hint = body.get("hint")
            prompt = body.get("prompt", "")
            steps = int(body.get("steps", 4))
            seed = int(body.get("seed", 1))
            guidance = float(body.get("guidance", 1.0))
            deadline_ms = int(body.get("deadline_ms", 300000))

            if not raw_image:
                raise SidecarError(422, ErrorKind.BAD_REQUEST, "Missing 'image' field in render request.")

            image_buffer = ImageBuffer.from_dict(raw_image)
            hint_buffer = ImageBuffer.from_dict(raw_hint) if raw_hint else None

            out_buffer, elapsed_ms = self.ctx.backend.render(
                image=image_buffer,
                hint=hint_buffer,
                prompt=prompt,
                steps=steps,
                seed=seed,
                guidance=guidance,
                deadline_ms=deadline_ms,
            )

            memory_report = build_memory_report()
            response_data = {
                "image": out_buffer.to_dict(),
                "elapsed_ms": elapsed_ms,
                "memory": memory_report.to_dict(),
            }
            self._send_json_response(200, response_data)
        finally:
            with self.ctx.state_lock:
                self.ctx.state = State.READY
            self.ctx.render_lock.release()

    def _handle_release(self) -> None:
        """POST /v1/release.

        Frees model memory and returns to IDLE state.
        """
        with self.ctx.state_lock:
            self.ctx.backend.release()
            self.ctx.state = State.IDLE

        report = build_memory_report()
        self._send_json_response(200, report.to_dict())

    def _handle_shutdown(self) -> None:
        """POST /v1/shutdown.

        Acknowledges with 202 Accepted and terminates the server process.
        """
        self._send_json_response(202, {"status": "shutting_down"})

        def _delayed_exit() -> None:
            time.sleep(0.05)
            if self.ctx.server:
                self.ctx.server.shutdown()
            os._exit(0)

        threading.Thread(target=_delayed_exit, name="sidecar-shutdown", daemon=True).start()


def create_server(
    host: str = "127.0.0.1",
    port: int = 0,
    token: Optional[str] = None,
    backend_name: Optional[str] = None,
    weights_dir: Optional[str] = None,
) -> Tuple[ThreadingHTTPServer, int]:
    """Create and bind a ThreadingHTTPServer on loopback."""
    server = ThreadingHTTPServer((host, port), SidecarRequestHandler)
    actual_port = server.server_address[1]

    ctx = SidecarServerContext(
        token=token,
        backend_name=backend_name,
        weights_dir=weights_dir,
    )
    ctx.server = server
    server.ctx = ctx  # type: ignore

    return server, actual_port
