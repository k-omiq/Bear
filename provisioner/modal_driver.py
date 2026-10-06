"""Modal driver: one installation in the user's Modal workspace.

Every call below was checked against the modal 1.5.5 source. The SDK is imported
lazily inside a session and every call passes the session's explicit client, so the
user's ~/.modal.toml, profiles and MODAL_TOKEN_* variables are never read (the CLI
also points MODAL_CONFIG_PATH at a private empty path before anything imports modal).

Resources of installation <prefix>:
- volume <prefix>-weights: the pinned snapshot of the chosen render model
- dict <prefix>-jobs: job documents and the seeding status
- app <prefix>: CPU gateway (web endpoint behind proxy auth), GPU Worker, seed_weights
- proxy token: the runtime credential; Modal-Key/Modal-Secret on every request

`inspect` also lists the installations already in the workspace (`discover`), so a
second computer can take one over instead of creating another app and volume and
downloading the weights again.
"""

from __future__ import annotations

import contextlib
import importlib
import json
import os
import re
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from datetime import datetime
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
from provisioner.journal import STAGE_CREDENTIAL_CREATED, STAGE_DEPLOYING
from provisioner.protocol import (
    ERR_ACTIONABLE_PERMISSION,
    ERR_EXECUTION_FAILED,
    ERR_EXECUTION_TIMEOUT,
    ERR_ORPHANED_TOKEN,
    ERR_PROVIDER_UNAVAILABLE,
    ERR_VALIDATION,
    ProtocolError,
)
from provisioner.redaction import GLOBAL_REGISTRY, redact_string

from deploy.cloud.common.weights import SEED_STATE_KEY, marker_matches, snapshot_name
from deploy.cloud.common.analysis_seed import DIRECTORIES as ANALYSIS_DIRECTORIES
from deploy.cloud.common.contract import ANALYSIS_SAM
from deploy.cloud.common.capacity import render_copies, render_worker_memory_gib
from deploy.cloud.common.deployment import DEFAULT_ROUTING_REGION, ROUTING_REGIONS, SEED_TIMEOUT_SECONDS
from deploy.cloud.common.manifest import PRODUCTION_MODELS, production_model
from deploy.cloud.modal.settings import (
    DEFAULT_GPU,
    GPU_ALLOWLIST,
    GPU_LIST_PRICE_USD_PER_HOUR,
    SEED_FUNCTION,
    WORKER_CLASS,
    ModalSettings,
)

PRICING_URL = "https://modal.com/pricing"
# USD per hour, list price on PRICING_DATE.
GPU_PRICE_PER_HOUR = GPU_LIST_PRICE_USD_PER_HOUR
CPU_CORE_HOUR = 0.047
MEMORY_GIB_HOUR = 0.008

APP_MODULE = "deploy.cloud.modal.app"
# Time kept back while waiting for the weights, for the token, endpoint and health steps.
WEIGHTS_RESERVE_SECONDS = 240.0
POLL_SECONDS = 5.0
# The Modal client keeps retrying a server it cannot reach for about a minute, and the
# desktop gives inspect and plan 60 s; sign-in gets less so the answer is ours.
SIGN_IN_SECONDS = 40.0
TOKEN_CREATE_INTENT_KEY = "proxy_issuance_pending"
# Key, in the installation's job Dict, of the choices it was applied with. Written once
# the weights are in place; discovery on another computer reads it to offer the same.
INSTALL_OPTIONS_KEY = "install:options"
INSTALL_OPTIONS_SCHEMA = 1
# Discovery reports at most this many installations, newest first, and keeps this
# much of the inspect budget back per installation it still has to read.
DISCOVERY_LIMIT = 10
DISCOVERY_RESERVE_SECONDS = 8.0
VOLUME_LIST_LIMIT = 500
# The app name an installation gets: installation_prefix() of an id, which is the id
# itself for the ids the desktop makes. Adopting one reuses it as the installation id.
INSTALLATION_APP_NAME = re.compile(r"^mc-[a-z0-9](?:[a-z0-9-]{0,22}[a-z0-9])?$")
WEIGHTS_SUFFIX = "-weights"


def _paths(entries: Any) -> set:
    """The paths of a volume listing, without leading or trailing slashes."""
    paths = set()
    for entry in entries or ():
        path = getattr(entry, "path", None)
        if isinstance(path, str) and path.strip("/"):
            paths.add(path.strip("/"))
    return paths


def _epoch(value: Any) -> Optional[int]:
    if isinstance(value, datetime):
        try:
            return int(value.timestamp())
        except (OverflowError, OSError, ValueError):
            return None
    return None


class _SignInTimeout(Exception):
    pass


