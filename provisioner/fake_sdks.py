"""In-memory stand-ins for the modal and beta9 SDKs, for tests only.

Each fake has the call surface the drivers use, with the same parameter names as
modal 1.5.5 and beta9 0.1.268 (test_sdk_fidelity checks that against the real
packages when they are installed). A fake cloud keeps what exists and records every
call, so tests can assert on side effects, and `kill_after` raises `Killed` right
after a chosen call has taken effect, which is what a process kill looks like to the
journal. Nothing here touches the network.
"""

from __future__ import annotations

import contextlib
import itertools
import json
import pickle
from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any, Callable, Dict, List, Optional, Tuple

from deploy.cloud.common.contract import PROTOCOL_VERSION, HealthResponse
from deploy.cloud.common.manifest import MODEL_PROD_FLUX, PROD_SNAPSHOT_TOTAL_BYTES, get_production_model_info
from deploy.cloud.common.analysis_seed import DIRECTORIES as ANALYSIS_DIRECTORIES
from deploy.cloud.common.weights import SEED_STATE_KEY, expected_marker, seed_state, snapshot_name

_ids = itertools.count(1)


class Killed(BaseException):
    """Stands in for the desktop killing the helper: nothing catches it."""


class FakeCloudBase:
    def __init__(self) -> None:
        self.calls: List[Tuple[str, Dict[str, Any]]] = []
        self.kill_after: Optional[str] = None
        self.fail: Dict[str, BaseException] = {}
        # Seeding: while a seed job runs, each read of the status document takes the
        # next entry (a dict, or raw bytes to store as they are); a done or failed
        # entry ends the job, and a job whose script runs out writes nothing more.
        self.seed_script: List[Any] = [
            seed_state("running", PROD_SNAPSHOT_TOTAL_BYTES // 2, now=1.0),
            seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=2.0),
        ]
        # Scripts of the seed jobs started after the first one, in order.
        self.next_seed_scripts: List[List[Any]] = []
        self.seed_started = 0
        self.seed_running = False

    def record(self, call: str, /, **details: Any) -> None:
        failure = self.fail.get(call)
        if failure is not None:
            raise failure
        self.calls.append((call, details))

    def done(self, name: str) -> None:
        if self.kill_after == name:
            self.kill_after = None
            raise Killed(name)

    def count(self, name: str) -> int:
        return sum(1 for call, _ in self.calls if call == name)

    def start_seed(self) -> None:
        if self.seed_started and self.next_seed_scripts:
            self.seed_script = self.next_seed_scripts.pop(0)
        self.seed_started += 1
        self.seed_running = True

    def next_seed_doc(self) -> Any:
        """The next status document of the running seed job, or None when none is due."""
        if not (self.seed_running and self.seed_script):
            return None
        doc = self.seed_script.pop(0)
        if isinstance(doc, dict) and doc.get("state") in ("done", "failed"):
            self.seed_running = False
        return doc

    def advance_seed(self, store: Dict[str, Any]) -> None:
        doc = self.next_seed_doc()
        if doc is not None:
            store[SEED_STATE_KEY] = doc


def edge_http(cloud: "FakeCloudBase", provider: str, host: Callable[[], Optional[str]], allowed: Callable[[Dict[str, str]], bool],
              served_model: Callable[[], str] = lambda: MODEL_PROD_FLUX):
    """The provider edge in front of the gateway: auth, then the two CPU-only routes.

    /model-info names the model the gateway was deployed with, as the real one does.
    """

    def get(url: str, headers: Dict[str, str], timeout: float) -> Tuple[Optional[int], bytes]:
        cloud.calls.append(("http.get", {"url": url}))
        base = host()
        if base is None or not url.startswith(base):
            return None, b""
        if not allowed(headers):
            return 401, b'{"error": "unauthorized"}'
        route = url[len(base):]
        if route == "/mc/v1/health":
            body = HealthResponse(status="ok", provider=provider, protocol_version=PROTOCOL_VERSION).to_dict()
        elif route == "/mc/v1/model-info":
            body = get_production_model_info(provider, served_model()).to_dict()
        else:
            return 404, b"{}"
        return 200, json.dumps(body).encode("utf-8")

    return get


# ---------------------------------------------------------------- modal


class _ModalErrors:
    class Error(Exception):
        pass

    class AuthError(Error):
        pass

    class PermissionDeniedError(Error):
        pass

    class NotFoundError(Error):
        pass

    class InvalidError(Error):
        pass

    class AlreadyExistsError(Error):
        pass

    class ConnectionError(Error):
        pass

    class TimeoutError(Error):
        pass

    class OutputExpiredError(TimeoutError):
        pass


