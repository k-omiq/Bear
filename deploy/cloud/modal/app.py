"""Modal app: CPU gateway, GPU worker and weights seeding.

The provisioner exports the MC_MODAL_* variables (see settings.py), imports this
module and deploys `app`. Each image carries the same variables, so a container that
imports this module again rebuilds exactly the app that was deployed.

- `gateway` serves /mc/v1 on a small CPU container behind Modal proxy auth. It keeps
  job documents in a modal.Dict, so health and model-info never start a GPU.
- `Worker` loads the pinned pipeline from the weights volume once per GPU container
  and renders one job per call. It holds `RENDER_COPIES` copies and serves that many
  renders at once, each on its own copy (common/capacity.py sizes it from the GPU's
  memory). It scales to zero after the idle window.
- `AnalysisGPU` serves `ANALYSIS_MAX_INPUTS` tiles or pages at once on threads that
  share its ONNX Runtime sessions and page denoise models.
- `Worker` and `AnalysisGPU` publish a heartbeat in the job Dict (common/gpu.py), so
  the gateway can say whether a GPU is up without starting one, and each has a
  `release` method a user's stop uses to send a warm container away.
- `seed_weights` downloads the pinned snapshot into the weights volume on a CPU
  container. GPU containers never download.
- `Worker` loads and warms the pipeline in `@modal.enter(snap=True)` and refuses to
  finish that phase without a loaded pipeline (WorkerNotReady). Its memory snapshot is
  off: live, Modal built a new GPU snapshot on every cold start instead of restoring
  one (docs/findings.md), so the split is kept only to turn it back on. With no
  snapshot Modal runs both enter phases on every start. `AnalysisGPU` snapshots CPU
  memory only: imports before
  the snapshot, ONNX Runtime sessions after it. Everything per start (heartbeat,
  release marker, clocks) runs in `@modal.enter(snap=False)`, which also runs after
  every restore. The gateway and `seed_weights` are not snapshotted.

Only this package's Python files are uploaded (include_source=False plus one
filtered add_local_dir), never anything else from the machine that deploys.
"""

from __future__ import annotations

import threading
import time
from pathlib import Path
from typing import Any, Dict

import modal

from deploy.cloud.common.capacity import (
    ANALYSIS_MAX_INPUTS,
    GATEWAY_TIMEOUT_SECONDS,
    render_copies,
    render_worker_memory_gib,
)
from deploy.cloud.common.deployment import (
    DEFAULT_ROUTING_REGION,
    JOB_TIMEOUT_SECONDS,
    SEED_TIMEOUT_SECONDS,
    WORKER_STARTUP_TIMEOUT_SECONDS,
)
from deploy.cloud.common.manifest import get_production_model_info, production_limits, production_model
from deploy.cloud.common.weights import run_seed
from deploy.cloud.common.analysis_seed import ANALYSIS_SAM, AnalysisUnavailable, VerifiedGraphCache, analysis_capabilities, run_analysis_seed
from deploy.cloud.common.gpu import GpuControl, GpuHeartbeat, release_container
from deploy.cloud.common.release import code_digest, shipped
from deploy.cloud.common.worker import WorkerRuntime
from deploy.cloud.modal.backend import (
    ModalDictStore,
    ModalGpuJobs,
    ModalWorkerDispatcher,
    analysis_failure,
    cancel_tracked_analysis,
    count_tracked_analysis,
    denoise_failure,
    prepare_render_container,
    run_tracked_analysis,
    start_gpu_container,
    worker_render,
)
from deploy.cloud.modal.settings import GPU_LIST_PRICE_USD_PER_HOUR, WEIGHTS_MOUNT, ModalSettings

SETTINGS = ModalSettings.from_env()
# Pipeline copies one render container holds, and so the renders it runs at once.
RENDER_COPIES = render_copies(production_model(SETTINGS.model_id), SETTINGS.gpu)