def call_within(fn: Callable[[], Any], seconds: float) -> Any:
    """fn() on a daemon thread; its result or exception, or _SignInTimeout after `seconds`.

    A call that runs over is abandoned, not stopped: the SDK's event loop cancels it
    when the helper exits.
    """
    outcome: Dict[str, Any] = {}

    def run() -> None:
        try:
            outcome["value"] = fn()
        except BaseException as exc:  # handed to the caller below
            outcome["error"] = exc

    worker = threading.Thread(target=run, name="modal-sign-in", daemon=True)
    worker.start()
    worker.join(seconds)
    if worker.is_alive():
        raise _SignInTimeout()
    if "error" in outcome:
        raise outcome["error"]
    return outcome["value"]


@dataclass(frozen=True)
class ModalCredentials:
    token_id: str
    token_secret: str

    def __repr__(self) -> str:  # never print the secret
        return "ModalCredentials(<redacted>)"


@dataclass
class ModalSession:
    sdk: Any
    client: Any
    workspace: Any
    workspace_name: str
    environment_name: str


def load_modal() -> Any:
    """The modal package with the submodules this driver uses."""
    modal = importlib.import_module("modal")
    importlib.import_module("modal.experimental")
    importlib.import_module("modal.runner")
    importlib.import_module("modal.exception")
    return modal


@contextlib.contextmanager
def _patched(target: Any, name: str, value: Any) -> Iterator[None]:
    original = getattr(target, name)
    setattr(target, name, value)
    try:
        yield
    finally:
        setattr(target, name, original)


@contextlib.contextmanager
def working_directory(path: str) -> Iterator[None]:
    """contextlib.chdir, which Python 3.10 lacks."""
    previous = os.getcwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(previous)


@contextlib.contextmanager
def _environment(values: Dict[str, str]) -> Iterator[None]:
    saved = {key: os.environ.get(key) for key in values}
    os.environ.update(values)
    try:
        yield
    finally:
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def import_modal_app(settings: ModalSettings) -> Any:
    """Import deploy/cloud/modal/app.py fresh with this installation's settings.

    The module reads its names from MC_MODAL_* at import time and bakes them into the
    images, so it is imported under exactly these variables and dropped afterwards.
    """
    sys.modules.pop(APP_MODULE, None)
    with _environment(settings.to_env()):
        try:
            return importlib.import_module(APP_MODULE)
        finally:
            sys.modules.pop(APP_MODULE, None)


async def _no_git_info() -> None:
    # modal.runner records the git commit of the working directory with each
    # deployment; the helper deploys from its own files, not from the user's repo.
    return None