class FakeModalCloud(FakeCloudBase):
    workspace_name = "studio-ws"

    def __init__(self, environment: str = "main") -> None:
        super().__init__()
        self.environment = environment
        self.valid_credentials = {("ak-good-token-id", "as-good-token-secret")}
        self.volumes: set = set()
        self.dicts: Dict[str, Dict[str, Any]] = {}
        self.apps: Dict[str, Any] = {}
        self.proxy_tokens: Dict[str, Dict[str, Any]] = {}
        self.calls_by_id: Dict[str, Dict[str, Any]] = {}
        # Outcomes of the next spawned calls ({"expired": True}, {"result": ...}), then running.
        self.next_call_outcomes: List[Dict[str, Any]] = []
        self.hello_error: Optional[BaseException] = None
        # Set to an Event to make hello() hang until it is set: a server that never answers.
        self.hello_gate: Optional[Any] = None
        self.allow_error: Optional[BaseException] = None
        self.environments_seen: set = set()
        self.clients: List["FakeModalClient"] = []
        # Files on each volume, as the paths a listing returns, and when it was made.
        self.volume_files: Dict[str, set] = {}
        # Contents of the files read_file serves (the ready markers), by volume and path.
        self.volume_contents: Dict[str, Dict[str, bytes]] = {}
        self.volume_created: Dict[str, float] = {}
        # The settings of the app whose seed job runs, so a finished job leaves its
        # ready markers on that app's volume as the real seed does.
        self.seeding: Optional[Any] = None

    def advance_seed(self, store: Dict[str, Any]) -> None:
        doc = self.next_seed_doc()
        if doc is None:
            return
        store[SEED_STATE_KEY] = doc
        if isinstance(doc, dict) and doc.get("state") == "done" and self.seeding is not None:
            self.mark_seeded(self.seeding.volume_name, self.seeding.model_id, self.seeding.analysis_models)

    def mark_seeded(self, volume: str, model_id: str, analysis: Any = ()) -> None:
        """Leave the ready markers a finished seed writes last."""
        files = self.volume_files.setdefault(volume, set())
        marker = f"{snapshot_name(model_id)}.ready.json"
        files.add(marker)
        self.volume_contents.setdefault(volume, {})[marker] = json.dumps(expected_marker(model_id)).encode()
        for capability in analysis:
            files.add(f"analysis/{ANALYSIS_DIRECTORIES[capability]}/ready.json")

    def seed_installation(self, app_name: str, model_id: str, analysis: Any = (), created: float = 1_760_000_000.0,
                          options: Optional[Dict[str, Any]] = None) -> None:
        """An installation some other computer made: deployed app, seeded volume, job dict."""
        self.apps[app_name] = {"functions": {"gateway", "seed_weights", "Worker", "AnalysisGPU"}, "gpu": "L4",
                               "app_id": f"ap-{app_name}"}
        self.volumes.add(f"{app_name}-weights")
        self.volume_created[f"{app_name}-weights"] = created
        self.mark_seeded(f"{app_name}-weights", model_id, analysis)
        store = self.dicts.setdefault(f"{app_name}-jobs", {})
        store[SEED_STATE_KEY] = seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=created, model_id=model_id)
        if options is not None:
            store["install:options"] = options

    def check_env(self, environment_name: Optional[str]) -> None:
        self.environments_seen.add(environment_name)

    def web_url(self, app_name: str) -> Optional[str]:
        app = self.apps.get(app_name)
        if not app:
            return None
        # As Modal names them: the Function's name in the label, and the routing
        # region as a subdomain for any region but us-east.
        settings = app.get("settings")
        region = getattr(settings, "routing_region", "us-east")
        name = getattr(settings, "gateway_function", "gateway").replace("_", "-")
        return f"https://{self.workspace_name}--{app_name}-{name}{'' if region == 'us-east' else '.' + region}.modal.run"

    def http(self):
        def allowed(headers: Dict[str, str]) -> bool:
            token = self.proxy_tokens.get(headers.get("Modal-Key", ""))
            return token is not None and token["secret"] == headers.get("Modal-Secret")

        def host() -> Optional[str]:
            return self.web_url(next(iter(self.apps), "")) if self.apps else None

        def served_model() -> str:
            settings = self.apps.get(next(iter(self.apps), ""), {}).get("settings")
            return settings.model_id if settings is not None else MODEL_PROD_FLUX

        return edge_http(self, "modal", host, allowed, served_model)

    def sdk(self) -> Any:
        if getattr(self, "_sdk", None) is None:
            self._sdk = build_fake_modal(self)
        return self._sdk


class FakeModalClient:
    def __init__(self, cloud: FakeModalCloud, token_id: str, token_secret: str) -> None:
        self.cloud = cloud
        self.token = (token_id, token_secret)

    def hello(self) -> None:
        self.cloud.record("Client.hello")
        if self.cloud.hello_gate is not None:
            self.cloud.hello_gate.wait(10)
        if self.cloud.hello_error is not None:
            raise self.cloud.hello_error
        if self.token not in self.cloud.valid_credentials:
            raise _ModalErrors.AuthError("Token missing or invalid")