PYTHON_VERSION = "3.12"
TORCH_INDEX_URL = "https://download.pytorch.org/whl/cu129"
TORCH_REQUIREMENT = "torch==2.13.0"
# spandrel (page denoise, OpenModelDB .pth models) imports torchvision, which must come
# from the same CUDA index as torch or its compiled operators do not register.
TORCHVISION_REQUIREMENT = "torchvision==0.28.0"
DENOISE_REQUIREMENTS = ("spandrel==0.4.2",)
GPU_REQUIREMENTS = (
    "diffusers==0.39.0",
    "sdnq==0.2.4",
    "transformers==5.15.0",
    "accelerate==1.14.0",
    "safetensors==0.8.0",
    "huggingface-hub==1.24.0",
    "pillow==12.3.0",
    "numpy==2.4.6",
    # Qwen-Image-Edit loads its Lightning LoRA as a PEFT adapter.
    "peft==0.21.1",
)
SEED_REQUIREMENTS = ("huggingface-hub==1.24.0",)
SAM_EXPORT_REQUIREMENTS = (
    "onnx==1.19.0", "onnx-ir==1.0.0", "onnxscript==0.5.6",
    "numpy==2.3.3", "pillow==11.3.0", "safetensors==0.6.2",
    "requests==2.34.2", "einops==0.8.1", "pyyaml==6.0.3",
)

# deploy/cloud/modal/app.py -> deploy/
DEPLOY_ROOT = Path(__file__).resolve().parents[2]
REPO_ROOT = DEPLOY_ROOT.parent
REMOTE_DEPLOY_ROOT = "/root/deploy"  # /root is on PYTHONPATH in Modal containers
ANALYSIS_SOURCE = "/root/analysis-source"

def _not_shipped(relative: Path) -> bool:
    """add_local_dir ignore predicate: ship the package inits plus common/ and modal/ sources."""
    return not shipped(relative)


def _finish(image: modal.Image, extra_env: Dict[str, str] | None = None) -> modal.Image:
    env = {**SETTINGS.to_env(), **(extra_env or {})}
    # add_local_dir must be the last step: files are attached at container start.
    return image.env(env).add_local_dir(DEPLOY_ROOT, REMOTE_DEPLOY_ROOT, ignore=_not_shipped)


# The gateway validates decoded analysis PNGs before it dispatches a GPU tile.
# Keep Pillow in this CPU image as well as in the analysis worker image.
gateway_image = _finish(
    modal.Image.debian_slim(python_version=PYTHON_VERSION).pip_install("pillow==12.3.0")
)

_seed_base = modal.Image.debian_slim(python_version=PYTHON_VERSION).pip_install(*SEED_REQUIREMENTS)
if ANALYSIS_SAM in SETTINGS.analysis_models:
    _seed_base = (_seed_base.pip_install(TORCH_REQUIREMENT, index_url=TORCH_INDEX_URL)
                  .pip_install(*SAM_EXPORT_REQUIREMENTS))
seed_image = _finish(
    _seed_base,
    {"HF_HUB_DISABLE_TELEMETRY": "1", "HF_HUB_DISABLE_PROGRESS_BARS": "1"},
)
if ANALYSIS_SAM in SETTINGS.analysis_models:
    seed_image = (seed_image
        .add_local_file(REPO_ROOT / "sam-bootstrap/bootstrap.py", f"{ANALYSIS_SOURCE}/bootstrap.py")
        .add_local_file(REPO_ROOT / "spikes/sam-ts-l/export_mask.py", f"{ANALYSIS_SOURCE}/export_mask.py")
        .add_local_file(REPO_ROOT / "spikes/sam-ts-l/fixtures/synthetic_page.png", f"{ANALYSIS_SOURCE}/synthetic_page.png"))

# torchvision: Qwen-Image-Edit's Qwen2-VL processor refuses to load without it.
gpu_image = _finish(
    modal.Image.debian_slim(python_version=PYTHON_VERSION)
    .pip_install(TORCH_REQUIREMENT, TORCHVISION_REQUIREMENT, index_url=TORCH_INDEX_URL)
    .pip_install(*GPU_REQUIREMENTS),
    {"HF_HUB_OFFLINE": "1", "HF_HUB_DISABLE_TELEMETRY": "1"},
)
_analysis_base = modal.Image.debian_slim(python_version=PYTHON_VERSION)
if SETTINGS.denoise:
    _analysis_base = (_analysis_base
        .pip_install(TORCH_REQUIREMENT, TORCHVISION_REQUIREMENT, index_url=TORCH_INDEX_URL)
        .pip_install("onnxruntime-gpu==1.23.2", "numpy==2.4.6", "pillow==12.3.0", *DENOISE_REQUIREMENTS))
else:
    _analysis_base = (_analysis_base
        .pip_install(TORCH_REQUIREMENT, index_url=TORCH_INDEX_URL)
        .pip_install("onnxruntime-gpu==1.23.2", "numpy==2.4.6", "pillow==12.3.0"))
analysis_gpu_image = _finish(_analysis_base, {"HF_HUB_OFFLINE": "1", "HF_HUB_DISABLE_TELEMETRY": "1"})

