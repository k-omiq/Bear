"""Beam driver: one installation in the user's Beam workspace.

Every call below was checked against the beta9 0.1.268 / beam-client 0.2.211 source.
The session sets one explicit gRPC channel (with its ConfigContext) as the SDK's
global channel, so ~/.beam/config.ini, profiles, BEAM_TOKEN and prompts are never
used; the CLI also points CONFIG_PATH at a private empty path, which is where the SDK
keeps its file-sync cache. All SDK terminal output goes to an in-memory buffer.

Beam has no documented way to create a scoped token for `authorized=True` endpoints
(the SDK's CreateToken call exists, but what a restricted token may call is decided by
the server and not visible in the source), so the runtime credential is the user's
own Beam API key, and the gateway keeps a copy in a Beam secret to queue GPU jobs.

Resources of installation <prefix>:
- volume <prefix>-weights: the pinned snapshot of the chosen render model
- map <prefix>-jobs: job documents and the seeding status
- secret MC_<PREFIX>_TOKEN: the API key the gateway uses to queue GPU jobs
- deployments <prefix>-seed, <prefix>-worker (task queues), <prefix>-gateway (ASGI)
"""

from __future__ import annotations

import importlib
import io
import json
import os
import pickle
import shutil
import sys
import tempfile
import time
import urllib.error
import urllib.request
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable, Dict, Iterator, List, Optional, Tuple

from provisioner.driver_base import (
    PRICING_DATE,
    SEED_INTENT_KEY,
    AccountInspectionResult,
    BaseProviderDriver,
    Deadline,
    DeploymentPlan,
    PlanResource,
    SeedWatch,
    StepContext,
    build_plan,
    installation_prefix,
    normalize_options,
    read_seed_state,
    seed_failed,
)
from provisioner.endpoint import check_endpoint, endpoint_from_base, runtime_credential
from provisioner.journal import STAGE_DEPLOYING
from provisioner.modal_driver import working_directory
from provisioner.protocol import (
    ERR_ACTIONABLE_PERMISSION,
    ERR_EXECUTION_FAILED,
    ERR_EXECUTION_TIMEOUT,
    ERR_PROVIDER_UNAVAILABLE,
    ERR_VALIDATION,
    IDENTIFIER_REGEX,
    ProtocolError,
)
from provisioner.redaction import redact_string

from deploy.cloud.beam.backend import MAP_TTL_SECONDS
from deploy.cloud.beam.settings import (
    APP_MODULE,
    DEFAULT_GATEWAY_HOST,
    DEFAULT_GATEWAY_PORT,
    DEFAULT_GPU,
    ENV_GATEWAY_HOST,
    ENV_GATEWAY_PORT,
    GPU_ALLOWLIST,
    BeamSettings,
)
from deploy.cloud.beam.stage import stage_app
from deploy.cloud.common.deployment import DEFAULT_ROUTING_REGION
from deploy.cloud.common.weights import SEED_STATE_KEY
from deploy.cloud.common.capacity import render_copies, render_worker_memory_gib
from deploy.cloud.common.manifest import production_model

PRICING_URL = "https://beam.cloud/pricing"
# USD per hour, list price on PRICING_DATE. Beam lists no price for A10G and RTX5090.
GPU_PRICE_PER_HOUR = {"RTX4090": 0.69}
GPU_CPU_CORE_HOUR = 0.37944
GPU_MEMORY_GIB_HOUR = 0.0198
CPU_CORE_HOUR = 0.045
MEMORY_GIB_HOUR = 0.00756

INSTALLATION_KEY = "mc:installation"
MAX_MAP_KEYS = 20000
MAP_CLEAR_ROUNDS = 3
RPC_SECONDS = 30.0
HTTP_TIMEOUT_SECONDS = 30.0
WEIGHTS_RESERVE_SECONDS = 180.0
POLL_SECONDS = 5.0
FAILED_TASK_STATES = {"ERROR", "CANCELLED", "TIMEOUT"}
LIVE_TASK_STATES = {"PENDING", "RUNNING", "RETRY"}


@dataclass(frozen=True)
class BeamCredentials:
    token: str

    def __repr__(self) -> str:  # never print the token
        return "BeamCredentials(<redacted>)"


@dataclass
class BeamSession:
    sdk: Any
    channel: Any
    token: str
    gateway_host: str
    gateway_port: int
    workspace_id: str = ""
    terminal: io.StringIO = field(default_factory=io.StringIO)