def build_fake_modal(cloud: FakeModalCloud) -> Any:
    errors = _ModalErrors

    def client_of(client: Any) -> None:
        if not isinstance(client, FakeModalClient):
            raise AssertionError("every Modal call must pass the session's explicit client")

    class Client:
        @staticmethod
        def from_credentials(token_id: str, token_secret: str) -> FakeModalClient:
            cloud.record("Client.from_credentials")
            client = FakeModalClient(cloud, token_id, token_secret)
            cloud.clients.append(client)
            return client

    class ProxyTokens:
        def create(self) -> Any:
            cloud.record("proxy_tokens.create")
            n = next(_ids)
            token = SimpleNamespace(token_id=f"wk-token{n:06d}", token_secret=f"ws-secret{n:06d}")
            cloud.proxy_tokens[token.token_id] = {"secret": token.token_secret, "environments": []}
            cloud.done("proxy_tokens.create")
            return token

        def allow(self, proxy_token_id: str, environment_name: str) -> None:
            cloud.record("proxy_tokens.allow", token_id=proxy_token_id, environment=environment_name)
            if cloud.allow_error is not None:
                raise cloud.allow_error
            cloud.proxy_tokens[proxy_token_id]["environments"].append(environment_name)

        def delete(self, proxy_token_id: str) -> None:
            cloud.record("proxy_tokens.delete", token_id=proxy_token_id)
            if cloud.proxy_tokens.pop(proxy_token_id, None) is None:
                raise errors.NotFoundError(f"token {proxy_token_id} not found")
            cloud.done("proxy_tokens.delete")

    class Workspace:
        def __init__(self, client: Any) -> None:
            client_of(client)
            self.name: Optional[str] = None
            self.proxy_tokens = ProxyTokens()

        @staticmethod
        def from_context(*, client: Any = None) -> "Workspace":
            return Workspace(client)

        def hydrate(self, client: Any = None) -> "Workspace":
            cloud.record("Workspace.hydrate")
            self.name = cloud.workspace_name
            return self

    class Environment:
        def __init__(self) -> None:
            self.name: Optional[str] = None

        @staticmethod
        def from_context(*, client: Any = None) -> "Environment":
            client_of(client)
            return Environment()

        def hydrate(self, client: Any = None) -> "Environment":
            cloud.record("Environment.hydrate")
            self.name = cloud.environment
            return self

    class VolumeManager:
        @staticmethod
        def create(name: str, *, version: Optional[int] = None, allow_existing: bool = False,
                   environment_name: Optional[str] = None, client: Any = None, experimental_options: Any = None) -> None:
            client_of(client)
            cloud.check_env(environment_name)
            cloud.record("Volume.objects.create", name=name, allow_existing=allow_existing)
            if name in cloud.volumes and not allow_existing:
                raise errors.AlreadyExistsError(name)
            if name not in cloud.volumes:
                cloud.volume_created[name] = 1_770_000_000.0 + len(cloud.volumes)
            cloud.volumes.add(name)
            cloud.done("Volume.objects.create")

        @staticmethod
        def delete(name: str, *, allow_missing: bool = False, environment_name: Optional[str] = None, client: Any = None) -> None:
            client_of(client)
            cloud.record("Volume.objects.delete", name=name)
            if name not in cloud.volumes and not allow_missing:
                raise errors.NotFoundError(name)
            cloud.volumes.discard(name)
            cloud.volume_files.pop(name, None)

        @staticmethod
        def list(*, max_objects: Optional[int] = None, created_before: Any = None, environment_name: str = "",
                 client: Any = None) -> List["FakeVolume"]:
            client_of(client)
            cloud.check_env(environment_name)
            cloud.record("Volume.objects.list")
            names = sorted(cloud.volumes, key=lambda n: cloud.volume_created.get(n, 0.0), reverse=True)
            handles = [FakeVolume(name) for name in names]
            return handles[:max_objects] if max_objects is not None else handles

    class FakeVolume:
        def __init__(self, name: str) -> None:
            self.name = name

        def info(self) -> Any:
            created = cloud.volume_created.get(self.name, 0.0)
            return SimpleNamespace(name=self.name, created_at=datetime.fromtimestamp(created, timezone.utc), created_by="someone")

        def listdir(self, path: str, *, recursive: bool = False) -> List[Any]:
            cloud.record("Volume.listdir", name=self.name, path=path)
            if self.name not in cloud.volumes:
                raise errors.NotFoundError(f"Volume {self.name} not found")
            prefix = path.strip("/")
            entries = set()
            for file in cloud.volume_files.get(self.name, set()):
                if prefix and not file.startswith(prefix + "/"):
                    continue
                rest = file[len(prefix) + 1:] if prefix else file
                parts = rest.split("/")
                if recursive:
                    entries.add(file)
                    for depth in range(1, len(parts)):
                        entries.add("/".join(([prefix] if prefix else []) + parts[:depth]))
                else:
                    entries.add("/".join(([prefix] if prefix else []) + parts[:1]))
            if prefix and not entries:
                raise errors.NotFoundError(f"{path} not found")
            return [SimpleNamespace(path=entry, type=1, mtime=0, size=0) for entry in sorted(entries)]

        def read_file(self, path: str) -> Any:
            cloud.record("Volume.read_file", name=self.name, path=path)
            data = cloud.volume_contents.get(self.name, {}).get(path.strip("/"))
            if data is None:
                raise errors.NotFoundError(f"{path} not found")
            yield data

    class DictManager:
        @staticmethod
        def create(name: str, *, allow_existing: bool = False, environment_name: Optional[str] = None, client: Any = None) -> None:
            client_of(client)
            cloud.check_env(environment_name)
            cloud.record("Dict.objects.create", name=name)
            cloud.dicts.setdefault(name, {})
            cloud.done("Dict.objects.create")

        @staticmethod
        def delete(name: str, *, allow_missing: bool = False, environment_name: Optional[str] = None, client: Any = None) -> None:
            client_of(client)
            cloud.record("Dict.objects.delete", name=name)
            if name not in cloud.dicts and not allow_missing:
                raise errors.NotFoundError(name)
            cloud.dicts.pop(name, None)

    class FakeDict:
        def __init__(self, name: str) -> None:
            self.name = name

        def _store(self) -> Dict[str, Any]:
            if self.name not in cloud.dicts:
                raise errors.NotFoundError(f"Dict {self.name} not found")
            return cloud.dicts[self.name]

        def get(self, key: Any, default: Any = None) -> Any:
            store = self._store()
            if key == SEED_STATE_KEY:
                cloud.advance_seed(store)
            return store.get(key, default)

        def pop(self, key: Any, default: Any = None) -> Any:
            return self._store().pop(key, default)

        def put(self, key: Any, value: Any, *, skip_if_exists: bool = False) -> bool:
            cloud.record("Dict.put", name=self.name, key=key)
            store = self._store()
            if skip_if_exists and key in store:
                return False
            store[key] = value
            return True

    class Dict_:
        objects = DictManager()

        @staticmethod
        def from_name(name: str, *, environment_name: Optional[str] = None, create_if_missing: bool = False, client: Any = None) -> FakeDict:
            client_of(client)
            cloud.check_env(environment_name)
            return FakeDict(name)

    class Volume:
        objects = VolumeManager()

        @staticmethod
        def from_name(name: str, *, environment_name: Optional[str] = None, create_if_missing: bool = False,
                      client: Any = None) -> FakeVolume:
            client_of(client)
            cloud.check_env(environment_name)
            return FakeVolume(name)

    class FakeFunctionCall:
        def __init__(self, call_id: str) -> None:
            self.object_id = call_id

        def get(self, timeout: Optional[float] = None, *, index: int = 0) -> Any:
            cloud.record("FunctionCall.get", call_id=self.object_id)
            call = cloud.calls_by_id.get(self.object_id)
            if call is None or call.get("expired"):
                raise errors.OutputExpiredError()
            if call.get("error") is not None:
                raise call["error"]
            if call.get("result") is not None:
                return call["result"]
            raise TimeoutError()

    class Function:
        def __init__(self, app_name: str, name: str) -> None:
            self.app_name, self.name = app_name, name

        @staticmethod
        def from_name(app_name: str, name: str, *, version: Optional[int] = None,
                      environment_name: Optional[str] = None, client: Any = None) -> "Function":
            client_of(client)
            cloud.check_env(environment_name)
            return Function(app_name, name)

        def hydrate(self, client: Any = None) -> "Function":
            cloud.record("Function.hydrate", name=self.name)
            app = cloud.apps.get(self.app_name)
            if app is None or self.name not in app["functions"]:
                raise errors.NotFoundError(f"{self.app_name}.{self.name}")
            return self

        def get_web_url(self) -> Optional[str]:
            return cloud.web_url(self.app_name) if self.name.startswith("gateway") else None

        def spawn(self, *args: Any, **kwargs: Any) -> FakeFunctionCall:
            cloud.record("Function.spawn", name=self.name)
            cloud.seeding = cloud.apps.get(self.app_name, {}).get("settings")
            call_id = f"fc-{next(_ids):06d}"
            cloud.calls_by_id[call_id] = cloud.next_call_outcomes.pop(0) if cloud.next_call_outcomes else {"result": None}
            cloud.start_seed()
            cloud.done("Function.spawn")
            return FakeFunctionCall(call_id)

    class Cls(Function):
        @staticmethod
        def from_name(app_name: str, name: str, *, version: Optional[int] = None,
                      environment_name: Optional[str] = None, client: Any = None) -> "Cls":
            client_of(client)
            cloud.check_env(environment_name)
            return Cls(app_name, name)

    class FunctionCall:
        @staticmethod
        def from_id(function_call_id: str, client: Any = None) -> FakeFunctionCall:
            client_of(client)
            return FakeFunctionCall(function_call_id)

    def stop_app(name: str, *, environment_name: Optional[str] = None, client: Any = None) -> None:
        client_of(client)
        cloud.record("stop_app", name=name)
        if cloud.apps.pop(name, None) is None:
            raise errors.NotFoundError(f"App {name} not found")

    def list_deployed_apps(environment_name: str = "", client: Any = None) -> List[Any]:
        client_of(client)
        cloud.check_env(environment_name)
        cloud.record("list_deployed_apps")
        return [SimpleNamespace(app_id=app.get("app_id", f"ap-{name}"), name=name, containers=0)
                for name, app in cloud.apps.items()]

    def get_app_lifecycle(app_id: str, *, client: Any = None) -> Any:
        client_of(client)
        cloud.record("get_app_lifecycle", app_id=app_id)
        when = datetime.fromtimestamp(1_770_500_000, timezone.utc)
        return SimpleNamespace(version=3, created_at=when, created_by="someone", deployed_at=when,
                               deployed_by="someone", stopped_at=None, stopped_by=None)

    async def get_git_commit_info() -> Any:
        raise AssertionError("the driver must not collect git info of the working directory")

    return SimpleNamespace(
        __version__="fake",
        Client=Client,
        Workspace=Workspace,
        Environment=Environment,
        Volume=Volume,
        Dict=Dict_,
        Function=Function,
        Cls=Cls,
        FunctionCall=FunctionCall,
        exception=errors,
        experimental=SimpleNamespace(stop_app=stop_app, list_deployed_apps=list_deployed_apps,
                                     get_app_lifecycle=get_app_lifecycle),
        runner=SimpleNamespace(get_git_commit_info=get_git_commit_info),
    )


