"""Beam app: CPU gateway, GPU worker and weights seeding.

The provisioner copies this file to the root of a clean staging directory as
mc_beam_app.py (see stage.py), next to the subset of the deploy package it needs,
exports the MC_BEAM_* variables (see settings.py), makes that directory the working
directory and deploys the three handlers below. Beam uploads the directory and
imports this module in each container. Every deployment carries the same variables,
so a container rebuilds exactly the objects that were deployed.

- `gateway` serves /mc/v1 on a small CPU container behind Beam's token check
  (authorized=True). It keeps job documents in a beam.Map, so health and model-info
  never start a GPU.
- `render` is the GPU task queue. `load_worker` loads the pinned pipeline from the
  weights volume once per worker process; each job runs once (retries=0) within the
  job timeout, and the container scales to zero after the idle window. The one
  container runs `RENDER_WORKERS` worker processes on its GPU, each with its own
  pipeline, so that many jobs render at once (common/capacity.py sizes it from the
  GPU's memory). `analyze` likewise runs `BEAM_ANALYSIS_WORKERS` processes.
- `seed` downloads the pinned snapshot into the weights volume on a CPU container.
  GPU containers never download.
"""

from __future__ import annotations

import os
import time
from pathlib import Path
from typing import Any, Dict

import beam

from deploy.cloud.beam.backend import (
    BeamMapStore,
    BeamWorkerDispatcher,
    beam_worker_render,
    make_http_enqueue,
)
from deploy.cloud.beam.settings import WEIGHTS_MOUNT, BeamSettings, resolve_weights_root
from deploy.cloud.common.capacity import BEAM_ANALYSIS_WORKERS, render_copies, render_worker_memory_gib
from deploy.cloud.common.deployment import JOB_TIMEOUT_SECONDS, SEED_TIMEOUT_SECONDS
from deploy.cloud.common.manifest import get_production_model_info, production_limits, production_model
from deploy.cloud.common.weights import run_seed
from deploy.cloud.common.analysis_seed import ANALYSIS_SAM, run_analysis_seed
from deploy.cloud.beam.analysis_backend import BeamAnalysisProxy, beam_analysis_task
from deploy.cloud.common.worker import WorkerRuntime

SETTINGS = BeamSettings.from_env()
DEPLOY_ENV = SETTINGS.to_env()

PYTHON_VERSION = "python3.12"
# torch comes from the CUDA 12.9 wheel index before the rest is installed, so pip
# never swaps in a different torch build from PyPI.
TORCH_INSTALL = (
    "python3.12 -m pip install --no-cache-dir torch==2.13.0 --index-url https://download.pytorch.org/whl/cu129"
)
# torchvision, from the same index: Qwen-Image-Edit's Qwen2-VL processor refuses to load without it.
GPU_TORCH_INSTALL = (
    "python3.12 -m pip install --no-cache-dir torch==2.13.0 torchvision==0.28.0"
    " --index-url https://download.pytorch.org/whl/cu129"
)
GPU_REQUIREMENTS = [
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
]
SEED_REQUIREMENTS = ["huggingface-hub==1.24.0"]
SAM_EXPORT_REQUIREMENTS = [
    "onnx==1.19.0", "onnx-ir==1.0.0", "onnxscript==0.5.6",
    "numpy==2.3.3", "pillow==11.3.0", "safetensors==0.6.2",
    "requests==2.34.2", "einops==0.8.1", "pyyaml==6.0.3",
]
ANALYSIS_SOURCE = str(Path(__file__).resolve().parent / "analysis-source")

# Beam volume writes can take up to a minute to reach another container, so the
# worker looks for the seeded marker a few times before it reports weights_missing.
MARKER_ATTEMPTS = 4
MARKER_DELAY_SECONDS = 30.0
GATEWAY_KEEP_WARM_SECONDS = 120
GATEWAY_TIMEOUT_SECONDS = 240