weights_volume = modal.Volume.from_name(SETTINGS.volume_name, environment_name=SETTINGS.environment)
jobs_dict = modal.Dict.from_name(SETTINGS.dict_name, environment_name=SETTINGS.environment)

app = modal.App(SETTINGS.app_name, include_source=False)

# A denoise page is one call on the analysis GPU; a 4x sharpen step on a large page
# takes far longer than an analysis tile. The app polls a page or tile it submitted
# (backend.ModalGpuJobs), so the call may run this long whatever Modal's web request
# limit; backend.GPU_JOB_LONGEST_CALL_SECONDS assumes 600 at most, startup included.
ANALYSIS_TIMEOUT_SECONDS = 600 if SETTINGS.denoise else 180

# Memory snapshot options per class. Modal takes a snapshot after the snap=True enter
# methods on a cold start and restores later cold starts from it; a redeploy that
# changes the code or the class config (GPU type included) makes a new one. Volume
# content changes do not, so a re-seed that deletes or moves the snapshot's weight
# files needs a redeploy.
# - Worker: off. With CPU and GPU memory (`"experimental_options": {"enable_gpu_snapshot":
#   True}`, experimental in Modal) the one live run built a fresh snapshot on every cold
#   start, about 1 min 45 s on top of the 55 s load, and never restored one (see
#   "Modal GPU snapshots" in docs/findings.md). Without it a cold start loads the
#   pipeline from the volume.
# - AnalysisGPU: CPU memory only. GPU snapshots checkpoint every CUDA context with
#   NVIDIA cuda-checkpoint (modal/_runtime/gpu_memory_snapshot.py), and nothing in
#   the SDK or Modal's docs says ONNX Runtime CUDA sessions survive a restore.
# - gateway, seed_weights: none. The gateway's snapshot would hold only this module's
#   pure-Python imports (Modal builds the ASGI app after the snapshot point), a small
#   gain for a CPU container that starts fast; seeding runs once.
WORKER_SNAPSHOT: Dict[str, Any] = {"enable_memory_snapshot": False}
ANALYSIS_SNAPSHOT: Dict[str, Any] = {"enable_memory_snapshot": True}


def _heartbeat(role: str, startup_timeout: int, call_timeout: int) -> GpuHeartbeat:
    return GpuHeartbeat(ModalDictStore(jobs_dict), role, SETTINGS.gpu, SETTINGS.idle_seconds,
                        startup_timeout, call_timeout)


@app.function(
    image=seed_image,
    volumes={WEIGHTS_MOUNT: weights_volume},
    cpu=2.0,
    memory=24576 if ANALYSIS_SAM in SETTINGS.analysis_models else 4096,
    timeout=SEED_TIMEOUT_SECONDS * (2 if ANALYSIS_SAM in SETTINGS.analysis_models else 1),
    max_containers=1,
)
def seed_weights() -> Dict[str, Any]:
    # Idempotent: a seeded volume returns at once and a partial download resumes.
    # run_seed retries internally, publishes progress to the job Dict for the
    # provisioner and returns failures as a typed result instead of raising.
    store = ModalDictStore(jobs_dict)
    analysis = run_analysis_seed(WEIGHTS_MOUNT, SETTINGS.analysis_models, ANALYSIS_SOURCE, store,
                                 model_id=SETTINGS.model_id, commit=weights_volume.commit)
    if analysis["status"] == "failed":
        return analysis
    if SETTINGS.denoise:
        from deploy.cloud.common.denoise import DenoiseError, ensure_models, preset_models
        try:
            ensure_models(WEIGHTS_MOUNT, preset_models(), download=True, commit=weights_volume.commit)
        except (DenoiseError, OSError) as exc:
            return {"status": "failed", "error_code": "denoise_seed_failed",
                    "message": f"Cloud denoise model installation failed: {exc}"[:512]}
    return run_seed(WEIGHTS_MOUNT, store, commit=weights_volume.commit,
                    model_id=SETTINGS.model_id)