def load_beta9() -> Any:
    """The beta9 modules this driver uses (beam re-exports beta9)."""
    importlib.import_module("beam")  # selects Beam's defaults inside beta9
    channel = importlib.import_module("beta9.channel")
    return SimpleNamespace(
        Channel=channel.Channel,
        rpc_timeout=channel.rpc_timeout,
        ConfigContext=importlib.import_module("beta9.config").ConfigContext,
        base=importlib.import_module("beta9.abstractions.base"),
        terminal=importlib.import_module("beta9.terminal"),
        gateway=importlib.import_module("beta9.clients.gateway"),
        secret=importlib.import_module("beta9.clients.secret"),
        volume=importlib.import_module("beta9.clients.volume"),
        map=importlib.import_module("beta9.clients.map"),
        grpc=importlib.import_module("grpc"),
    )


def import_staged_app(stage_dir: Path, env: Dict[str, str]) -> Any:
    """Import the staged mc_beam_app from `stage_dir`, which is the working directory.

    Beam names each handler after the module path relative to the working directory
    and uploads that directory, so the caller keeps it the working directory until the
    deploy finished. The module reads MC_BEAM_* at import time.
    """
    saved = {key: os.environ.get(key) for key in env}
    os.environ.update(env)
    sys.path.insert(0, str(stage_dir))
    sys.modules.pop(APP_MODULE, None)
    try:
        return importlib.import_module(APP_MODULE)
    finally:
        sys.modules.pop(APP_MODULE, None)
        if str(stage_dir) in sys.path:
            sys.path.remove(str(stage_dir))
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


class _PlainData(pickle.Unpickler):
    """Unpickles dicts, lists and scalars only; any class or function is refused."""

    def find_class(self, module: str, name: str) -> Any:
        raise pickle.UnpicklingError("the seeding status holds more than plain data")


def load_plain(data: bytes) -> Any:
    return _PlainData(io.BytesIO(data)).load()


def post_json(url: str, token: str, body: Dict[str, Any], timeout: float) -> Tuple[Optional[int], bytes]:
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
            "User-Agent": "manga-cleaner-provisioner",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, response.read(64 * 1024)
    except urllib.error.HTTPError as exc:
        try:
            return exc.code, exc.read(64 * 1024)
        finally:
            exc.close()
    except (urllib.error.URLError, OSError, ValueError):
        return None, b""


def gateway_address() -> Tuple[str, int]:
    """Beam's gRPC gateway; MC_BEAM_GATEWAY_HOST/PORT override it for offline smoke tests."""
    host = os.environ.get(ENV_GATEWAY_HOST) or DEFAULT_GATEWAY_HOST
    port = os.environ.get(ENV_GATEWAY_PORT) or str(DEFAULT_GATEWAY_PORT)
    if not all(c.isalnum() or c in ".-" for c in host) or not port.isdigit() or not 0 < int(port) < 65536:
        raise ProtocolError(ERR_VALIDATION, "Invalid Beam gateway override")
    return host, int(port)