class FakeModalApp:
    """What `import deploy.cloud.modal.app` gives the driver: an app with deploy()."""

    def __init__(self, cloud: FakeModalCloud, settings: Any) -> None:
        self.cloud = cloud
        self.settings = settings

    def deploy(self, *, name: Optional[str] = None, environment_name: Optional[str] = None, tag: str = "",
               client: Any = None, strategy: str = "rolling") -> Any:
        import asyncio

        if not isinstance(client, FakeModalClient):
            raise AssertionError("App.deploy must get the session's client")
        # The runner's git lookup must be replaced for the duration of the deploy.
        assert asyncio.run(self.cloud.sdk().runner.get_git_commit_info()) is None
        self.cloud.check_env(environment_name)
        self.cloud.record("App.deploy", name=name, environment_name=environment_name)
        if self.settings.volume_name not in self.cloud.volumes or self.settings.dict_name not in self.cloud.dicts:
            raise _ModalErrors.NotFoundError("the app references a volume or dict that does not exist")
        self.cloud.apps[name] = {"functions": {self.settings.gateway_function, "seed_weights", "Worker", "AnalysisGPU"}, "gpu": self.settings.gpu,
                                 "app_id": f"ap-{name}", "settings": self.settings}
        self.cloud.done("App.deploy")
        return SimpleNamespace(app_id="ap-fake")