weights_volume = beam.Volume(name=SETTINGS.volume_name, mount_path=WEIGHTS_MOUNT)
# Render worker processes in the one GPU container: `workers=N` runs N processes that
# share the container's GPU, CPU and memory, so each process is one pipeline copy.
RENDER_WORKERS = render_copies(production_model(SETTINGS.model_id), SETTINGS.gpu)

gpu_image = (
    beam.Image(python_version=PYTHON_VERSION)
    .add_commands([GPU_TORCH_INSTALL])
    .add_python_packages(GPU_REQUIREMENTS)
    .with_envs({"HF_HUB_OFFLINE": "1", "HF_HUB_DISABLE_TELEMETRY": "1"})
)
seed_image = (
    beam.Image(python_version=PYTHON_VERSION)
    .add_python_packages(SEED_REQUIREMENTS)
    .with_envs({"HF_HUB_DISABLE_TELEMETRY": "1", "HF_HUB_DISABLE_PROGRESS_BARS": "1"})
)
if ANALYSIS_SAM in SETTINGS.analysis_models:
    seed_image = (seed_image.add_commands([TORCH_INSTALL])
                  .add_python_packages(SAM_EXPORT_REQUIREMENTS))
analysis_gpu_image = (
    beam.Image(python_version=PYTHON_VERSION)
    .add_commands([TORCH_INSTALL])
    .add_python_packages(["onnxruntime-gpu==1.23.2", "numpy==2.4.6", "pillow==12.3.0"])
    .with_envs({"HF_HUB_OFFLINE": "1", "HF_HUB_DISABLE_TELEMETRY": "1"})
)
gateway_image = beam.Image(python_version=PYTHON_VERSION)


def _store() -> BeamMapStore:
    return BeamMapStore(beam.Map(name=SETTINGS.map_name))


def _workspace_token() -> str:
    # The provisioner stores the workspace token in this secret; Beam injects it as
    # an environment variable of the same name.
    return os.environ.get(SETTINGS.secret_name, "")


def _stop_task(task_id: str) -> None:
    from beta9.channel import Channel
    from beta9.clients.gateway import GatewayServiceStub, StopTasksRequest

    channel = Channel(addr=f"{SETTINGS.gateway_host}:{SETTINGS.gateway_port}", token=_workspace_token())
    try:
        response = GatewayServiceStub(channel).stop_tasks(StopTasksRequest(task_ids=[task_id]))
    finally:
        channel.close()
    if not response.ok:
        raise RuntimeError(response.err_msg or "Beam did not stop the task")


def load_worker() -> WorkerRuntime:
    # A missing snapshot is reported per job, not raised here: the container stays up
    # and loads on its next job once the volume is seeded.
    runtime = WorkerRuntime(
        str(resolve_weights_root(SETTINGS.volume_name)),
        marker_attempts=MARKER_ATTEMPTS,
        marker_delay_seconds=MARKER_DELAY_SECONDS,
        model_id=SETTINGS.model_id,
    )
    runtime.load()
    return runtime


@beam.task_queue(
    name=SETTINGS.seed_name,
    image=seed_image,
    cpu=2.0,
    memory="24Gi" if ANALYSIS_SAM in SETTINGS.analysis_models else "4Gi",
    volumes=[weights_volume],
    env=DEPLOY_ENV,
    timeout=SEED_TIMEOUT_SECONDS * (2 if ANALYSIS_SAM in SETTINGS.analysis_models else 1),
    retries=0,
    workers=1,
    keep_warm_seconds=10,
    max_pending_tasks=4,
    autoscaler=beam.QueueDepthAutoscaler(max_containers=1),
    authorized=True,
)
def seed(context: Any = None) -> Dict[str, Any]:
    # Idempotent: a seeded volume returns at once and a partial download resumes.
    # Progress and the final state go to the Map, where the provisioner reads them.
    root = str(resolve_weights_root(SETTINGS.volume_name))
    store = _store()
    analysis = run_analysis_seed(root, SETTINGS.analysis_models, ANALYSIS_SOURCE, store,
                                 model_id=SETTINGS.model_id)
    if analysis["status"] == "failed":
        return analysis
    return run_seed(root, store, model_id=SETTINGS.model_id)