@app.cls(
    image=analysis_gpu_image,
    gpu=SETTINGS.gpu,
    volumes={WEIGHTS_MOUNT: weights_volume},
    cpu=2.0,
    memory=24576 if ANALYSIS_SAM in SETTINGS.analysis_models or SETTINGS.denoise else 12288,
    timeout=ANALYSIS_TIMEOUT_SECONDS,
    startup_timeout=ANALYSIS_TIMEOUT_SECONDS,
    scaledown_window=SETTINGS.idle_seconds,
    max_containers=1,
    **ANALYSIS_SNAPSHOT,
)
@modal.concurrent(max_inputs=ANALYSIS_MAX_INPUTS)
class AnalysisGPU:
    @modal.enter(snap=True)
    def prepare(self) -> None:
        # No GPU is attached here (CPU memory snapshot only): imports and the runtime
        # object, nothing that opens a CUDA context. Sessions load on the first tile.
        from deploy.cloud.common.analysis_runtime import OnDemandAnalysis, import_runtime_modules
        import_runtime_modules()
        self.runtime = OnDemandAnalysis(WEIGHTS_MOUNT, SETTINGS.analysis_models,
                                        reload=weights_volume.reload)
        # Page denoise shares this GPU role: small ONNX and PyTorch models, one page
        # per call, the same heartbeat, stop and admission as analysis tiles.
        from deploy.cloud.common.denoise import DenoiseRuntime
        self.denoise_runtime = DenoiseRuntime(WEIGHTS_MOUNT, reload=weights_volume.reload)

    @modal.enter(snap=False)
    def start(self) -> None:
        # Every start, restores included: a fresh heartbeat (container id, started_at)
        # and the release marker. A container started only to deliver a release just
        # stays out of the status.
        self.heartbeat = _heartbeat("analysis", ANALYSIS_TIMEOUT_SECONDS, ANALYSIS_TIMEOUT_SECONDS)
        start_gpu_container(self.heartbeat)

    # Inputs run on threads of this container; the heartbeat says idle only once the
    # last one has ended.
    @modal.method()
    def analyze(self, metadata: Dict[str, Any], tile_png: bytes) -> Dict[str, Any]:
        with self.heartbeat.serving():
            return self.runtime.analyze(metadata, tile_png)

    # A batch (contract.py `validate_analysis_batch`): one page's requests in one
    # call, answered in request order.
    @modal.method()
    def analyze_batch(self, metadata: Dict[str, Any], pairs: list) -> Dict[str, Any]:
        with self.heartbeat.serving():
            return {"protocol_version": metadata["protocol_version"], "request_digest": metadata["request_digest"],
                    "results": [self.runtime.analyze(request, png) for request, png in pairs]}

    @modal.method()
    def denoise(self, metadata: Dict[str, Any], page_png: bytes) -> Dict[str, Any]:
        with self.heartbeat.serving():
            return self.denoise_runtime.denoise(metadata, page_png)

    @modal.method()
    def release(self) -> Dict[str, Any]:
        return release_container(self.heartbeat, modal.experimental.stop_fetching_inputs)

    @modal.exit()
    def unload(self) -> None:
        self.heartbeat.clear()


# Volume.reload fails while any file of the volume is open in this container
# ("there are open files preventing the operation"), and the graph check after it
# reads the graphs. The gateway serves requests on threads, so every reload and the
# reads that follow it take this lock.
_volume_reload_lock = threading.Lock()


def require_render_weights() -> None:
    """Reject an outdated/incomplete installation before starting any GPU."""
    from deploy.cloud.common.jobs import PreEnqueueError
    from deploy.cloud.common.weights import WeightsError, require_snapshot
    try:
        with _volume_reload_lock:
            weights_volume.reload()
            require_snapshot(WEIGHTS_MOUNT, model_id=SETTINGS.model_id)
    except WeightsError as error:
        raise PreEnqueueError(error.error_code, str(error), retryable=False) from error
    except Exception as error:
        raise PreEnqueueError("service_unavailable_pre_queue", "Could not check installed weights") from error


def _analysis_job_kind(handle: str) -> str:
    return "analysis_batch" if handle.startswith("analysis_batch-") else "analysis"


class ModalDenoiseProxy:
    def __init__(self, gpu_control: GpuControl, jobs: ModalGpuJobs) -> None:
        self._gpu_control = gpu_control
        self._jobs = jobs

    def capabilities(self) -> Dict[str, Any]:
        from deploy.cloud.common.denoise import denoise_capabilities
        with _volume_reload_lock:
            weights_volume.reload()
            return denoise_capabilities(WEIGHTS_MOUNT)

    def denoise(self, metadata: Dict[str, Any], page_png: bytes) -> Dict[str, Any]:
        # The synchronous route, for an app without the job routes. Tracked like an
        # analysis tile, so a GPU stop cancels a page in flight.
        return run_tracked_analysis(AnalysisGPU().denoise.spawn, ModalDictStore(jobs_dict), metadata, page_png,
                                    admission=self._gpu_control.analysis_admission)

    # Submit-then-poll (/mc/denoise/v1/jobs): no request waits on the GPU.
    def submit_job(self, metadata: Dict[str, Any], page_png: bytes, attempt_id: str) -> Dict[str, Any]:
        return self._jobs.submit("denoise", AnalysisGPU().denoise.spawn, metadata, page_png, attempt_id,
                                 admission=self._gpu_control.analysis_admission)

    def job_status(self, handle: str) -> Dict[str, Any] | None:
        return self._jobs.status("denoise", handle, denoise_failure)

    def cancel_job(self, handle: str) -> Dict[str, Any] | None:
        return self._jobs.cancel("denoise", handle)