class BeamDriver(BaseProviderDriver):
    provider_id = "beam"
    pipeline_steps = ("volume", "state", "secret", "image", "deploy", "weights", "endpoint", "health")
    # The secret holds the user's key; every run stores the key it was given.
    repeat_steps = ("secret", "health")
    seed_state_keys = ("seed_task_id", SEED_INTENT_KEY)
    cleanup_order = ("gateway", "worker", "secret", "dict", "volume")

    def __init__(
        self,
        sdk_loader: Callable[[], Any] = load_beta9,
        app_loader: Callable[[Path, Dict[str, str]], Any] = import_staged_app,
        http: Optional[Callable[..., Any]] = None,
        post: Callable[..., Tuple[Optional[int], bytes]] = post_json,
        sleep: Callable[[float], None] = time.sleep,
        clock: Callable[[], float] = time.monotonic,
        wall_clock: Callable[[], float] = time.time,
    ) -> None:
        self._sdk_loader = sdk_loader
        self._app_loader = app_loader
        self._http = http
        self._post = post
        self._sleep = sleep
        self._clock = clock
        self._wall_clock = wall_clock

    # ---------- credentials and session ----------

    def parse_credentials(self, raw: Any) -> BeamCredentials:
        token = raw.get("token") if isinstance(raw, dict) else None
        if not isinstance(token, str) or not token.strip():
            raise ProtocolError(ERR_VALIDATION, "Beam credentials are missing: paste a Beam API key")
        token = token.strip()
        if len(token) > 4096 or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in token):
            raise ProtocolError(ERR_VALIDATION, "The Beam API key is not valid")
        return BeamCredentials(token=token)

    def _load_sdk(self) -> Any:
        try:
            return self._sdk_loader()
        except ImportError as exc:
            raise ProtocolError(
                ERR_PROVIDER_UNAVAILABLE,
                f"The Beam SDK is not available in this helper: {exc}",
                actionable_guidance="Reinstall the app; the cloud helper ships with the Beam SDK.",
            ) from None

    def _failure(
        self, session: BeamSession, exc: BaseException, doing: str, before_changes: bool = False
    ) -> ProtocolError:
        grpc = session.sdk.grpc
        if isinstance(exc, SystemExit):
            # beta9 reports some failures on its console and exits; the console is ours.
            said = redact_string(self._last_console_line(session))
            return ProtocolError(
                ERR_EXECUTION_FAILED,
                f"Beam could not {doing}: {said[:400] or 'the SDK stopped'}",
                actionable_guidance="Run Resume to try again; finished steps are not repeated.",
            )
        if isinstance(exc, grpc.RpcError):
            code = exc.code() if callable(getattr(exc, "code", None)) else None
            code_name = getattr(code, "name", str(code))
            if code in (grpc.StatusCode.UNAUTHENTICATED, grpc.StatusCode.PERMISSION_DENIED):
                return ProtocolError(
                    ERR_ACTIONABLE_PERMISSION,
                    f"Beam refused to {doing} ({code_name})",
                    actionable_guidance="Check that the Beam API key is active and belongs to the workspace you want to use.",
                )
            if before_changes and code in (grpc.StatusCode.UNAVAILABLE, grpc.StatusCode.DEADLINE_EXCEEDED):
                return ProtocolError(
                    ERR_PROVIDER_UNAVAILABLE,
                    f"Could not reach Beam to {doing} ({code_name})",
                    actionable_guidance="Check the internet connection and try again.",
                )
            return ProtocolError(
                ERR_EXECUTION_FAILED,
                f"Beam could not {doing} ({code_name})",
                actionable_guidance="Run Resume to try again; finished steps are not repeated.",
            )
        return ProtocolError(
            ERR_EXECUTION_FAILED,
            f"Beam could not {doing}: {type(exc).__name__}: {redact_string(str(exc))[:400]}",
            actionable_guidance="Run Resume to try again; finished steps are not repeated.",
        )

    @staticmethod
    def _last_console_line(session: BeamSession) -> str:
        lines = [line.strip() for line in session.terminal.getvalue().splitlines() if line.strip()]
        return lines[-1] if lines else ""

    def _refused(self, doing: str, message: str) -> ProtocolError:
        return ProtocolError(
            ERR_EXECUTION_FAILED,
            f"Beam could not {doing}: {redact_string(message or 'no reason given')[:400]}",
            actionable_guidance="Run Resume to try again; finished steps are not repeated.",
        )

    @contextmanager
    def session(self, credentials: BeamCredentials, deadline: Deadline) -> Iterator[BeamSession]:
        sdk = self._load_sdk()
        host, port = gateway_address()
        channel = sdk.Channel(
            addr=f"{host}:{port}",
            token=credentials.token,
            # No reconnect watcher and no eager connect: every call has its own deadline.
            retry=(lambda state: None, False),
        )
        # The SDK's runners read their config from the channel; without it they look
        # for a config file and would prompt.
        channel.config = sdk.ConfigContext(token=credentials.token, gateway_host=host, gateway_port=port)
        session = BeamSession(sdk=sdk, channel=channel, token=credentials.token, gateway_host=host, gateway_port=port)
        sdk.base.set_channel(channel=channel)
        try:
            with sdk.terminal.redirect_terminal_to_buffer(session.terminal):
                try:
                    with sdk.rpc_timeout(min(RPC_SECONDS, max(1.0, deadline.remaining()))):
                        response = sdk.gateway.GatewayServiceStub(channel).authorize(sdk.gateway.AuthorizeRequest())
                except (Exception, SystemExit) as exc:
                    raise self._failure(session, exc, "sign in", before_changes=True) from None
                if not response.ok:
                    raise ProtocolError(
                        ERR_ACTIONABLE_PERMISSION,
                        f"Beam did not accept the API key: {redact_string(response.error_msg or '')[:200]}",
                        actionable_guidance="Copy a current API key from Beam Settings, API Keys.",
                    )
                session.workspace_id = str(response.workspace_id or "")
                yield session
        finally:
            sdk.base.unset_channel()
            try:
                channel.close()
            except Exception:
                pass

    def _rpc(self, session: BeamSession, deadline: Deadline, doing: str, call: Callable[[], Any]) -> Any:
        try:
            with session.sdk.rpc_timeout(min(RPC_SECONDS, max(1.0, deadline.remaining()))):
                return call()
        except ProtocolError:
            raise
        except (Exception, SystemExit) as exc:
            raise self._failure(session, exc, doing) from None

    # ---------- inspect and plan ----------

    def inspect(self, session: BeamSession) -> AccountInspectionResult:
        return AccountInspectionResult(
            provider="beam",
            account_id=session.workspace_id,
            workspace_name=session.workspace_id,
            authenticated=True,
            permissions_granted=[],
            permissions_missing=[],
            eligible=True,
            platform_notes="Beam workspace. Permissions are checked when each resource is created.",
        )

    def normalize_options(self, options: Any) -> Dict[str, Any]:
        normalized = normalize_options(options, GPU_ALLOWLIST, DEFAULT_GPU, "beam")
        if normalized.pop("denoise"):
            raise ProtocolError(ERR_VALIDATION, "Page denoise is only available on Modal installations")
        if normalized.pop("routing_region") != DEFAULT_ROUTING_REGION:
            raise ProtocolError(ERR_VALIDATION, "A routing region is only available on Modal installations")
        return normalized

    def settings_for(self, installation_id: str, options: Dict[str, Any]) -> BeamSettings:
        host, port = gateway_address()
        return BeamSettings.for_installation(
            installation_id,
            installation_prefix(installation_id),
            gpu=options["gpu"],
            idle_seconds=options["idle_seconds"],
            model_id=options["model_id"],
            analysis_models=tuple(options["analysis_models"]),
            gateway_host=host,
            gateway_port=port,
        )

    def plan(self, inspection: AccountInspectionResult, installation_id: str, options: Dict[str, Any]) -> DeploymentPlan:
        s = self.settings_for(installation_id, options)
        gpu, idle = s.gpu, s.idle_seconds
        model = production_model(s.model_id)
        # The deployed queue's own sizing (common/capacity.py), so the quote is what bills.
        worker_memory = render_worker_memory_gib(model, gpu, "beam")
        copies = render_copies(model, gpu)
        copies_note = f", {copies} worker processes rendering at once" if copies > 1 else ""
        resources = [
            PlanResource("volume", s.volume_name, f"Beam volume holding the pinned {model.label} weights ({model.total_bytes / 1e9:.1f} GB)"
                         + (" and selected analysis graphs." if s.analysis_models else ".")),
            PlanResource("dict", s.map_name, "Beam map with job state, so status checks never start a GPU."),
            PlanResource(
                "secret",
                s.secret_name,
                "Beam secret holding your Beam API key, which the gateway uses to queue GPU jobs.",
            ),
            PlanResource("worker", s.seed_name,
                         f"CPU task queue (2 CPU, {24 if 'text_mask_sam_ts@1' in s.analysis_models else 4} GiB) that installs pinned model weights and selected analysis graphs once."),
            PlanResource(
                "worker",
                s.worker_name,
                f"{gpu} GPU task queue (1 CPU, {worker_memory} GiB, at most 1 container{copies_note}, stays warm {idle} s after the last render, 600 s per job).",
            ),
            PlanResource("gateway", s.gateway_name, "CPU web endpoint (0.5 CPU, 1 GiB) that needs a Beam API key."),
        ]
        if s.analysis_models:
            resources.insert(-1, PlanResource(
                "worker", s.analysis_name,
                f"{gpu} GPU analysis task queue (at most 1 container, scales to zero after {idle} s).",
            ))
        price = GPU_PRICE_PER_HOUR.get(gpu)
        if price is not None:
            gpu_text = (
                f"A {gpu} worker costs about ${price + GPU_CPU_CORE_HOUR + worker_memory * GPU_MEMORY_GIB_HOUR:.2f} per hour while it runs "
                f"({gpu} ${price:.2f}/h, 1 CPU core ${GPU_CPU_CORE_HOUR}/h, {worker_memory} GiB at ${GPU_MEMORY_GIB_HOUR}/GiB/h)"
            )
        else:
            gpu_text = f"The {gpu} worker is billed by Beam at its current rate while it runs"
        cost = (
            f"Billed by Beam per second of use. {gpu_text}, during each render and for {idle} s after it. "
            + (f"Selected analysis also starts a separate {gpu} GPU task queue on demand and keeps it warm {idle} s; it is billed at the same GPU rate. " if s.analysis_models else "")
            +
            f"The gateway runs while it answers requests and stays warm 120 s after the last one "
            f"(0.5 CPU at ${CPU_CORE_HOUR}/h per core, 1 GiB at ${MEMORY_GIB_HOUR}/GiB/h). "
            f"Storage for the weights volume is billed by Beam. List prices on {PRICING_DATE}; check {PRICING_URL}."
        )
        return build_plan(
            provider="beam",
            installation_id=installation_id,
            inspection=inspection,
            app_name=s.prefix,
            options=options,
            gpu_allowlist=GPU_ALLOWLIST,
            allocation={
                "worker_cpu": 1.0,
                "worker_memory_gib": worker_memory,
                "gateway_cpu": 0.5,
                "gateway_memory_gib": 1,
                "max_containers": 1,
                "job_timeout_seconds": 600,
            },
            resources=resources,
            required_permissions=["Deploy task queues and endpoints", "Create volumes, maps and secrets"],
            cost=cost,
            notes=[
                "Beam has no separate access token for one endpoint: the app calls it with your own Beam API key, "
                "kept in this computer's keychain, and the gateway keeps a copy in the Beam secret above. "
                "Revoking the key stops the endpoint until you run setup again.",
                "The weights download once on a CPU container; setup waits for it.",
            ],
            cleanup_summary=[
                f"Stop and delete the deployments {', '.join(s.deployment_names.values())}",
                f"Delete the secret {s.secret_name}",
                f"Delete the job state in the map {s.map_name}",
                f"Delete the volume {s.volume_name} and the weights in it",
            ],
        )

    # ---------- pipeline ----------

    def _settings(self, ctx: StepContext) -> BeamSettings:
        return self.settings_for(ctx.journal.record.installation_id, ctx.options)

    def run_step(self, step: str, session: BeamSession, ctx: StepContext) -> None:
        settings = self._settings(ctx)
        handler = getattr(self, f"_step_{step}")
        try:
            handler(session, ctx, settings)
        except ProtocolError:
            raise
        except (Exception, SystemExit) as exc:
            raise self._failure(session, exc, _STEP_DOING[step]) from None

    def _step_volume(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        sdk = session.sdk
        ctx.journal.record_resource(s.volume_name, "volume", s.volume_name, STAGE_DEPLOYING)
        response = self._rpc(
            session,
            ctx.deadline,
            "create the weights volume",
            lambda: sdk.volume.VolumeServiceStub(session.channel).get_or_create_volume(
                sdk.volume.GetOrCreateVolumeRequest(name=s.volume_name)
            ),
        )
        if not response.ok:
            raise self._refused("create the weights volume", response.err_msg)

    def _map_set(self, session: BeamSession, deadline: Deadline, name: str, key: str, value: Any) -> None:
        sdk = session.sdk
        response = self._rpc(
            session,
            deadline,
            "write the job state",
            lambda: sdk.map.MapServiceStub(session.channel).map_set(
                sdk.map.MapSetRequest(name=name, key=key, value=pickle.dumps(value, protocol=4), ttl=MAP_TTL_SECONDS)
            ),
        )
        if not response.ok:
            raise self._refused("write the job state", response.err_msg)

    def _map_get(self, session: BeamSession, deadline: Deadline, name: str, key: str) -> Any:
        sdk = session.sdk
        response = self._rpc(
            session,
            deadline,
            "read the job state",
            lambda: sdk.map.MapServiceStub(session.channel).map_get(sdk.map.MapGetRequest(name=name, key=key)),
        )
        if not response.ok or not response.value:
            return None
        try:
            return load_plain(bytes(response.value))
        except Exception:
            return None

    def _map_delete(self, session: BeamSession, deadline: Deadline, name: str, key: str) -> None:
        # The answer is only ok, and the SDK reads a refusal as a missing key; callers
        # that must know list the map again.
        sdk = session.sdk
        self._rpc(
            session,
            deadline,
            "delete the job state",
            lambda: sdk.map.MapServiceStub(session.channel).map_delete(sdk.map.MapDeleteRequest(name=name, key=key)),
        )

    def _map_keys(self, session: BeamSession, deadline: Deadline, name: str) -> List[str]:
        sdk = session.sdk
        doing = f"list the job state in {name}"
        response = self._rpc(
            session,
            deadline,
            doing,
            lambda: sdk.map.MapServiceStub(session.channel).map_keys(sdk.map.MapKeysRequest(name=name)),
        )
        if not response.ok:
            # The answer carries no reason, so a refusal is never taken for a missing map.
            raise self._refused(doing, "")
        return list(response.keys)

    def _clear_map(self, session: BeamSession, deadline: Deadline, name: str) -> str:
        """Delete every key of a map, then check that none is left.

        beta9 0.1.268 has no call that deletes a map itself (MapService only sets, gets,
        deletes, counts and lists keys), and a map with no keys holds nothing, so an
        empty map counts as missing.
        """
        keys = self._map_keys(session, deadline, name)
        if not keys:
            return "missing"
        for _ in range(MAP_CLEAR_ROUNDS):
            for key in keys[:MAX_MAP_KEYS]:
                deadline.require(RPC_SECONDS, f"delete the job state in {name}")
                self._map_delete(session, deadline, name, key)
            keys = self._map_keys(session, deadline, name)
            if not keys:
                return "deleted"
        raise self._refused(f"delete the job state in {name}", f"{len(keys)} keys remain")

    def _step_state(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        ctx.journal.record_resource(s.map_name, "dict", s.map_name, STAGE_DEPLOYING)
        # A Beam map exists once it holds a key; this one also names its owner.
        self._map_set(
            session,
            ctx.deadline,
            s.map_name,
            INSTALLATION_KEY,
            {"installation_id": s.installation_id, "managed_by": "manga-cleaner"},
        )

    def _step_secret(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        sdk = session.sdk
        ctx.journal.record_resource(s.secret_name, "secret", s.secret_name, STAGE_DEPLOYING)
        stub = sdk.secret.SecretServiceStub(session.channel)
        created = self._rpc(
            session,
            ctx.deadline,
            "store the gateway secret",
            lambda: stub.create_secret(sdk.secret.CreateSecretRequest(name=s.secret_name, value=session.token)),
        )
        if created.ok:
            return
        updated = self._rpc(
            session,
            ctx.deadline,
            "store the gateway secret",
            lambda: stub.update_secret(sdk.secret.UpdateSecretRequest(name=s.secret_name, value=session.token)),
        )
        if not updated.ok:
            raise self._refused("store the gateway secret", updated.err_msg or created.err_msg)

    def _deploy(
        self, session: BeamSession, deadline: Deadline, settings: BeamSettings, handler_name: str, deployment_name: str
    ) -> Dict[str, Any]:
        """Stage the app, import it with `settings` and deploy one handler."""
        doing = f"deploy {deployment_name}"
        stage_dir = Path(tempfile.mkdtemp(prefix="mc-beam-")).resolve()
        try:
            stage_app(stage_dir)
            with working_directory(str(stage_dir)):
                module = self._app_loader(stage_dir, settings.to_env())
                handler = getattr(module, handler_name)
                with session.sdk.rpc_timeout(max(1.0, deadline.remaining())):
                    result, ok = handler.deploy(name=deployment_name, invocation_details_func=lambda **_: None)
        except ProtocolError:
            raise
        except (Exception, SystemExit) as exc:
            raise self._failure(session, exc, doing) from None
        finally:
            shutil.rmtree(stage_dir, ignore_errors=True)
        if not ok or not isinstance(result, dict) or result.get("status") != "accepted":
            raise self._refused(doing, self._last_console_line(session))
        deployment_id = str(result.get("deployment_id") or "")
        invoke_url = str(result.get("invoke_url") or "")
        if not IDENTIFIER_REGEX.match(deployment_id) or not invoke_url.startswith("https://"):
            raise ProtocolError(ERR_EXECUTION_FAILED, f"Beam returned no deployment id or https URL for {deployment_name}")
        return {"deployment_id": deployment_id, "invoke_url": invoke_url}

    def _step_image(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        journal = ctx.journal
        # Building the task queue images (the GPU one carries torch) is the long part.
        journal.record_resource(s.seed_name, "worker", s.seed_name, STAGE_DEPLOYING)
        journal.record_resource(s.worker_name, "worker", s.worker_name, STAGE_DEPLOYING)
        if s.analysis_models:
            journal.record_resource(s.analysis_name, "worker", s.analysis_name, STAGE_DEPLOYING)
        seed = self._deploy(session, ctx.deadline, s, "seed", s.seed_name)
        journal.set_state(seed_url=seed["invoke_url"], seed_deployment_id=seed["deployment_id"])
        if ctx.reporter is not None:
            ctx.reporter.pct(50)
        worker = self._deploy(session, ctx.deadline, s, "render", s.worker_name)
        journal.set_state(worker_url=worker["invoke_url"], worker_deployment_id=worker["deployment_id"])
        if s.analysis_models:
            analysis = self._deploy(session, ctx.deadline, s, "analyze", s.analysis_name)
            journal.set_state(analysis_url=analysis["invoke_url"], analysis_deployment_id=analysis["deployment_id"])

    def _step_deploy(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        journal = ctx.journal
        worker_url = journal.get_state("worker_url")
        if not worker_url:
            raise ProtocolError(ERR_EXECUTION_FAILED, "The GPU worker URL is missing from the journal")
        journal.record_resource(s.gateway_name, "gateway", s.gateway_name, STAGE_DEPLOYING)
        analysis_url = journal.get_state("analysis_url") if s.analysis_models else ""
        if s.analysis_models and not analysis_url:
            raise ProtocolError(ERR_EXECUTION_FAILED, "The analysis GPU worker URL is missing from the journal")
        gateway = self._deploy(session, ctx.deadline,
                               s.with_worker_url(worker_url).with_analysis_url(analysis_url),
                               "gateway", s.gateway_name)
        journal.set_state(gateway_url=gateway["invoke_url"], gateway_deployment_id=gateway["deployment_id"])

    def _task_status(self, session: BeamSession, deadline: Deadline, task_id: str) -> Optional[str]:
        """The task's status, or None when Beam no longer lists the task."""
        sdk = session.sdk
        doing = "read the weights download task"
        response = self._rpc(
            session,
            deadline,
            doing,
            lambda: sdk.gateway.GatewayServiceStub(session.channel).list_tasks(
                sdk.gateway.ListTasksRequest(filters={"id": sdk.gateway.StringList(values=[task_id])}, limit=1)
            ),
        )
        if not response.ok:
            if "not found" in (response.err_msg or "").lower():
                return None
            # A failed listing says nothing about the task: never take it for a gone one.
            raise self._refused(doing, response.err_msg)
        for task in response.tasks:
            if task.id == task_id:
                return str(task.status or "").upper()
        return None

    def _start_seed(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        seed_url = ctx.journal.get_state("seed_url")
        if not seed_url:
            raise ProtocolError(ERR_EXECUTION_FAILED, "The weights task URL is missing from the journal")
        # A new seeding task overwrites the status document; drop a stale one first.
        self._map_delete(session, ctx.deadline, s.map_name, SEED_STATE_KEY)
        # The intent is journaled before the task is queued: a helper stopped before it
        # learns the task id then waits on that task instead of queueing another.
        ctx.journal.set_state(seed_task_id=None, **{SEED_INTENT_KEY: int(self._wall_clock())})
        status, body = self._post(seed_url, session.token, {}, HTTP_TIMEOUT_SECONDS)
        if status in (401, 403):
            raise ProtocolError(ERR_ACTIONABLE_PERMISSION, f"Beam refused to start the weights download (HTTP {status})")
        try:
            task_id = json.loads(body.decode("utf-8")).get("task_id") if status == 200 else None
        except (ValueError, AttributeError):
            task_id = None
        if not isinstance(task_id, str) or not IDENTIFIER_REGEX.match(task_id):
            raise ProtocolError(
                ERR_EXECUTION_FAILED,
                f"Beam did not start the weights download (HTTP {status})" if status else "Could not reach the weights task",
                actionable_guidance="Run Resume to try again.",
            )
        ctx.journal.set_state(seed_task_id=task_id)

    def _seed_doc(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> Any:
        return self._map_get(session, ctx.deadline, s.map_name, SEED_STATE_KEY)

    def _step_weights(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        journal = ctx.journal
        state, _, _ = read_seed_state(self._seed_doc(session, ctx, s), s.model_id)
        if state == "done":
            return
        task_id = journal.get_state("seed_task_id")
        # A seed the journal knows of, by its task id or only by the intent to queue it,
        # may still run: it is waited on below and replaced only once it is gone.
        if (
            state == "failed"
            or not (task_id or journal.get_state(SEED_INTENT_KEY))
            or (task_id and self._task_status(session, ctx.deadline, task_id) in FAILED_TASK_STATES)
        ):
            self._start_seed(session, ctx, s)
        replaced = False
        watch = SeedWatch(journal.get_state(SEED_INTENT_KEY), self._wall_clock, self._clock)
        while True:
            doc = self._seed_doc(session, ctx, s)
            state, pct, problem = read_seed_state(doc, s.model_id)
            if pct is not None and ctx.reporter is not None:
                ctx.reporter.pct(pct)
            if state == "done":
                return
            task_id = journal.get_state("seed_task_id")
            task_state = self._task_status(session, ctx.deadline, task_id) if task_id else None
            if state == "failed" or task_state in FAILED_TASK_STATES:
                reason = problem or f"the task ended {task_state}"
                seed_failed(journal, "seed_task_id", f"The weights download failed: {redact_string(reason)[:400]}")
            watch.observe(doc if state == "running" else None)
            if task_state == "COMPLETE":
                # The task writes done before it ends; read again to rule out a stale read.
                if read_seed_state(self._seed_doc(session, ctx, s), s.model_id)[0] == "done":
                    return
                gone = True
            else:
                # Beam reports a queued or running task; for one it does not list (or whose
                # id was never journaled) the status document tells.
                gone = task_state not in LIVE_TASK_STATES and watch.gone()
            if gone:
                if replaced:
                    seed_failed(journal, "seed_task_id", "The weights download ended without reporting a result")
                replaced = True
                self._start_seed(session, ctx, s)
                watch = SeedWatch(journal.get_state(SEED_INTENT_KEY), self._wall_clock, self._clock)
            elif ctx.deadline.remaining() < WEIGHTS_RESERVE_SECONDS + POLL_SECONDS:
                raise ProtocolError(
                    ERR_EXECUTION_TIMEOUT,
                    "The weights are still downloading",
                    actionable_guidance="The download continues in your Beam account. Run Resume in a few minutes.",
                )
            self._sleep(POLL_SECONDS)

    def _step_endpoint(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        endpoint = endpoint_from_base(ctx.journal.get_state("gateway_url"), "the gateway")
        ctx.journal.record.endpoint_url = endpoint
        ctx.journal.save()

    def _step_health(self, session: BeamSession, ctx: StepContext, s: BeamSettings) -> None:
        ctx.health = check_endpoint(
            ctx.journal.record.endpoint_url,
            self.issued_credential(session, ctx),
            provider="beam",
            deadline=ctx.deadline,
            http=self._http,
            sleep=self._sleep,
            clock=self._clock,
            model_id=ctx.options["model_id"],
        )
        ctx.journal.record.compatibility_status = "compatible"
        ctx.journal.save()

    def issued_credential(self, session: BeamSession, ctx: StepContext) -> Dict[str, str]:
        # No dedicated token exists on Beam; see the module docstring.
        return runtime_credential("beam_bearer", token=session.token)

    # ---------- cleanup and probe ----------

    def _delete_deployments(self, session: BeamSession, deadline: Deadline, name: str) -> str:
        sdk = session.sdk
        stub = sdk.gateway.GatewayServiceStub(session.channel)
        listed = self._rpc(
            session,
            deadline,
            f"list the deployments named {name}",
            lambda: stub.list_deployments(
                sdk.gateway.ListDeploymentsRequest(filters={"name": sdk.gateway.StringList(values=[name])}, limit=100)
            ),
        )
        if not listed.ok:
            raise self._refused(f"list the deployments named {name}", listed.err_msg)
        # Every version of the deployment, matched exactly: the filter is only a hint.
        found = [d for d in listed.deployments if d.name == name]
        for deployment in found:
            if deployment.active:
                stopped = self._rpc(
                    session, deadline, f"stop {name}", lambda: stub.stop_deployment(sdk.gateway.StopDeploymentRequest(id=deployment.id))
                )
                if not stopped.ok:
                    raise self._refused(f"stop {name}", stopped.err_msg)
            deleted = self._rpc(
                session, deadline, f"delete {name}", lambda: stub.delete_deployment(sdk.gateway.DeleteDeploymentRequest(id=deployment.id))
            )
            if not deleted.ok:
                raise self._refused(f"delete {name}", deleted.err_msg)
        return "deleted" if found else "missing"

    def delete_resource(self, session: BeamSession, journal: Any, resource: Any, deadline: Deadline) -> str:
        sdk = session.sdk
        kind, name = resource.resource_type, resource.name
        if kind in ("gateway", "worker"):
            return self._delete_deployments(session, deadline, name)
        if kind == "secret":
            response = self._rpc(
                session,
                deadline,
                f"delete the secret {name}",
                lambda: sdk.secret.SecretServiceStub(session.channel).delete_secret(sdk.secret.DeleteSecretRequest(name=name)),
            )
            if response.ok:
                return "deleted"
            if "not found" in (response.err_msg or "").lower():
                return "missing"
            raise self._refused(f"delete the secret {name}", response.err_msg)
        if kind == "dict":
            return self._clear_map(session, deadline, name)
        if kind == "volume":
            response = self._rpc(
                session,
                deadline,
                f"delete the volume {name}",
                lambda: sdk.volume.VolumeServiceStub(session.channel).delete_volume(sdk.volume.DeleteVolumeRequest(name=name)),
            )
            if response.ok:
                return "deleted"
            if "not found" in (response.err_msg or "").lower():
                return "missing"
            raise self._refused(f"delete the volume {name}", response.err_msg)
        raise ProtocolError(ERR_VALIDATION, f"Unknown Beam resource type '{kind}'")

    def probe(self, endpoint_url: str, credential: Dict[str, Any], deadline: Deadline) -> Dict[str, Any]:
        return check_endpoint(
            endpoint_url, credential, provider="beam", deadline=deadline, http=self._http, sleep=self._sleep, clock=self._clock
        )


_STEP_DOING = {
    "volume": "create the weights volume",
    "state": "create the job state",
    "secret": "store the gateway secret",
    "image": "build and deploy the task queues",
    "deploy": "deploy the gateway",
    "weights": "download the weights",
    "endpoint": "find the gateway URL",
    "health": "check the endpoint",
}