def load_analysis_worker():
    from deploy.cloud.common.analysis_runtime import OnDemandAnalysis
    return OnDemandAnalysis(str(resolve_weights_root(SETTINGS.volume_name)), SETTINGS.analysis_models)


@beam.task_queue(
    name=SETTINGS.analysis_name,
    image=analysis_gpu_image,
    gpu=SETTINGS.gpu,
    cpu=2.0,
    memory="24Gi" if ANALYSIS_SAM in SETTINGS.analysis_models else "12Gi",
    volumes=[weights_volume],
    env=DEPLOY_ENV,
    timeout=180,
    retries=0,
    workers=BEAM_ANALYSIS_WORKERS,
    keep_warm_seconds=SETTINGS.idle_seconds,
    max_pending_tasks=8,
    on_start=load_analysis_worker,
    autoscaler=beam.QueueDepthAutoscaler(max_containers=1, tasks_per_container=BEAM_ANALYSIS_WORKERS),
    authorized=True,
)
def analyze(handle: str = "", context: Any = None) -> Dict[str, Any]:
    runtime = getattr(context, "on_start_value", None) or load_analysis_worker()
    return beam_analysis_task(runtime, _store(), handle)


@beam.task_queue(
    name=SETTINGS.worker_name,
    image=gpu_image,
    gpu=SETTINGS.gpu,
    cpu=1.0,
    memory=f"{render_worker_memory_gib(production_model(SETTINGS.model_id), SETTINGS.gpu, 'beam')}Gi",
    volumes=[weights_volume],
    env=DEPLOY_ENV,
    timeout=JOB_TIMEOUT_SECONDS,
    retries=0,
    workers=RENDER_WORKERS,
    keep_warm_seconds=SETTINGS.idle_seconds,
    max_pending_tasks=16,
    on_start=load_worker,
    autoscaler=beam.QueueDepthAutoscaler(max_containers=1, tasks_per_container=RENDER_WORKERS),
    authorized=True,
)
def render(handle: str = "", context: Any = None) -> Dict[str, Any]:
    runtime = getattr(context, "on_start_value", None) or load_worker()
    return beam_worker_render(runtime, _store(), handle)


@beam.asgi(
    name=SETTINGS.gateway_name,
    image=gateway_image,
    cpu=0.5,
    memory="1Gi",
    env=DEPLOY_ENV,
    secrets=[SETTINGS.secret_name],
    volumes=[weights_volume],
    workers=1,
    concurrent_requests=32,
    keep_warm_seconds=GATEWAY_KEEP_WARM_SECONDS,
    timeout=GATEWAY_TIMEOUT_SECONDS,
    autoscaler=beam.QueueDepthAutoscaler(max_containers=1),
    authorized=True,
)
def gateway(context: Any = None):
    # Starlette ships with Beam's container runtime, which needs an app it can add
    # middleware and a lifespan to.
    from deploy.cloud.beam.routes import mount_gateway_app
    from deploy.cloud.common.api import CloudGateway
    from deploy.cloud.common.jobs import DispatchJobBackend

    limits = production_limits()
    store = _store()
    dispatcher = BeamWorkerDispatcher(
        store,
        enqueue=make_http_enqueue(SETTINGS.worker_url, _workspace_token),
        stop_task=_stop_task,
    )
    gateway_app = CloudGateway(
        provider="beam",
        model_info=get_production_model_info("beam", SETTINGS.model_id),
        limits=limits,
        backend=DispatchJobBackend("beam", store, dispatcher, limits, clock=time.time),
        analysis_worker=BeamAnalysisProxy(
            str(resolve_weights_root(SETTINGS.volume_name)), SETTINGS.analysis_models,
            store, make_http_enqueue(SETTINGS.analysis_url, _workspace_token),
        ),
        trust_edge_auth=True,
    )
    return mount_gateway_app(gateway_app.as_asgi_app())
