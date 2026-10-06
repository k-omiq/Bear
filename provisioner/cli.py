"""CLI entrypoint and stdin/stdout protocol runner for cloud provisioner helper.

Protocol guarantees:
- Accepts exactly one bounded versioned JSON request via stdin (bounded to 64 KiB).
- Emits exactly one redacted JSON response on stdout.
- Emits only IC-2 progress records on stderr, one JSON object per line.
- Uses zero secret-bearing argv or environment logging.

Provider SDKs print, warn and log freely, and some C extensions write straight to the
process's file descriptors. Before anything imports them, `main` keeps private copies
of fd 1 and fd 2 for the response and the progress records and points the process-level
stdout and stderr at the null device, so nothing else can reach the desktop.
"""

import atexit
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
from typing import IO, Any, Dict, Optional, Tuple
import warnings

from provisioner.protocol import (
    ERR_EXECUTION_FAILED,
    ERR_INVALID_PAYLOAD,
    ERR_PAYLOAD_TOO_LARGE,
    MAX_REQUEST_BYTES,
    make_error_response,
)

# Credentials and profile selection the SDKs would otherwise pick up from the user's
# environment. The helper passes credentials explicitly, per request.
_SDK_VARIABLES = (
    "MODAL_TOKEN_ID",
    "MODAL_TOKEN_SECRET",
    "MODAL_PROFILE",
    "MODAL_ENVIRONMENT",
    "BEAM_TOKEN",
    "BETA9_TOKEN",
    "BETA9_GATEWAY_HOST",
    "BETA9_GATEWAY_PORT",
    "BETA9_API_URL",
    "GATEWAY_HOST",
    "GATEWAY_PORT",
    "API_HOST",
    "API_PORT",
    "CONTAINER_ID",
    "BETA9_IMPORTING_USER_CODE",
)


def _resolve_journal_root() -> Path:
    """Parse journal root from argv or env without secret logging."""
    argv = sys.argv[1:]
    for idx, arg in enumerate(argv):
        if arg in ("--journal-root", "-j") and idx + 1 < len(argv):
            return Path(argv[idx + 1]).resolve()
        if arg.startswith("--journal-root="):
            return Path(arg.split("=", 1)[1]).resolve()

    env_val = os.environ.get("MANGA_CLEANER_JOURNAL_ROOT")
    if env_val:
        return Path(env_val).resolve()

    # Default isolated path in user home directory
    return (Path.home() / ".manga-cleaner" / "cloud" / "journal").resolve()


def harden_environment() -> str:
    """Keep the SDKs away from the user's config files and token variables.

    Both SDKs read their config file path from the environment when they are first
    imported: modal from MODAL_CONFIG_PATH, beta9 from CONFIG_PATH (it also keeps its
    file-sync cache next to that file). Both point into a private directory that is
    removed at exit. Returns that directory.
    """
    private = tempfile.mkdtemp(prefix="mc-provisioner-")
    atexit.register(shutil.rmtree, private, True)
    for name in _SDK_VARIABLES:
        os.environ.pop(name, None)
    os.environ["MODAL_CONFIG_PATH"] = os.path.join(private, "modal.toml")
    os.environ["CONFIG_PATH"] = os.path.join(private, "beam", "config.ini")
    # Quiet gRPC's C core; its logs would go to fd 2.
    os.environ["GRPC_VERBOSITY"] = "NONE"
    os.environ.setdefault("GRPC_ENABLE_FORK_SUPPORT", "0")
    return private


def ensure_deploy_importable() -> None:
    """Make the deploy package importable from real files on disk.

    In the frozen helper the deploy tree ships as data next to the bundled modules
    (sys._MEIPASS), because Modal uploads those files and Beam copies them into its
    staging directory. In a checkout it sits next to this package.
    """
    root = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parents[1]))
    if (root / "deploy" / "__init__.py").is_file() and str(root) not in sys.path:
        sys.path.insert(0, str(root))


def isolate_standard_streams() -> Tuple[IO[str], IO[str]]:
    """Private text streams on the original fd 1 and fd 2; the fds themselves go to null."""
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.flush()
        except (AttributeError, OSError, ValueError):
            pass
    response_fd = os.dup(1)
    progress_fd = os.dup(2)
    null_fd = os.open(os.devnull, os.O_WRONLY)
    os.dup2(null_fd, 1)
    os.dup2(null_fd, 2)
    os.close(null_fd)
    response = os.fdopen(response_fd, "w", encoding="utf-8", newline="\n")
    progress = os.fdopen(progress_fd, "w", encoding="utf-8", newline="\n")
    sys.stdout = open(os.devnull, "w", encoding="utf-8")
    sys.stderr = open(os.devnull, "w", encoding="utf-8")
    return response, progress


def _write(stdout: IO[str], text: str) -> None:
    stdout.write(text + "\n")
    stdout.flush()