class ModalDriver(BaseProviderDriver):
    provider_id = "modal"
    pipeline_steps = ("volume", "state", "image", "deploy", "weights", "token", "endpoint", "health")
    # A resume redeploys the app: `App.deploy` is idempotent and reuses built
    # images, and it is the only way an installation made by an older release
    # gets the gateway routes and worker methods this release expects.
    repeat_steps = ("image", "deploy", "token", "health")
    seed_state_keys = ("seed_call_id", SEED_INTENT_KEY)
    cleanup_order = ("proxy_token", "app", "dict", "volume")

    def __init__(
        self,
        sdk_loader: Callable[[], Any] = load_modal,
        app_loader: Callable[[ModalSettings], Any] = import_modal_app,
        http: Optional[Callable[..., Any]] = None,
        sleep: Callable[[float], None] = time.sleep,
        clock: Callable[[], float] = time.monotonic,
        sign_in_seconds: float = SIGN_IN_SECONDS,
        wall_clock: Callable[[], float] = time.time,
    ) -> None:
        self._sdk_loader = sdk_loader
        self._app_loader = app_loader
        self._http = http
        self._sleep = sleep
        self._clock = clock
        self._sign_in_seconds = sign_in_seconds
        self._wall_clock = wall_clock

    # ---------- credentials and session ----------

    def parse_credentials(self, raw: Any) -> ModalCredentials:
        if not isinstance(raw, dict):
            raise ProtocolError(ERR_VALIDATION, "Modal credentials are missing: paste the token ID and token secret")
        values = {}
        for field in ("token_id", "token_secret"):
            value = raw.get(field)
            if not isinstance(value, str) or not value.strip():
                raise ProtocolError(ERR_VALIDATION, f"Modal credential '{field}' is missing")
            value = value.strip()
            if len(value) > 256 or any(c.isspace() or ord(c) < 32 or ord(c) == 127 for c in value):
                raise ProtocolError(ERR_VALIDATION, f"Modal credential '{field}' is not a Modal token")
            values[field] = value
        if values["token_id"].startswith("wk-") or values["token_secret"].startswith("ws-"):
            raise ProtocolError(
                ERR_VALIDATION,
                "This is a Modal proxy auth token, which can only call endpoints",
                actionable_guidance="Paste an API token (ak-... and as-...) from Modal Settings, API Tokens.",
            )
        return ModalCredentials(**values)

    def _load_sdk(self) -> Any:
        try:
            return self._sdk_loader()
        except ImportError as exc:
            raise ProtocolError(
                ERR_PROVIDER_UNAVAILABLE,
                f"The Modal SDK is not available in this helper: {exc}",
                actionable_guidance="Reinstall the app; the cloud helper ships with the Modal SDK.",
            ) from None

    def _failure(self, sdk: Any, exc: BaseException, doing: str, before_changes: bool = False) -> ProtocolError:
        """Map a Modal exception to a typed helper error."""
        errors = getattr(sdk, "exception", None)
        name = type(exc).__name__
        text = redact_string(str(exc))[:400]
        if errors is not None and isinstance(exc, (errors.AuthError, errors.PermissionDeniedError)):
            return ProtocolError(
                ERR_ACTIONABLE_PERMISSION,
                f"Modal refused to {doing}: {text}",
                actionable_guidance="Check that the token is active and may create apps, volumes, dicts and proxy tokens in this workspace.",
            )
        unreachable = isinstance(exc, (OSError, TimeoutError)) or (
            errors is not None and isinstance(exc, (errors.ConnectionError, errors.TimeoutError))
        )
        if unreachable and before_changes:
            return ProtocolError(
                ERR_PROVIDER_UNAVAILABLE,
                f"Could not reach Modal to {doing}: {name}",
                actionable_guidance="Check the internet connection and try again.",
            )
        return ProtocolError(
            ERR_EXECUTION_FAILED,
            f"Modal could not {doing}: {name}: {text}",
            actionable_guidance="Run Resume to try again; finished steps are not repeated.",
        )

    @contextlib.contextmanager
    def session(self, credentials: ModalCredentials, deadline: Deadline) -> Iterator[ModalSession]:
        sdk = self._load_sdk()

        def sign_in() -> Tuple[Any, Any, Any]:
            client = sdk.Client.from_credentials(credentials.token_id, credentials.token_secret)
            # from_credentials only opens a channel; hello() is the authenticated call.
            client.hello()
            workspace = sdk.Workspace.from_context(client=client)
            workspace.hydrate()
            environment = sdk.Environment.from_context(client=client)
            environment.hydrate()
            return client, workspace, environment

        seconds = max(1.0, min(self._sign_in_seconds, deadline.remaining() - 5.0))
        try:
            client, workspace, environment = call_within(sign_in, seconds)
        except _SignInTimeout:
            raise ProtocolError(
                ERR_PROVIDER_UNAVAILABLE,
                f"Modal did not answer within {int(seconds)} seconds",
                actionable_guidance="Check the internet connection and try again.",
            ) from None
        except ProtocolError:
            raise
        except Exception as exc:
            raise self._failure(sdk, exc, "sign in", before_changes=True) from None
        yield ModalSession(
            sdk=sdk,
            client=client,
            workspace=workspace,
            workspace_name=str(workspace.name or ""),
            environment_name=str(environment.name or ""),
        )

    # ---------- inspect and plan ----------

    def inspect(self, session: ModalSession) -> AccountInspectionResult:
        where = f"workspace {session.workspace_name}"
        if session.environment_name:
            where += f", environment {session.environment_name}"
        return AccountInspectionResult(
            provider="modal",
            account_id=session.workspace_name,
            workspace_name=session.workspace_name,
            authenticated=True,
            permissions_granted=[],
            permissions_missing=[],
            eligible=True,
            platform_notes=f"Modal {where}. Permissions are checked when each resource is created.",
            environment_name=session.environment_name,
        )

    def discover(self, session: ModalSession, deadline: Deadline) -> Tuple[List[Dict[str, Any]], bool]:
        """Manga Cleaner installations in this workspace and environment. Reads only.

        An installation is a deployed app named like one (`mc-...`) beside its
        `<app>-weights` volume. For each: which models are on the volume, from the
        ready markers the seed writes last (a listing, no download), the choices it
        was applied with when it recorded them, and when it was made and deployed.
        Anything that cannot be read is left out rather than guessed.
        """
        sdk, client, env = session.sdk, session.client, session.environment_name
        try:
            apps = sdk.experimental.list_deployed_apps(environment_name=env, client=client)
        except Exception:
            return [], False
        candidates: Dict[str, Any] = {}
        for app in apps or ():
            name = getattr(app, "name", None)
            if isinstance(name, str) and INSTALLATION_APP_NAME.match(name):
                candidates[name] = app
        if not candidates:
            return [], True
        try:
            volumes = list(sdk.Volume.objects.list(max_objects=VOLUME_LIST_LIMIT, environment_name=env, client=client))
        except Exception:
            return [], False
        complete = len(volumes) < VOLUME_LIST_LIMIT
        found: List[Tuple[str, Any, Optional[int]]] = []
        for volume in volumes:
            name = getattr(volume, "name", None)
            if not isinstance(name, str) or not name.endswith(WEIGHTS_SUFFIX):
                continue
            app_name = name[: -len(WEIGHTS_SUFFIX)]
            if app_name not in candidates:
                continue
            created = None
            with contextlib.suppress(Exception):
                created = _epoch(volume.info().created_at)
            found.append((app_name, volume, created))
        found.sort(key=lambda item: (item[2] or 0, item[0]), reverse=True)
        if len(found) > DISCOVERY_LIMIT:
            complete, found = False, found[:DISCOVERY_LIMIT]
        installations: List[Dict[str, Any]] = []
        for app_name, volume, created in found:
            if deadline.remaining() < DISCOVERY_RESERVE_SECONDS:
                complete = False
                break
            entry: Dict[str, Any] = {
                "installation_id": app_name,
                "app_name": app_name,
                "created_at": created,
                "deployed_at": None,
                "weights_checked": False,
                "models_ready": [],
                "analysis_ready": [],
                "options": None,
            }
            with contextlib.suppress(Exception):
                models, analysis = self._ready_on_volume(volume)
                entry.update(weights_checked=True, models_ready=models, analysis_ready=analysis)
            entry["options"] = self._recorded_options(session, f"{app_name}-jobs", env or None)
            app_id = getattr(candidates[app_name], "app_id", None)
            if isinstance(app_id, str) and app_id:
                with contextlib.suppress(Exception):
                    entry["deployed_at"] = _epoch(sdk.experimental.get_app_lifecycle(app_id, client=client).deployed_at)
            installations.append(entry)
        return installations, complete

    @staticmethod
    def _ready_on_volume(volume: Any) -> Tuple[List[str], List[str]]:
        """(model ids, analysis capabilities) whose ready marker is on the volume.

        The seed writes each marker last, after the files are verified, so a marker
        is a finished download. Listing the volume transfers no weights; each listed
        model marker (a few hundred bytes) is read, because one written for an older
        file set (before a recipe gained its LoRA) is not this snapshot. The worker
        would refuse it, so it must not stop the seed from fetching what is missing.
        """
        root = _paths(volume.listdir("/"))
        models = []
        for model_id in PRODUCTION_MODELS:
            marker = f"{snapshot_name(model_id)}.ready.json"
            if marker not in root:
                continue
            with contextlib.suppress(Exception):
                if marker_matches(json.loads(b"".join(volume.read_file(marker))), model_id):
                    models.append(model_id)
        analysis: List[str] = []
        if "analysis" in root:
            listed = _paths(volume.listdir("analysis", recursive=True))
            analysis = sorted(
                capability for capability, directory in ANALYSIS_DIRECTORIES.items()
                if f"analysis/{directory}/ready.json" in listed
            )
        return models, analysis

    def _recorded_options(self, session: ModalSession, dict_name: str, environment: Optional[str]) -> Optional[Dict[str, Any]]:
        """The choices an installation recorded at apply time, when valid today; else None."""
        try:
            store = session.sdk.Dict.from_name(dict_name, environment_name=environment, client=session.client)
            doc = store.get(INSTALL_OPTIONS_KEY)
        except Exception:
            return None
        if not isinstance(doc, dict) or doc.get("schema") != INSTALL_OPTIONS_SCHEMA:
            return None
        try:
            return self.normalize_options(
                {key: doc.get(key) for key in ("gpu", "idle_seconds", "model_id", "analysis_models")}
                | {"denoise": doc.get("denoise", False),
                   "routing_region": doc.get("routing_region", DEFAULT_ROUTING_REGION)}
            )
        except ProtocolError:
            return None

    def normalize_options(self, options: Any) -> Dict[str, Any]:
        return normalize_options(options, GPU_ALLOWLIST, DEFAULT_GPU, "modal")

    def settings_for(self, installation_id: str, environment_name: str, options: Dict[str, Any]) -> ModalSettings:
        return ModalSettings.for_installation(
            installation_id,
            installation_prefix(installation_id),
            environment_name=environment_name,
            gpu=options["gpu"],
            idle_seconds=options["idle_seconds"],
            model_id=options["model_id"],
            analysis_models=tuple(options["analysis_models"]),
            denoise=options.get("denoise", False),
            routing_region=options.get("routing_region", DEFAULT_ROUTING_REGION),
        )

    def plan(self, inspection: AccountInspectionResult, installation_id: str, options: Dict[str, Any]) -> DeploymentPlan:
        s = self.settings_for(installation_id, inspection.environment_name, options)
        gpu, idle = s.gpu, s.idle_seconds
        model = production_model(s.model_id)
        # The deployed worker's own sizing (common/capacity.py), so the quote is what bills.
        worker_memory = render_worker_memory_gib(model, gpu, "modal")
        copies = render_copies(model, gpu)
        copies_note = f", {copies} renders at once" if copies > 1 else ""
        seed_memory = 24 if ANALYSIS_SAM in s.analysis_models else 4
        seed_timeout = SEED_TIMEOUT_SECONDS * (2 if ANALYSIS_SAM in s.analysis_models else 1)
        seed_hour = 2 * CPU_CORE_HOUR + seed_memory * MEMORY_GIB_HOUR
        analysis_note = (
            f" A separate {gpu} GPU analysis worker (at most 1 container, stops {idle} s after a request) "
            "runs only for the selected SAM-TS-L lettering mask or Ogkalu comic text & bubble detector (Full) analysis."
        ) if s.analysis_models else ""
        if s.denoise:
            analysis_note += (
                f" Page denoise runs on {'the same' if s.analysis_models else f'a separate {gpu} GPU'} analysis worker "
                f"(at most 1 container, stops {idle} s after a page), one page per request."
            )
        resources = [
            PlanResource("volume", s.volume_name, f"Modal Volume holding the pinned {model.label} weights ({model.total_bytes / 1e9:.1f} GB)"
                         + (" and selected analysis graphs" if s.analysis_models else "")
                         + (" and the page denoise models (about 0.5 GB)" if s.denoise else "") + "."),
            PlanResource("dict", s.dict_name, "Modal Dict with job state, so status checks never start a GPU."),
            PlanResource(
                "app",
                s.app_name,
                f"Modal App: a CPU gateway (0.25 CPU, 0.5 GiB) behind Modal proxy auth, reached through Modal's {s.routing_region} servers, a {gpu} GPU worker "
                f"(2 CPU, {worker_memory} GiB, at most 1 container{copies_note}, stops {idle} s after the last render, 600 s per job) "
                f"and a seed function (2 CPU, {seed_memory} GiB, up to {seed_timeout // 3600} h per attempt) that downloads the weights. "
                "Each GPU worker start loads the model from the volume, about a minute." + analysis_note,
            ),
            PlanResource(
                "proxy_token",
                f"{s.app_name}-proxy-token",
                "Modal proxy auth token for endpoints in this environment (or the workspace if environment access control is unavailable). The app keeps it in this computer's keychain.",
            ),
        ]
        price = GPU_PRICE_PER_HOUR[gpu]
        worker_hour = price + 2 * CPU_CORE_HOUR + worker_memory * MEMORY_GIB_HOUR
        cost = (
            f"Billed by Modal per second of use. A {gpu} worker costs about ${worker_hour:.2f} per hour while it runs "
            f"({gpu} ${price:.2f}/h, 2 CPU cores at ${CPU_CORE_HOUR}/h each, {worker_memory} GiB at ${MEMORY_GIB_HOUR}/GiB/h), "
            f"during each render and for {idle} s after it."
            + (f" Selected analysis or page denoise also starts a separate {gpu} GPU worker on demand and keeps it warm {idle} s; it is billed at the same GPU rate." if s.analysis_models or s.denoise else "")
            + " The gateway runs only while it answers requests "
            f"(0.25 CPU, 0.5 GiB). Initial seeding and any retries use 2 CPU and {seed_memory} GiB "
            f"for up to {seed_timeout // 3600} h per attempt, about ${seed_hour:.2f}/h while active. "
            "Storage for the weights volume is billed by Modal. Nothing runs while idle. "
            f"List prices on {PRICING_DATE}; check {PRICING_URL}."
        )
        return build_plan(
            provider="modal",
            installation_id=installation_id,
            inspection=inspection,
            app_name=s.app_name,
            options=options,
            gpu_allowlist=GPU_ALLOWLIST,
            allocation={
                "worker_cpu": 2.0,
                "worker_memory_gib": worker_memory,
                "gateway_cpu": 0.25,
                "gateway_memory_gib": 0.5,
                "seed_cpu": 2.0,
                "seed_memory_gib": seed_memory,
                "seed_timeout_seconds": seed_timeout,
                "max_containers": 1,
                "job_timeout_seconds": 600,
                "environment": inspection.environment_name,
                # Where the app's requests enter Modal. The review screen offers the list.
                "routing_region": s.routing_region,
                "routing_region_options": list(ROUTING_REGIONS),
            },
            resources=resources,
            required_permissions=[
                "Deploy Modal apps",
                "Create volumes and dicts",
                "Create proxy auth tokens",
            ],
            cost=cost,
            notes=[
                f"The weights download on a seed container (2 CPU, {seed_memory} GiB, up to {seed_timeout // 3600} h per attempt); setup waits for it. Failed attempts can incur additional cost.",
                "A render starts the GPU worker; a cold start loads 5.5 GB of weights first.",
            ],
            cleanup_summary=[
                "Delete the proxy auth token",
                f"Stop the Modal app {s.app_name}",
                f"Delete the dict {s.dict_name}",
                f"Delete the volume {s.volume_name} and the weights in it",
            ],
        )

    # ---------- pipeline ----------

    def _settings(self, ctx: StepContext) -> ModalSettings:
        rec = ctx.journal.record
        return self.settings_for(rec.installation_id, ctx.journal.get_state("environment_name", ""), ctx.options)

    def run_step(self, step: str, session: ModalSession, ctx: StepContext) -> None:
        settings = self._settings(ctx)
        handler = getattr(self, f"_step_{step}")
        try:
            handler(session, ctx, settings)
        except ProtocolError:
            raise
        except Exception as exc:
            raise self._failure(session.sdk, exc, _STEP_DOING[step]) from None

    def _env(self, settings: ModalSettings) -> Optional[str]:
        return settings.environment

    def _step_volume(self, session: ModalSession, ctx: StepContext, s: ModalSettings) -> None:
        ctx.journal.record_resource(s.volume_name, "volume", s.volume_name, STAGE_DEPLOYING)
        session.sdk.Volume.objects.create(
            s.volume_name, allow_existing=True, environment_name=self._env(s), client=session.client
        )

    def _step_state(self, session: ModalSession, ctx: StepContext, s: ModalSettings) -> None:
        ctx.journal.record_resource(s.dict_name, "dict", s.dict_name, STAGE_DEPLOYING)
        session.sdk.Dict.objects.create(
            s.dict_name, allow_existing=True, environment_name=self._env(s), client=session.client
        )

    def _step_image(self, session: ModalSession, ctx: StepContext, s: ModalSettings) -> None:
        ctx.journal.record_resource(s.app_name, "app", s.app_name, STAGE_DEPLOYING)
        module = self._app_loader(s)
        # App.deploy uploads the deploy package, builds the three images and creates
        # the functions; it is idempotent for the same app name.
        with tempfile.TemporaryDirectory(prefix="mc-modal-") as workdir, working_directory(workdir):
            with _patched(session.sdk.runner, "get_git_commit_info", _no_git_info):
                module.app.deploy(name=s.app_name, environment_name=self._env(s), client=session.client)

    def _lookup(self, session: ModalSession, s: ModalSettings, name: str, cls: bool = False) -> Any:
        kind = session.sdk.Cls if cls else session.sdk.Function
        obj = kind.from_name(s.app_name, name, environment_name=self._env(s), client=session.client)
        obj.hydrate()
        return obj

    def _step_deploy(self, session: ModalSession, ctx: StepContext, s: ModalSettings) -> None:
        for name in (s.gateway_function, SEED_FUNCTION):
            self._lookup(session, s, name)
        self._lookup(session, s, WORKER_CLASS, cls=True)

    def _start_seed(self, session: ModalSession, ctx: StepContext, s: ModalSettings, store: Any) -> Any:
        seed = self._lookup(session, s, SEED_FUNCTION)
        # A new seeding job overwrites the status document; drop a stale one first so an
        # old failure is not read as this job's result.
        store.pop(SEED_STATE_KEY, None)
        # The intent is journaled before the spawn: a helper stopped before it records
        # the call id then waits on that call instead of spawning another.
        ctx.journal.set_state(seed_call_id=None, **{SEED_INTENT_KEY: int(self._wall_clock())})
        call = seed.spawn()
        ctx.journal.set_state(seed_call_id=str(call.object_id))
        return call

    def _step_weights(self, session: ModalSession, ctx: StepContext, s: ModalSettings) -> None:
        store = session.sdk.Dict.from_name(s.dict_name, environment_name=self._env(s), client=session.client)
        self._wait_for_weights(session, ctx, s, store)
        self._record_options(store, s)

    def _record_options(self, store: Any, s: ModalSettings) -> None:
        """Note the choices beside the job state, for discovery on another computer. Best effort."""
        doc = {
            "schema": INSTALL_OPTIONS_SCHEMA,
            "gpu": s.gpu,
            "idle_seconds": s.idle_seconds,
            "model_id": s.model_id,
            "analysis_models": list(s.analysis_models),
            "denoise": s.denoise,
            "routing_region": s.routing_region,
            "recorded_at": int(self._wall_clock()),
        }
        with contextlib.suppress(Exception):
            store.put(INSTALL_OPTIONS_KEY, doc)

    def _already_seeded(self, session: ModalSession, s: ModalSettings) -> bool:
        """Whether the volume holds this model and every selected analysis graph.

        A listing that fails counts as no: the seed then runs, and it skips what is
        already verified on the volume, so the worst case is a short CPU job. Page
        denoise models carry no ready marker, so with denoise on the seed always
        runs and checks them itself.
        """
        if s.denoise:
            return False
        try:
            volume = session.sdk.Volume.from_name(s.volume_name, environment_name=self._env(s), client=session.client)
            models, analysis = self._ready_on_volume(volume)
        except Exception:
            return False
        return s.model_id in models and set(s.analysis_models) <= set(analysis)

    def _wait_for_weights(self, session: ModalSession, ctx: StepContext, s: ModalSettings, store: Any) -> None:
        errors = session.sdk.exception
        journal = ctx.journal
        doc = store.get(SEED_STATE_KEY)
        state, _, _ = read_seed_state(doc, s.model_id)
        call = None
        call_id = journal.get_state("seed_call_id")
        # A seed the journal knows of, by its call id or only by the intent to spawn it,
        # may still run: it is waited on below and replaced only once it is gone.
        in_flight = bool(call_id or journal.get_state(SEED_INTENT_KEY))
        if not in_flight:
            # No seed of this journal's: a new installation, or one taken over from
            # another computer. Its volume says what is there; a "done" document
            # alone only covers the FLUX snapshot, not the analysis graphs chosen now.
            if self._already_seeded(session, s):
                return
            call = self._start_seed(session, ctx, s, store)
        elif state == "done":
            return
        elif state == "failed":
            call = self._start_seed(session, ctx, s, store)
        elif call_id:
            call = session.sdk.FunctionCall.from_id(call_id, client=session.client)
        replaced = False
        watch = SeedWatch(journal.get_state(SEED_INTENT_KEY), self._wall_clock, self._clock)
        while True:
            doc = store.get(SEED_STATE_KEY)
            state, pct, problem = read_seed_state(doc, s.model_id)
            if pct is not None and ctx.reporter is not None:
                ctx.reporter.pct(pct)
            if state == "done":
                return
            if state == "failed":
                seed_failed(journal, "seed_call_id", f"The weights download failed: {redact_string(problem)}")
            watch.observe(doc if state == "running" else None)
            # Without a call id only the status document tells whether the seed runs.
            gone = call is None and watch.gone()
            if call is not None:
                try:
                    result = call.get(timeout=0)
                except TimeoutError:
                    result = None  # still running
                except errors.OutputExpiredError:
                    result, gone = None, True  # Modal no longer knows the call
                except Exception as exc:
                    # The call itself failed (timeout, crash); the next run starts a new one.
                    seed_failed(
                        journal,
                        "seed_call_id",
                        f"The weights download stopped: {type(exc).__name__}: {redact_string(str(exc))[:300]}",
                    )
                if isinstance(result, dict):
                    if result.get("status") in ("present", "seeded"):
                        return
                    seed_failed(
                        journal,
                        "seed_call_id",
                        "The weights download failed: "
                        + redact_string(f"{result.get('error_code')}: {result.get('message')}")[:400],
                    )
            if gone:
                if replaced:
                    seed_failed(journal, "seed_call_id", "The weights download ended without reporting a result")
                replaced = True
                call = self._start_seed(session, ctx, s, store)
                watch = SeedWatch(journal.get_state(SEED_INTENT_KEY), self._wall_clock, self._clock)
            elif ctx.deadline.remaining() < WEIGHTS_RESERVE_SECONDS + POLL_SECONDS:
                raise ProtocolError(
                    ERR_EXECUTION_TIMEOUT,
                    "The weights are still downloading",
                    actionable_guidance="The download continues in your Modal account. Run Resume in a few minutes.",
                )
            self._sleep(POLL_SECONDS)

    def _step_token(self, session: ModalSession, ctx: StepContext, s: ModalSettings) -> None:
        errors = session.sdk.exception
        journal = ctx.journal
        tokens = session.workspace.proxy_tokens
        name = f"{s.app_name}-proxy-token"
        if journal.get_state(TOKEN_CREATE_INTENT_KEY) and journal.get_resource("proxy_token", name) is None:
            raise ProtocolError(
                ERR_ORPHANED_TOKEN,
                "A Modal proxy token may have been created before its ID was saved.",
                actionable_guidance="Open the Modal dashboard and remove the proxy token created during this setup attempt.",
                remedy_steps=[
                    "Open Modal Settings, Proxy Auth Tokens, and remove the token created during this setup attempt.",
                    "Use Cleanup for this installation, then start a new setup. Resume will stay blocked because the token ID is unknown.",
                ],
            )
        # Keep the old credential usable until the replacement passes the endpoint
        # health check. Both IDs stay journaled if setup is interrupted.
        old = journal.get_resource("proxy_token", name)
        if old is not None:
            old.name = f"{s.app_name[:37]}-retiring-{old.resource_id[-8:]}"
            journal.save()
        journal.set_state(**{TOKEN_CREATE_INTENT_KEY: True})
        issued = tokens.create()
        token_id = getattr(issued, "token_id", None)
        token_secret = getattr(issued, "token_secret", None)
        if not token_id or not token_secret:
            raise ProtocolError(ERR_EXECUTION_FAILED, "Modal did not return a proxy token")
        credential = runtime_credential("modal_proxy", token_id=token_id, token_secret=token_secret)
        GLOBAL_REGISTRY.register(credential["token_secret"])
        journal.record_resource(credential["token_id"], "proxy_token", name, STAGE_CREDENTIAL_CREATED)
        journal.record.runtime_credential_ref = {
            "resource_type": "proxy_token",
            "name": name,
            "resource_id": credential["token_id"],
        }
        journal.save()
        journal.set_state(**{TOKEN_CREATE_INTENT_KEY: None})
        if s.environment_name:
            try:
                tokens.allow(credential["token_id"], s.environment_name)
            except errors.Error:
                # Workspaces without per-environment access control accept the
                # token everywhere; the health step shows whether the edge takes it.
                journal.set_state(environment_allowed=False)
        ctx.runtime_credential = credential

    def _step_endpoint(self, session: ModalSession, ctx: StepContext, s: ModalSettings) -> None:
        gateway = self._lookup(session, s, s.gateway_function)
        endpoint = endpoint_from_base(gateway.get_web_url(), "the gateway")
        ctx.journal.record.endpoint_url = endpoint
        ctx.journal.save()

    def _step_health(self, session: ModalSession, ctx: StepContext, s: ModalSettings) -> None:
        if ctx.runtime_credential is None:
            raise ProtocolError(ERR_EXECUTION_FAILED, "No runtime credential was issued in this run")
        ctx.health = check_endpoint(
            ctx.journal.record.endpoint_url,
            ctx.runtime_credential,
            provider="modal",
            deadline=ctx.deadline,
            http=self._http,
            sleep=self._sleep,
            clock=self._clock,
            model_id=ctx.options["model_id"],
        )
        ctx.journal.record.compatibility_status = "compatible"
        ctx.journal.save()
        # Revoke only after the new token was allowed and accepted by the edge.
        for resource in list(ctx.journal.record.resources):
            if resource.resource_type == "proxy_token" and "-retiring-" in resource.name:
                try:
                    session.workspace.proxy_tokens.delete(resource.resource_id)
                except session.sdk.exception.NotFoundError:
                    pass
                ctx.journal.remove_resource("proxy_token", resource.name)

    def issued_credential(self, session: ModalSession, ctx: StepContext) -> Dict[str, str]:
        if ctx.runtime_credential is None:
            raise ProtocolError(ERR_EXECUTION_FAILED, "No runtime credential was issued in this run")
        return ctx.runtime_credential

    # ---------- cleanup and probe ----------

    def delete_resource(self, session: ModalSession, journal: Any, resource: Any, deadline: Deadline) -> str:
        sdk = session.sdk
        environment = journal.get_state("environment_name", "") or None
        try:
            if resource.resource_type == "proxy_token":
                session.workspace.proxy_tokens.delete(resource.resource_id)
            elif resource.resource_type == "app":
                sdk.experimental.stop_app(resource.name, environment_name=environment, client=session.client)
            elif resource.resource_type == "dict":
                sdk.Dict.objects.delete(resource.name, allow_missing=True, environment_name=environment, client=session.client)
            elif resource.resource_type == "volume":
                sdk.Volume.objects.delete(resource.name, allow_missing=True, environment_name=environment, client=session.client)
            else:
                raise ProtocolError(ERR_VALIDATION, f"Unknown Modal resource type '{resource.resource_type}'")
        except ProtocolError:
            raise
        except sdk.exception.NotFoundError:
            return "missing"
        except Exception as exc:
            raise self._failure(sdk, exc, f"delete the {resource.resource_type} {resource.name}") from None
        return "deleted"

    def probe(self, endpoint_url: str, credential: Dict[str, Any], deadline: Deadline) -> Dict[str, Any]:
        return check_endpoint(
            endpoint_url, credential, provider="modal", deadline=deadline, http=self._http, sleep=self._sleep, clock=self._clock
        )


_STEP_DOING = {
    "volume": "create the weights volume",
    "state": "create the job dict",
    "image": "build and deploy the app",
    "deploy": "find the deployed functions",
    "weights": "download the weights",
    "token": "issue the proxy auth token",
    "endpoint": "find the gateway URL",
    "health": "check the endpoint",
}
