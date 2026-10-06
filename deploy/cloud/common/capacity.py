"""How much work one GPU container takes at once, from a memory budget.

Every number that decides per-container concurrency lives here, so the Modal app,
the Beam app and the provisioner's quote agree:

- `render_copies(model, gpu)`: how many copies of the render pipeline one container
  holds. diffusers pipelines are not thread safe, so each concurrent render borrows a
  copy of its own (common/worker.py RunnerPool). A copy fits when the measured peak
  VRAM of one copy (`ProductionModel.render_peak_vram_mib`, CUDA context included)
  times the copies stays under the GPU's memory less `VRAM_HEADROOM`. A model with
  no measured peak gets one copy. A model that names `multi_copy_gpus` (the heavy
  ones: FLUX 9B, Qwen) runs one copy everywhere else and `MAX_RENDER_COPIES` on those
  GPUs, by decision rather than budget (see `render_copies`).
- `render_worker_memory_gib(model, gpu, provider)`: host memory the render container
  reserves for those copies. Modal loads the copies one after another in one process,
  so each extra copy adds its measured resident size; Beam runs each copy as its own
  worker process (`workers=`), all loading at once, so each adds a whole worker.
- `ANALYSIS_MAX_INPUTS`: concurrent inputs of the analysis GPU (ONNX detection tiles
  and page denoise), which share their sessions across threads.
- `GATEWAY_TIMEOUT_SECONDS`: the Modal gateway's per-request limit. Analysis tiles and
  denoise pages go through submit-then-poll job routes (common/contract.py
  `GPU_JOB_OPERATIONS`) whose every request returns in seconds, so it only has to
  outlast `SYNC_GPU_WAIT_SECONDS`, the bounded wait of the older synchronous routes.
"""

from __future__ import annotations

from typing import Any, Mapping, Optional

# Total memory each allowed GPU reports (nvidia-smi "MiB"), per provider name.
# L4: measured live (docs/cloud-work-log.md, 25 September 2026). The others are the
# totals those cards report for their nominal size, not measured on this deployment.
GPU_MEMORY_MIB: Mapping[str, int] = {
    "L4": 23034,
    "A10": 23028,
    "A10G": 23028,
    "L40S": 46068,
    "RTX4090": 24564,
    "RTX5090": 32607,
}

# Share of the GPU kept free of every budget: allocator fragmentation, cuBLAS and
# cuDNN workspaces, and crops larger than the one the peak was measured on.
VRAM_HEADROOM = 0.15

# Never more copies than this, whatever fits. Copies on one GPU share its compute, so
# beyond two the gain is overlap of the CPU-side work only and each copy still costs
# host memory and load time on every cold start.
MAX_RENDER_COPIES = 2

# A render whose working size is above this many pixels runs alone, holding every
# copy. The only measured peak is at 768x576 (0.44 MP); activations and the VAE decode
# grow with the working size, and two 4 MP crops at once have never been measured.
EXCLUSIVE_WORK_PIXELS = 1024 * 1024

# Analysis GPU (Modal AnalysisGPU, Beam analysis queue). The desktop keeps two denoise
# pages in flight and sends detection tiles one after another, so three inputs lets
# both pages and a tile run without queueing. More would only contend for the
# container's 2 CPU cores, which run the denoise tiling, blending, Lanczos and PNG
# steps, and would multiply the host memory of large 4x pages.
ANALYSIS_MAX_INPUTS = 3
# Beam runs analysis workers as processes, each loading its own sessions.
BEAM_ANALYSIS_WORKERS = 2

# Modal answers a web request still running after this long with a 303 redirect to
# a result URL. The desktop follows no redirect (src-tauri inference/http.rs), so no
# request may need this long: tiles and pages are submitted and then polled.
MODAL_WEB_REDIRECT_SECONDS = 150
# The synchronous `/mc/analysis/v1/analyze` and `/mc/denoise/v1/page` routes stay for
# an app that predates the job routes. They wait on their GPU call this long, then
# cancel it and answer 504, under the redirect: past it that app could never read the
# answer, and the GPU would bill a page nobody collects.
SYNC_GPU_WAIT_SECONDS = 120
# Per request. Outlasts the synchronous wait plus reading a large upload; nothing
# the current app sends comes near it.
GATEWAY_TIMEOUT_SECONDS = 300


def gpu_memory_mib(gpu: str) -> Optional[int]:
    return GPU_MEMORY_MIB.get(str(gpu).upper()) or GPU_MEMORY_MIB.get(str(gpu))


def copies_that_fit(peak_mib: Optional[int], gpu: str, cap: int = MAX_RENDER_COPIES,
                    headroom: float = VRAM_HEADROOM) -> int:
    """Copies of a model with `peak_mib` peak VRAM that fit one `gpu`, at least 1."""
    total = gpu_memory_mib(gpu)
    if not peak_mib or peak_mib <= 0 or total is None:
        return 1
    usable = total * (1.0 - headroom)
    return max(1, min(int(cap), int(usable // peak_mib)))


def render_copies(model: Any, gpu: str) -> int:
    """Render pipeline copies one container of `model` holds on `gpu`.

    A model with no `multi_copy_gpus` (FLUX 4B) takes whatever the budget fits. One
    that names them (FLUX 9B, Qwen) is held to one copy on any other GPU and runs
    `MAX_RENDER_COPIES` on those, by the owner's decision (2026-10-01) and not by the
    budget: Qwen's two copies are about 43 of the L40S's 45 GiB, over the headroom,
    and FLUX 9B's peak is not measured. What catches a miss is the worker, not this
    rule: an extra copy that fails to load leaves the container serving with one
    (`WorkerRuntime`), and a render that runs out of memory beside another is retried
    alone.
    """
    allowed = tuple(str(name).upper() for name in getattr(model, "multi_copy_gpus", ()) or ())
    if allowed:
        return max(1, int(MAX_RENDER_COPIES)) if str(gpu).upper() in allowed else 1
    return copies_that_fit(getattr(model, "render_peak_vram_mib", None), gpu)


def render_worker_memory_gib(model: Any, gpu: str, provider: str) -> int:
    """Host memory (GiB) of the render container that holds `render_copies` copies."""
    copies = render_copies(model, gpu)
    base = int(model.worker_memory_gib)
    if copies <= 1:
        return base
    if provider == "beam":
        return base * copies
    extra = getattr(model, "render_copy_host_gib", None)
    return base + int(extra if extra else base) * (copies - 1)


def exclusive_render(work_width: int, work_height: int) -> bool:
    """Whether a render at this working size must run with no other render beside it."""
    return int(work_width) * int(work_height) > EXCLUSIVE_WORK_PIXELS