def run_helper(stdin_stream=None, stdout_stream=None, progress_stream: Optional[IO[str]] = None, controller=None) -> int:
    """Execute one bounded JSON helper request and emit one redacted response."""
    stdin = stdin_stream if stdin_stream is not None else sys.stdin.buffer
    stdout = stdout_stream if stdout_stream is not None else sys.stdout

    # Read bounded input (up to MAX_REQUEST_BYTES + 1)
    try:
        raw_bytes = stdin.read(MAX_REQUEST_BYTES + 1)
    except Exception as e:
        err = make_error_response(
            request_id="req-unknown",
            code=ERR_INVALID_PAYLOAD,
            message=f"Failed to read request from stdin: {e}",
        )
        _write(stdout, err.serialize())
        return 0

    if len(raw_bytes) > MAX_REQUEST_BYTES:
        err = make_error_response(
            request_id="req-unknown",
            code=ERR_PAYLOAD_TOO_LARGE,
            message=f"Request payload size ({len(raw_bytes)} bytes) exceeds maximum limit of {MAX_REQUEST_BYTES} bytes",
            actionable_guidance=f"Ensure JSON request payload is smaller than {MAX_REQUEST_BYTES} bytes (64 KiB).",
        )
        _write(stdout, err.serialize())
        return 0

    if not raw_bytes or not raw_bytes.strip():
        err = make_error_response(
            request_id="req-unknown",
            code=ERR_INVALID_PAYLOAD,
            message="Empty request payload received on stdin",
            actionable_guidance="Provide a valid versioned JSON request envelope via stdin.",
        )
        _write(stdout, err.serialize())
        return 0

    try:
        if controller is None:
            from provisioner.controller import ProvisioningController

            controller = ProvisioningController(journal_root=_resolve_journal_root(), progress_stream=progress_stream)
        _write(stdout, controller.handle_request(raw_bytes))
        return 0
    except (Exception, SystemExit) as e:
        err = make_error_response(
            request_id="req-unknown",
            code=ERR_EXECUTION_FAILED,
            message=f"Fatal provisioner error: {type(e).__name__}: {e}",
        )
        _write(stdout, err.serialize())
        return 0


def self_check() -> Dict[str, Any]:
    """Import everything a real run imports, without any network call.

    Used to smoke test the frozen helper: the deploy tree must import from files on
    disk, both SDKs with their data files must load, and both provider apps must build
    their objects. Beam's decorators need a channel, so a loopback one is set first.
    """
    report: Dict[str, Any] = {"python": sys.version.split()[0], "frozen": bool(getattr(sys, "frozen", False))}
    import deploy.cloud.common.api  # noqa: F401
    import deploy.cloud.common.jobs  # noqa: F401
    from deploy.cloud.beam.settings import BeamSettings
    from deploy.cloud.beam.stage import stage_app
    from deploy.cloud.modal.settings import ModalSettings

    from provisioner.beam_driver import import_staged_app, load_beta9
    from provisioner.modal_driver import import_modal_app, load_modal, working_directory

    modal = load_modal()
    report["modal"] = getattr(modal, "__version__", "")
    app = import_modal_app(ModalSettings.for_installation("mc-selfcheck", "mc-selfcheck"))
    report["modal_app"] = sorted(app.app.registered_functions) + sorted(app.app.registered_classes)
    report["deploy_root"] = str(Path(app.DEPLOY_ROOT))

    sdk = load_beta9()
    channel = sdk.Channel(addr="127.0.0.1:9", token="self-check", retry=(lambda state: None, False))
    channel.config = sdk.ConfigContext(token="self-check", gateway_host="127.0.0.1", gateway_port=9)
    sdk.base.set_channel(channel=channel)
    stage_dir = Path(tempfile.mkdtemp(prefix="mc-selfcheck-")).resolve()
    try:
        stage_app(stage_dir)
        settings = BeamSettings.for_installation("mc-selfcheck", "mc-selfcheck", gateway_host="127.0.0.1", gateway_port=9)
        with working_directory(str(stage_dir)):
            beam_app = import_staged_app(stage_dir, settings.with_worker_url("https://example.invalid").to_env())
        report["beam_app"] = sorted(name for name in ("seed", "render", "analyze", "gateway") if hasattr(getattr(beam_app, name), "deploy"))
        report["beam_staged_files"] = sorted(str(p.relative_to(stage_dir)) for p in stage_dir.rglob("*.py"))
    finally:
        sdk.base.unset_channel()
        channel.close()
        shutil.rmtree(stage_dir, ignore_errors=True)
    report["ok"] = True
    return report


def main() -> int:
    """Main CLI entrypoint."""
    response, progress = isolate_standard_streams()
    harden_environment()
    ensure_deploy_importable()
    sys.dont_write_bytecode = True
    warnings.simplefilter("ignore")
    if "--self-check" in sys.argv[1:]:
        try:
            report = self_check()
        except (Exception, SystemExit) as exc:
            report = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        _write(response, json.dumps(report, sort_keys=True))
        return 0 if report.get("ok") else 1
    return run_helper(sys.stdin.buffer, response, progress)


if __name__ == "__main__":
    sys.exit(main())