class ModalAnalysisProxy:
    def __init__(self, gpu_control: GpuControl, jobs: ModalGpuJobs) -> None:
        self._verified_graphs = VerifiedGraphCache()
        self._gpu_control = gpu_control
        self._jobs = jobs

    def capabilities(self) -> Dict[str, Any]:
        with _volume_reload_lock:
            return analysis_capabilities(WEIGHTS_MOUNT, SETTINGS.analysis_models, weights_volume.reload,
                                         cache=self._verified_graphs)

    def _require_installed(self, *requests: Dict[str, Any]) -> None:
        installed = {item["capability"] for item in self.capabilities()["capabilities"]}
        if any(request.get("capability") not in installed for request in requests):
            raise AnalysisUnavailable("Selected analysis graph is not installed")

    def analyze(self, metadata: Dict[str, Any], tile_png: bytes) -> Dict[str, Any]:
        self._require_installed(metadata)
        # The synchronous route, for an app without the job routes. Spawned and
        # awaited rather than `.remote`, so a GPU stop can cancel the tile by the call
        # id this records.
        return run_tracked_analysis(AnalysisGPU().analyze.spawn, ModalDictStore(jobs_dict), metadata, tile_png,
                                    admission=self._gpu_control.analysis_admission)

    # Submit-then-poll (/mc/analysis/v1/jobs): no request waits on the GPU.
    def submit_job(self, metadata: Dict[str, Any], tile_png: bytes, attempt_id: str) -> Dict[str, Any]:
        self._require_installed(metadata)
        return self._jobs.submit("analysis", AnalysisGPU().analyze.spawn, metadata, tile_png, attempt_id,
                                 admission=self._gpu_control.analysis_admission)

    def submit_batch_job(self, metadata: Dict[str, Any], pairs: list, attempt_id: str) -> Dict[str, Any]:
        self._require_installed(*(request for request, _ in pairs))
        return self._jobs.submit("analysis_batch", AnalysisGPU().analyze_batch.spawn, metadata, pairs, attempt_id,
                                 admission=self._gpu_control.analysis_admission)

    # Tile and batch jobs share these routes' worker; the gateway has checked the
    # handle against the route's kind, and its prefix names that kind.
    def job_status(self, handle: str) -> Dict[str, Any] | None:
        return self._jobs.status(_analysis_job_kind(handle), handle, analysis_failure)

    def cancel_job(self, handle: str) -> Dict[str, Any] | None:
        return self._jobs.cancel(_analysis_job_kind(handle), handle)