def modal_app_loader(cloud: FakeModalCloud) -> Callable[[Any], Any]:
    def load(settings: Any) -> Any:
        cloud.record("import app", env=settings.to_env())
        return SimpleNamespace(app=FakeModalApp(cloud, settings))

    return load


# ---------------------------------------------------------------- beam


def _message(name: str, fields: Dict[str, Any]) -> type:
    """A betterproto-like message: keyword fields only, unknown ones rejected."""

    def __init__(self: Any, *args: Any, **kwargs: Any) -> None:
        names = list(fields)
        if len(args) > len(names):
            raise TypeError(f"{name} takes at most {len(names)} positional fields")
        values = dict(zip(names, args))
        for key, value in kwargs.items():
            if key not in fields:
                raise TypeError(f"{name} has no field {key!r}")
            values[key] = value
        for key, default in fields.items():
            setattr(self, key, values.get(key, default() if callable(default) else default))

    return type(name, (), {"__init__": __init__, "_fields": tuple(fields)})


GATEWAY_MESSAGES = {
    "AuthorizeRequest": {},
    "AuthorizeResponse": {"ok": False, "workspace_id": "", "new_token": "", "error_msg": ""},
    "StringList": {"values": list},
    "ListDeploymentsRequest": {"filters": dict, "limit": 0},
    "ListDeploymentsResponse": {"ok": False, "err_msg": "", "deployments": list},
    "Deployment": {"id": "", "name": "", "active": False, "stub_id": "", "stub_type": "", "version": 0},
    "StopDeploymentRequest": {"id": ""},
    "StopDeploymentResponse": {"ok": False, "err_msg": ""},
    "DeleteDeploymentRequest": {"id": ""},
    "DeleteDeploymentResponse": {"ok": False, "err_msg": ""},
    "ListTasksRequest": {"filters": dict, "limit": 0},
    "ListTasksResponse": {"ok": False, "err_msg": "", "tasks": list, "total": 0},
    "Task": {"id": "", "status": "", "stub_id": ""},
}
SECRET_MESSAGES = {
    "CreateSecretRequest": {"name": "", "value": ""},
    "CreateSecretResponse": {"ok": False, "err_msg": "", "id": "", "name": ""},
    "UpdateSecretRequest": {"name": "", "value": ""},
    "UpdateSecretResponse": {"ok": False, "err_msg": ""},
    "DeleteSecretRequest": {"name": ""},
    "DeleteSecretResponse": {"ok": False, "err_msg": ""},
}
VOLUME_MESSAGES = {
    "GetOrCreateVolumeRequest": {"name": ""},
    "GetOrCreateVolumeResponse": {"ok": False, "err_msg": "", "volume": None},
    "DeleteVolumeRequest": {"name": ""},
    "DeleteVolumeResponse": {"ok": False, "err_msg": ""},
}
MAP_MESSAGES = {
    "MapSetRequest": {"name": "", "key": "", "value": b"", "ttl": 0},
    "MapSetResponse": {"ok": False, "err_msg": ""},
    "MapGetRequest": {"name": "", "key": ""},
    "MapGetResponse": {"ok": False, "value": b""},
    "MapDeleteRequest": {"name": "", "key": ""},
    "MapDeleteResponse": {"ok": False},
    "MapKeysRequest": {"name": ""},
    "MapKeysResponse": {"ok": False, "keys": list},
}