@app.cls(
    image=gpu_image,
    gpu=SETTINGS.gpu,
    volumes={WEIGHTS_MOUNT: weights_volume},
    cpu=2.0,
    memory=render_worker_memory_gib(production_model(SETTINGS.model_id), SETTINGS.gpu, "modal") * 1024,
    timeout=JOB_TIMEOUT_SECONDS,
    startup_timeout=WORKER_STARTUP_TIMEOUT_SECONDS,
    scaledown_window=SETTINGS.idle_seconds,
    max_containers=1,
    **WORKER_SNAPSHOT,
)
@modal.concurrent(max_inputs=RENDER_COPIES)
class Worker:
    # Modal refuses snap=True on a class without memory snapshots, so the phase follows
    # WORKER_SNAPSHOT. With both enters snap=False, Modal runs them in definition order.
    @modal.enter(snap=WORKER_SNAPSHOT["enable_memory_snapshot"])
    def load(self) -> None:
        # Runs on every start while WORKER_SNAPSHOT is off (with it on: once per
        # snapshot, never after a restore). Loads and warms the pipeline on the GPU or
        # raises WorkerNotReady (weights not seeded, load failed, started by a stop's
        # release), so a container never serves without a loaded pipeline and no
        # snapshot could hold an unloaded one (see prepare_render_container).
        self.runtime = prepare_render_container(
            ModalDictStore(jobs_dict),
            lambda: _heartbeat("render", WORKER_STARTUP_TIMEOUT_SECONDS, JOB_TIMEOUT_SECONDS),
            lambda: WorkerRuntime(WEIGHTS_MOUNT, reload=weights_volume.reload, marker_attempts=2,
                                  model_id=SETTINGS.model_id, copies=RENDER_COPIES),
        )

    @modal.enter(snap=False)
    def start(self) -> None:
        # Every start, restores included: a fresh heartbeat (container id, started_at)
        # and the release marker. The pipeline is already loaded here.
        self.heartbeat = _heartbeat("render", WORKER_STARTUP_TIMEOUT_SECONDS, JOB_TIMEOUT_SECONDS)
        start_gpu_container(self.heartbeat, self.runtime)

    @modal.method()
    def render(self, handle: str, metadata_json: str, image_png: bytes, hint_png: bytes) -> Dict[str, Any]:
        # Up to RENDER_COPIES renders run at once, each on a pipeline copy of its own.
        with self.heartbeat.serving():
            return worker_render(self.runtime, ModalDictStore(jobs_dict), handle, metadata_json, image_png, hint_png)

    @modal.method()
    def release(self) -> Dict[str, Any]:
        return release_container(self.heartbeat, modal.experimental.stop_fetching_inputs)

    @modal.exit()
    def unload(self) -> None:
        self.heartbeat.clear()


# The default region is deployed without either argument, exactly as before the
# choice existed. Any other region names its own Function (settings.gateway_function):
# Modal does not let a deployed Function change its routing region. Only the gateway
# is routed: a Function outside us-east cannot be started with `.spawn`, which is how
# the gateway starts the GPU classes.
GATEWAY_ROUTING: Dict[str, Any] = {} if SETTINGS.routing_region == DEFAULT_ROUTING_REGION else {
    "name": SETTINGS.gateway_function,
    "routing_region": SETTINGS.routing_region,
}


@app.function(
    image=gateway_image,
    volumes={WEIGHTS_MOUNT: weights_volume},
    cpu=0.25,
    memory=512,
    # Per request. Tiles and pages are submitted and polled, so no request of the
    # current app waits on a GPU; this only has to outlast the older synchronous
    # routes' bounded wait (common/capacity.py SYNC_GPU_WAIT_SECONDS).
    timeout=GATEWAY_TIMEOUT_SECONDS,
    max_containers=1,
    **GATEWAY_ROUTING,
)
@modal.concurrent(max_inputs=32)
@modal.asgi_app(requires_proxy_auth=True)
def gateway():
    from deploy.cloud.common.api import CloudGateway
    from deploy.cloud.common.jobs import DispatchJobBackend

    limits = production_limits()
    store = ModalDictStore(jobs_dict)
    dispatcher = ModalWorkerDispatcher(
        spawn=Worker().render.spawn,
        function_call_from_id=modal.FunctionCall.from_id,
        exceptions=modal.exception,
        store=store,
    )
    backend = DispatchJobBackend("modal", store, dispatcher, limits, clock=time.time, preflight=require_render_weights)
    gpu_control = GpuControl(
        provider="modal",
        store=store,
        gpu=SETTINGS.gpu,
        backend=backend,
        release={"render": lambda: Worker().release.spawn(), "analysis": lambda: AnalysisGPU().release.spawn()},
        cancel_analysis=lambda: cancel_tracked_analysis(store, modal.FunctionCall.from_id),
        analysis_in_flight=lambda: count_tracked_analysis(store),
        list_prices=GPU_LIST_PRICE_USD_PER_HOUR,
    )
    # One job table for tiles and pages, in the same Dict as render jobs.
    gpu_jobs = ModalGpuJobs(store, modal.FunctionCall.from_id, modal.exception, clock=time.time)
    gateway_app = CloudGateway(
        provider="modal",
        model_info=get_production_model_info("modal", SETTINGS.model_id),
        limits=limits,
        backend=backend,
        analysis_worker=ModalAnalysisProxy(gpu_control, gpu_jobs),
        trust_edge_auth=True,
        gpu_control=gpu_control,
        denoise_worker=ModalDenoiseProxy(gpu_control, gpu_jobs) if SETTINGS.denoise else None,
        # The shipped sources as the container holds them: the code this deployment runs.
        code_digest=code_digest(DEPLOY_ROOT),
    )
    return gateway_app.as_asgi_app()