class _StatusCode:
    def __init__(self, name: str) -> None:
        self.name = name

    def __repr__(self) -> str:
        return f"StatusCode.{self.name}"


class FakeRpcError(Exception):
    def __init__(self, code: _StatusCode, details: str = "") -> None:
        super().__init__(details)
        self._code = code

    def code(self) -> _StatusCode:
        return self._code


class FakeBeamCloud(FakeCloudBase):
    workspace_id = "ws-0001-fake"

    def __init__(self) -> None:
        super().__init__()
        self.valid_tokens = {"b9-good-api-key"}
        self.status = SimpleNamespace(
            **{name: _StatusCode(name) for name in ("UNAUTHENTICATED", "PERMISSION_DENIED", "UNAVAILABLE", "DEADLINE_EXCEEDED")}
        )
        self.volumes: set = set()
        self.maps: Dict[str, Dict[str, bytes]] = {}
        self.secrets: Dict[str, str] = {}
        self.deployments: Dict[str, Dict[str, Any]] = {}
        self.tasks: Dict[str, str] = {}
        # Statuses the next seed tasks are listed with (None: never listed), then RUNNING.
        self.next_task_statuses: List[Optional[str]] = []
        # RPC name to the reason it answers ok=False with (list_tasks, map_keys, map_delete).
        self.refused: Dict[str, str] = {}
        self.channel_open = 0
        self.global_channel: Any = None
        self.authorize_error: Optional[BaseException] = None
        self.rpc_timeouts: List[float] = []
        self.path_style_urls = False

    def rpc_error(self, code: str, details: str = "") -> FakeRpcError:
        return FakeRpcError(getattr(self.status, code), details)

    def invoke_url(self, name: str, version: int) -> str:
        if self.path_style_urls:
            return f"https://app.beam.cloud/endpoint/{name}"
        return f"https://{name}-a1b2c3d-v{version}.app.beam.cloud"

    def gateway_base(self) -> Optional[str]:
        live = [d for d in self.deployments.values() if d["name"].endswith("-gateway") and d["active"]]
        return live[-1]["invoke_url"] if live else None

    def http(self):
        def allowed(headers: Dict[str, str]) -> bool:
            auth = headers.get("Authorization", "")
            return auth.startswith("Bearer ") and auth[len("Bearer "):] in self.valid_tokens

        def served_model() -> str:
            live = [d for d in self.deployments.values() if d["name"].endswith("-gateway") and d["active"]]
            return (live[-1]["env"].get("MC_BEAM_MODEL_ID") if live else None) or MODEL_PROD_FLUX

        return edge_http(self, "beam", self.gateway_base, allowed, served_model)

    def post(self, url: str, token: str, body: Dict[str, Any], timeout: float) -> Tuple[Optional[int], bytes]:
        self.record("http.post", url=url)
        if token not in self.valid_tokens:
            return 401, b"{}"
        seed = [d for d in self.deployments.values() if d["invoke_url"] == url and d["name"].endswith("-seed")]
        if not seed:
            return 404, b"{}"
        task_id = f"task-{next(_ids):06d}"
        status = self.next_task_statuses.pop(0) if self.next_task_statuses else "RUNNING"
        if status is not None:
            self.tasks[task_id] = status
        self.start_seed()
        self.done("http.post")
        return 200, json.dumps({"task_id": task_id}).encode("utf-8")

    def sdk(self) -> Any:
        if getattr(self, "_sdk", None) is None:
            self._sdk = build_fake_beta9(self)
        return self._sdk


def build_fake_beta9(cloud: FakeBeamCloud) -> Any:
    gateway = SimpleNamespace(**{name: _message(name, fields) for name, fields in GATEWAY_MESSAGES.items()})
    secret = SimpleNamespace(**{name: _message(name, fields) for name, fields in SECRET_MESSAGES.items()})
    volume = SimpleNamespace(**{name: _message(name, fields) for name, fields in VOLUME_MESSAGES.items()})
    map_ = SimpleNamespace(**{name: _message(name, fields) for name, fields in MAP_MESSAGES.items()})

    class Channel:
        def __init__(self, addr: str, token: Optional[str] = None, credentials: Any = None, options: Any = None,
                     retry: Any = None, metadata: Any = None) -> None:
            cloud.record("Channel", addr=addr)
            self.addr, self.token, self.config = addr, token, None
            self.closed = False
            cloud.channel_open += 1

        def close(self) -> None:
            if not self.closed:
                self.closed = True
                cloud.channel_open -= 1

    def authed(channel: Channel) -> None:
        if channel.token not in cloud.valid_tokens:
            raise cloud.rpc_error("UNAUTHENTICATED", "invalid token")
        if cloud.global_channel is not channel or channel.config is None:
            raise AssertionError("the explicit channel with its config must be the SDK's global channel")

    class GatewayServiceStub:
        def __init__(self, channel: Channel) -> None:
            self.channel = channel

        def authorize(self, request: Any) -> Any:
            cloud.record("authorize")
            if cloud.authorize_error is not None:
                raise cloud.authorize_error
            if self.channel.token not in cloud.valid_tokens:
                return gateway.AuthorizeResponse(ok=False, error_msg="invalid token")
            return gateway.AuthorizeResponse(ok=True, workspace_id=cloud.workspace_id)

        def list_deployments(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("list_deployments", filters={k: list(v.values) for k, v in request.filters.items()})
            found = [gateway.Deployment(id=i, name=d["name"], active=d["active"]) for i, d in cloud.deployments.items()]
            return gateway.ListDeploymentsResponse(ok=True, deployments=found)

        def stop_deployment(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("stop_deployment", id=request.id)
            cloud.deployments[request.id]["active"] = False
            return gateway.StopDeploymentResponse(ok=True)

        def delete_deployment(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("delete_deployment", id=request.id)
            cloud.deployments.pop(request.id)
            return gateway.DeleteDeploymentResponse(ok=True)

        def list_tasks(self, request: Any) -> Any:
            authed(self.channel)
            if "list_tasks" in cloud.refused:
                return gateway.ListTasksResponse(ok=False, err_msg=cloud.refused["list_tasks"])
            wanted = request.filters["id"].values
            tasks = [gateway.Task(id=t, status=s) for t, s in cloud.tasks.items() if t in wanted]
            return gateway.ListTasksResponse(ok=True, tasks=tasks, total=len(tasks))

    class SecretServiceStub:
        def __init__(self, channel: Channel) -> None:
            self.channel = channel

        def create_secret(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("create_secret", name=request.name)
            if request.name in cloud.secrets:
                return secret.CreateSecretResponse(ok=False, err_msg="secret already exists")
            cloud.secrets[request.name] = request.value
            cloud.done("create_secret")
            return secret.CreateSecretResponse(ok=True, name=request.name)

        def update_secret(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("update_secret", name=request.name)
            cloud.secrets[request.name] = request.value
            return secret.UpdateSecretResponse(ok=True)

        def delete_secret(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("delete_secret", name=request.name)
            if cloud.secrets.pop(request.name, None) is None:
                return secret.DeleteSecretResponse(ok=False, err_msg="secret not found")
            return secret.DeleteSecretResponse(ok=True)

    class VolumeServiceStub:
        def __init__(self, channel: Channel) -> None:
            self.channel = channel

        def get_or_create_volume(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("get_or_create_volume", name=request.name)
            cloud.volumes.add(request.name)
            cloud.done("get_or_create_volume")
            return volume.GetOrCreateVolumeResponse(ok=True)

        def delete_volume(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("delete_volume", name=request.name)
            if request.name not in cloud.volumes:
                return volume.DeleteVolumeResponse(ok=False, err_msg="volume not found")
            cloud.volumes.discard(request.name)
            return volume.DeleteVolumeResponse(ok=True)

    class MapServiceStub:
        def __init__(self, channel: Channel) -> None:
            self.channel = channel

        def map_set(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("map_set", name=request.name, key=request.key)
            cloud.maps.setdefault(request.name, {})[request.key] = request.value
            return map_.MapSetResponse(ok=True)

        def map_get(self, request: Any) -> Any:
            authed(self.channel)
            store = cloud.maps.setdefault(request.name, {})
            if request.key == SEED_STATE_KEY:
                doc = cloud.next_seed_doc()
                if doc is not None:
                    store[request.key] = doc if isinstance(doc, bytes) else pickle.dumps(doc)
            value = store.get(request.key)
            return map_.MapGetResponse(ok=value is not None, value=value or b"")

        def map_delete(self, request: Any) -> Any:
            authed(self.channel)
            cloud.record("map_delete", name=request.name, key=request.key)
            if "map_delete" in cloud.refused:
                return map_.MapDeleteResponse(ok=False)
            cloud.maps.get(request.name, {}).pop(request.key, None)
            return map_.MapDeleteResponse(ok=True)

        def map_keys(self, request: Any) -> Any:
            authed(self.channel)
            if "map_keys" in cloud.refused:
                return map_.MapKeysResponse(ok=False)
            # A map is only its keys: one that holds none lists as empty (assumed; the
            # server is not in the SDK source, see docs/cloud-provisioning.md).
            return map_.MapKeysResponse(ok=True, keys=list(cloud.maps.get(request.name, {})))

    gateway.GatewayServiceStub = GatewayServiceStub
    secret.SecretServiceStub = SecretServiceStub
    volume.VolumeServiceStub = VolumeServiceStub
    map_.MapServiceStub = MapServiceStub

    class ConfigContext:
        def __init__(self, token: Optional[str] = None, gateway_host: Optional[str] = None,
                     gateway_port: Optional[int] = None, api_url: Optional[str] = None) -> None:
            self.token, self.gateway_host, self.gateway_port, self.api_url = token, gateway_host, gateway_port, api_url

    def set_channel(channel: Any = None, context: Any = None) -> None:
        cloud.global_channel = channel

    def unset_channel() -> None:
        cloud.global_channel = None

    @contextlib.contextmanager
    def rpc_timeout(seconds: float):
        cloud.rpc_timeouts.append(seconds)
        yield

    @contextlib.contextmanager
    def redirect_terminal_to_buffer(buffer: Any):
        cloud.terminal_buffer = buffer
        yield

    return SimpleNamespace(
        Channel=Channel,
        rpc_timeout=rpc_timeout,
        ConfigContext=ConfigContext,
        base=SimpleNamespace(set_channel=set_channel, unset_channel=unset_channel),
        terminal=SimpleNamespace(redirect_terminal_to_buffer=redirect_terminal_to_buffer),
        gateway=gateway,
        secret=secret,
        volume=volume,
        map=map_,
        grpc=SimpleNamespace(RpcError=FakeRpcError, StatusCode=cloud.status),
    )


class FakeBeamHandler:
    def __init__(self, cloud: FakeBeamCloud, env: Dict[str, str], stage_dir: Any, handler: str) -> None:
        self.cloud, self.env, self.stage_dir, self.handler = cloud, env, stage_dir, handler

    def deploy(self, name: Optional[str] = None, context: Any = None, invocation_details_func: Any = None,
               rollout: str = "auto", **invocation_details_options: Any) -> Tuple[Dict[str, Any], bool]:
        import os

        cloud = self.cloud
        if os.getcwd() != str(self.stage_dir) or not (self.stage_dir / "mc_beam_app.py").is_file():
            raise AssertionError("Beam deploys from the staged directory as the working directory")
        if cloud.global_channel is None:
            raise AssertionError("deploy needs the session channel")
        if self.handler == "gateway" and not self.env.get("MC_BEAM_WORKER_URL", "").startswith("https://"):
            raise AssertionError("the gateway is deployed with the worker URL")
        if self.handler == "gateway" and self.env.get("MC_BEAM_ANALYSIS_MODELS") and not self.env.get("MC_BEAM_ANALYSIS_URL", "").startswith("https://"):
            raise AssertionError("the gateway is deployed with the analysis GPU URL")
        if self.handler == "gateway" and self.env.get("MC_BEAM_SECRET") not in cloud.secrets:
            return {}, False
        cloud.record("deploy", name=name, handler=self.handler)
        version = sum(1 for d in cloud.deployments.values() if d["name"] == name) + 1
        deployment_id = f"dep-{next(_ids):06d}"
        for other in cloud.deployments.values():
            if other["name"] == name:
                other["active"] = False
        cloud.deployments[deployment_id] = {
            "name": name,
            "active": True,
            "invoke_url": cloud.invoke_url(name, version),
            "env": dict(self.env),
        }
        cloud.done("deploy")
        return (
            {
                "deployment_id": deployment_id,
                "stub_id": "stub-1",
                "status": "accepted",
                "deployment_name": name,
                "invoke_url": cloud.invoke_url(name, version),
                "version": version,
                "warning": "",
                "rollout_action": "",
            },
            True,
        )


def beam_app_loader(cloud: FakeBeamCloud) -> Callable[[Any, Dict[str, str]], Any]:
    def load(stage_dir: Any, env: Dict[str, str]) -> Any:
        cloud.record("import app", env=dict(env))
        return SimpleNamespace(
            seed=FakeBeamHandler(cloud, env, stage_dir, "seed"),
            render=FakeBeamHandler(cloud, env, stage_dir, "render"),
            analyze=FakeBeamHandler(cloud, env, stage_dir, "analyze"),
            gateway=FakeBeamHandler(cloud, env, stage_dir, "gateway"),
        )

    return load
