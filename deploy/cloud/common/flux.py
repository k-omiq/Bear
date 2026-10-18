"""FLUX worker: the pinned recipe, run the way the local sdnq backend runs it.

`SdnqFluxRunner` is the production path. It shares the local sidecar's working size,
hole and output conversion (sidecar/manga_cleaner_sidecar/backend/sdnq.py). Both render
through FLUX.2 Klein's inpaint pipeline: the hint, grown by `HOLE_GROWTH`, is the hole,
and the latents outside it are held to the crop's own at every step. Since recipe v2
the cloud runner first fills the hole with the app's manga LaMa (`LamaFill`) and gives
Klein that filled crop as its only image. The pipeline conditions on the image it is
given, so with the lettering still in it Klein drew bold sound effects back
(docs/findings.md, "Cloud Klein v2"). The local sidecar does not fill yet. A request
may carry guidance (`flux_prompt`). torch, diffusers, sdnq, onnxruntime, PIL and numpy
are imported when a runner loads, never at module import, so the CPU gateway image
does not need them.

`render_job` is what a GPU worker runs per job: validate, render, validate the result.
`FluxWorker` is the offline fake the gateway tests drive through the local backend.
"""

from __future__ import annotations

import hashlib
import inspect
import io
import struct
import time
import zlib
from dataclasses import dataclass
from enum import Enum
from types import SimpleNamespace
from typing import Any, Dict, Optional, Tuple

from deploy.cloud.common.contract import (
    LATENT_STRIDE,
    ContractValidationError,
    JobRequestMetadata,
    QwenEdit,
    ServiceLimits,
    provisional_fixture_limits,
    validate_crop_payload,
    validate_worker_result,
)
from deploy.cloud.common.handle_mapping import JobRecord
from deploy.cloud.common.manifest import (
    ADAPTER_DIR,
    LAMA_FILE,
    RECIPE_PROMPT,
    WORK_LONG_SIDE,
    get_pinned_sampling,
    is_recipe_supported,
)


# ---------------------------------------------------------------------------
# Recipe arithmetic, shared with the local backend and free of heavy imports.
# ---------------------------------------------------------------------------


def snap_stride(value: int) -> int:
    """Largest multiple of LATENT_STRIDE not exceeding `value` (never below one stride)."""
    value = int(value)
    return max(LATENT_STRIDE, value - (value % LATENT_STRIDE))


def working_size(width: int, height: int) -> Tuple[int, int]:
    """Resolution a crop is edited at: upscale only, snapped down to the latent stride."""
    long_side = max(int(width), int(height))
    if long_side >= WORK_LONG_SIDE:
        return int(width), int(height)
    scale = WORK_LONG_SIDE / long_side
    return snap_stride(round(width * scale)), snap_stride(round(height * scale))


def guidance_from_scaled(guidance_scaled: int) -> float:
    """Wire guidance is sent times 100 (1.0 travels as 100)."""
    return int(guidance_scaled) / 100.0


def filter_call_kwargs(pipeline: Any, candidate: Dict[str, Any]) -> Dict[str, Any]:
    """`candidate` less anything the pipeline's `__call__` does not take.

    Same rule as the local backend: an argument a diffusers upgrade removes is
    dropped instead of raising, and one it adds is passed as soon as it exists.
    """
    try:
        accepted = inspect.signature(type(pipeline).__call__).parameters
    except (TypeError, ValueError):
        return dict(candidate)
    if any(p.kind == inspect.Parameter.VAR_KEYWORD for p in accepted.values()):
        return dict(candidate)
    return {name: value for name, value in candidate.items() if name in accepted}


# What each guidance target asks Klein to do. Measured on two real pages (docs/findings.md,
# "Cloud Klein v2"): the target alone never hurt, while a description is drawn as said,
# so it describes what lies under the lettering.
FLUX_ACTIONS: Dict[str, str] = {
    "auto": "Remove all text, including hand-drawn Japanese sound effects and onomatopoeia.",
    "dialogue": "Remove the dialogue lettering inside the speech bubble and leave the bubble interior blank.",
    "sound_effect": "Remove the hand-drawn sound effect lettering and redraw the artwork it covered.",
    "other": "Remove the unwanted lettering and redraw what it covered.",
}


def flux_prompt(edit: Optional[QwenEdit]) -> str:
    """The prompt for one render: the recipe's own, unless the request carries guidance."""
    if edit is None or (edit.target == "auto" and not edit.description.strip()):
        return RECIPE_PROMPT
    behind = f" Behind the lettering: {edit.description.strip()}." if edit.description.strip() else ""
    return (FLUX_ACTIONS[edit.target] + behind
            + " Preserve character line art, screentones, panel borders, and background"
            + " details exactly as they appear. Match the original contrast, line weight and shading."
            + " Add no new text or symbols.")


def recipe_mismatch(meta: JobRequestMetadata) -> Optional[str]:
    """Why a request is not the pinned render, or None when it is."""
    if not is_recipe_supported(meta.recipe):
        return f"Recipe '{meta.recipe.recipe_id}' is not served by this deployment"
    return sampling_mismatch(meta)


def sampling_mismatch(meta: JobRequestMetadata) -> Optional[str]:
    """Why a request's seed, steps or guidance differ from what its recipe pins.

    Steps and guidance are fixed. The pinned seed is the one a first request for a
    region carries. A retry of the same region carries a later seed, so that it is
    a new render and not the same image again; a seed below the pinned one is no
    request the app makes.
    """
    sampling = get_pinned_sampling(meta.recipe.recipe_id)
    if sampling is None:
        return None
    if meta.seed < sampling.seed or (meta.steps, meta.guidance_scaled) != (
        sampling.steps,
        sampling.guidance_scaled,
    ):
        return (
            f"Recipe '{meta.recipe.recipe_id}' is pinned to seed>={sampling.seed}, "
            f"steps={sampling.steps}, guidance_scaled={sampling.guidance_scaled}"
        )
    return None


# ---------------------------------------------------------------------------
# Production runner
# ---------------------------------------------------------------------------


class WorkerRenderError(RuntimeError):
    """Typed render failure; `error_code` is a stable snake_case code for the job status."""

    def __init__(self, error_code: str, message: str):
        super().__init__(message)
        self.error_code = error_code


# Crop side of the warmup render; working_size() scales it up to WORK_LONG_SIDE.
WARMUP_SIDE = 64

# How far the hole reaches past the hint, in crop pixels: the client writes the answer
# up to ISOLATION_RADIUS (5 px, crates/cleaner-core/src/constants.rs) from the
# lettering, and 3 px more give the model the lettering's antialiased edge to redraw.
# The local sidecar's HOLE_GROWTH (backend/base.py); test_recipe_parity.py holds them equal.
HOLE_GROWTH = 8


def looks_like_memory(detail: str) -> bool:
    return "out of memory" in detail.lower()


# lama-manga.onnx takes a fixed 512 x 512 input (crates/cleaner-core/src/engines/lama.rs).
LAMA_SIDE = 512
# Tiles of a larger crop overlap by 128 px, as rung 2's do.
LAMA_STEP = LAMA_SIDE - 128


class LamaFill:
    """The hole filled by the app's manga LaMa, at the crop's own resolution.

    The graph's contract is rung 2's: RGB in 0..1, mask 1 = hole, fixed 512 x 512. A
    crop smaller than that is padded by edge replication; a larger one runs in 512 tiles
    128 px apart, each filling what is still a hole, so a later tile reads the fill of
    an earlier one. Only hole pixels change. ONNX Runtime must run it on CUDA in a GPU
    worker: a CPU fallback would hide a broken image.
    """

    def __init__(self, path: str, device: str):
        import numpy
        import onnxruntime
        import torch  # noqa: F401  loads the CUDA libraries ONNX Runtime links against
        from PIL import Image

        self.np, self.Image = numpy, Image
        providers = ["CUDAExecutionProvider"] if device == "cuda" else ["CPUExecutionProvider"]
        self.session = onnxruntime.InferenceSession(path, providers=providers)
        if providers[0] not in self.session.get_providers():
            raise RuntimeError(f"LaMa did not start on {providers[0]}")

    def fill(self, crop: Any, hole: Any) -> Any:
        np = self.np
        image = np.asarray(crop.convert("RGB"), dtype=np.float32) / 255.0
        left = np.asarray(hole, dtype=np.uint8) > 0
        height, width = left.shape
        pad_h, pad_w = max(height, LAMA_SIDE), max(width, LAMA_SIDE)
        if (pad_h, pad_w) != (height, width):
            image = np.pad(image, ((0, pad_h - height), (0, pad_w - width), (0, 0)), mode="edge")
            left = np.pad(left, ((0, pad_h - height), (0, pad_w - width)))
        rows = sorted(set(range(0, pad_h - LAMA_SIDE + 1, LAMA_STEP)) | {pad_h - LAMA_SIDE})
        cols = sorted(set(range(0, pad_w - LAMA_SIDE + 1, LAMA_STEP)) | {pad_w - LAMA_SIDE})
        for y in rows:
            for x in cols:
                window = (slice(y, y + LAMA_SIDE), slice(x, x + LAMA_SIDE))
                mask = left[window]
                if not mask.any():
                    continue
                weight = mask.astype(np.float32)
                tile = image[window] * (1.0 - weight[..., None])
                out = self.session.run(None, {"image": tile.transpose(2, 0, 1)[None],
                                              "mask": weight[None, None]})[0][0].transpose(1, 2, 0)
                image[window][mask] = np.clip(out[mask], 0.0, 1.0)
                left[window] = False
        filled = (image[:height, :width] * 255.0).round().astype(np.uint8)
        return self.Image.fromarray(np.ascontiguousarray(filled))


def _default_modules() -> SimpleNamespace:
    import numpy
    import torch

    # Importing sdnq registers SDNQConfig with diffusers' quantization loader, so it must
    # happen before from_pretrained reads the snapshot's quantization_config.json.
    import sdnq  # noqa: F401
    from diffusers import Flux2KleinInpaintPipeline
    from PIL import Image, ImageFilter

    return SimpleNamespace(torch=torch, np=numpy, Image=Image, ImageFilter=ImageFilter,
                           Pipeline=Flux2KleinInpaintPipeline, Lama=LamaFill)


class SdnqFluxRunner:
    """Loads the pinned snapshot once per container and renders crops with the recipe."""

    def __init__(self, snapshot_path: str, device: str = "cuda", modules: Optional[SimpleNamespace] = None):
        self.snapshot_path = str(snapshot_path)
        self.device = device
        self._modules = modules
        self.pipeline: Any = None
        self.lama: Any = None
        self.render_count = 0

    def _mods(self) -> SimpleNamespace:
        if self._modules is None:
            self._modules = _default_modules()
        return self._modules

    def load(self) -> Any:
        if self.pipeline is not None:
            return self.pipeline
        m = self._mods()
        if self.device == "cuda" and not bool(m.torch.cuda.is_available()):
            raise WorkerRenderError("gpu_unavailable", "The worker container has no CUDA device.")
        try:
            # The snapshot names Flux2KleinPipeline; the inpaint pipeline takes the same
            # modules and reads is_distilled from the same model_index.json.
            pipeline = m.Pipeline.from_pretrained(
                self.snapshot_path,
                torch_dtype=m.torch.bfloat16,
                local_files_only=True,
                low_cpu_mem_usage=True,
            )
            # Every GPU on the allowlist has at least 24 GB, and the 4-bit pipeline is
            # about 5.5 GB, so it moves to the GPU once. The local backend's CPU offload
            # exists for small consumer GPUs and would only add transfers per call here.
            # SDNQ's Triton quantized matmul stays off on purpose: it compiles kernels on
            # the first call, and with scale-to-zero that compile lands on most jobs.
            pipeline.to(self.device)
            lama = m.Lama(f"{self.snapshot_path}/{ADAPTER_DIR}/{LAMA_FILE}", self.device)
        except WorkerRenderError:
            raise
        except Exception as exc:
            code = "out_of_memory" if looks_like_memory(str(exc)) else "model_load_failed"
            raise WorkerRenderError(code, f"Failed to load the pinned pipeline: {exc}") from exc
        self.lama = lama
        self.pipeline = pipeline
        return pipeline

    def warmup(self) -> None:
        """One single-step render at the working size, output discarded.

        Run before a GPU memory snapshot, so the CUDA context, cuBLAS handles and the
        allocator's pools are set up once and restored with the snapshot instead of
        on the first job after every cold start. The seed is fixed and each job seeds
        its own generator, so nothing random carries over into a job.
        """
        m = self._mods()
        image, hint = io.BytesIO(), io.BytesIO()
        m.Image.new("RGB", (WARMUP_SIDE, WARMUP_SIDE), (255, 255, 255)).save(image, format="PNG")
        # Some lettering, so LaMa runs too and its CUDA setup is not left to the first job.
        lettering = m.Image.new("L", (WARMUP_SIDE, WARMUP_SIDE), 0)
        lettering.paste(255, (WARMUP_SIDE // 4, WARMUP_SIDE // 4, WARMUP_SIDE // 2, WARMUP_SIDE // 2))
        lettering.save(hint, format="PNG")
        count = self.render_count
        self.render_png(image.getvalue(), hint.getvalue(), WARMUP_SIDE, WARMUP_SIDE,
                        seed=0, steps=1, guidance_scaled=100)
        self.render_count = count

    def render_png(
        self,
        image_png: bytes,
        hint_png: bytes,
        width: int,
        height: int,
        seed: int,
        steps: int,
        guidance_scaled: int,
        qwen_edit: Optional[QwenEdit] = None,
    ) -> bytes:
        """Inpaint the hint's lettering in one crop; an RGB PNG at exactly width x height.

        `qwen_edit` is the request's guidance, None for a recipe that takes none.
        """
        pipeline = self.load()
        m = self._mods()
        torch, np, Image = m.torch, m.np, m.Image

        with Image.open(io.BytesIO(image_png)) as source:
            crop = source.convert("RGB")
        with Image.open(io.BytesIO(hint_png)) as source:
            hint = source.convert("L")
        if crop.size != (width, height) or hint.size != (width, height):
            raise WorkerRenderError("invalid_image", "Image geometry differs from the request")

        work_w, work_h = working_size(width, height)
        resized = (work_w, work_h) != crop.size
        # backend/base.py hole_for, in the sidecar.
        grown = hint.point(lambda v: 255 if v else 0).filter(m.ImageFilter.MaxFilter(2 * HOLE_GROWTH + 1))
        try:
            filled = self.lama.fill(crop, grown)
        except Exception as exc:
            code = "out_of_memory" if looks_like_memory(str(exc)) else "inference_failed"
            raise WorkerRenderError(code, f"LaMa fill failed: {exc}") from exc
        work_crop = filled.resize((work_w, work_h), Image.Resampling.BICUBIC) if resized else filled
        hole = grown.resize((work_w, work_h), Image.Resampling.NEAREST) if resized else grown
        # Host generator, as locally: one seed gives one result on any accelerator.
        generator = torch.Generator(device="cpu").manual_seed(int(seed))
        kwargs = filter_call_kwargs(
            pipeline,
            {
                "image": work_crop,
                "prompt": flux_prompt(qwen_edit),
                "width": work_w,
                "height": work_h,
                "num_inference_steps": int(steps),
                "guidance_scale": float(guidance_from_scaled(guidance_scaled)),
                "generator": generator,
                "output_type": "pt",
                "max_area": work_w * work_h,
            },
        )
        # Never filtered: a pipeline without them would redraw the whole crop. The pipeline
        # already conditions on `image`, the filled crop; a second copy as
        # `image_reference` changed nothing on the bench and cost 30% more time.
        kwargs.update(mask_image=hole, strength=1.0)

        try:
            # Out-of-place nan_to_num and clamp: inference tensors refuse in-place edits.
            with torch.inference_mode():
                out = pipeline(**kwargs)
                tensor = torch.nan_to_num(out.images[0], nan=0.0, posinf=1.0, neginf=0.0).clamp(0.0, 1.0)
                samples = tensor.float().mul(255.0).round().to(torch.uint8).permute(1, 2, 0).cpu().numpy()
        except Exception as exc:
            code = "out_of_memory" if looks_like_memory(str(exc)) else "inference_failed"
            raise WorkerRenderError(code, f"Diffusion inference failed: {exc}") from exc

        # A uint8 HxWx3 array is RGB; Pillow 12 deprecates passing mode here.
        result = Image.fromarray(np.ascontiguousarray(samples))
        if result.mode != "RGB":
            result = result.convert("RGB")
        if result.size != (width, height):
            result = result.resize((width, height), Image.Resampling.BOX)
        buffer = io.BytesIO()
        result.save(buffer, format="PNG")
        self.render_count += 1
        return buffer.getvalue()


@dataclass(frozen=True)
class RenderedJob:
    png: bytes
    digest: str


def render_job(
    runner: Any,
    meta: JobRequestMetadata,
    image_png: bytes,
    hint_png: bytes,
    limits: ServiceLimits,
) -> RenderedJob:
    """Validate one job, render it and validate the output, all in the worker.

    The gateway checked the same things before enqueueing; the worker checks again
    because the provider queue is reachable with the user's token on its own. Under
    preprocessing_version 2.0.0 the hint is the region's stored lettering and the crop
    carries up to 128 px of page per side (less when a larger crop would exceed the
    render limits). A FLUX runner inpaints the hint grown by HOLE_GROWTH; the Qwen
    runner edits the whole crop and reads the hint only to retry a seed that left the
    lettering untouched. The model takes no mask channel
    (native_mask_conditioning stays false): the pipeline only holds the latents outside
    the hole. The client still composites through its own write weights and aligns
    the returned tone to the page around the edit.
    """
    try:
        validate_crop_payload(meta, image_png, hint_png, limits)
    except ContractValidationError as exc:
        raise WorkerRenderError("invalid_request", str(exc)) from exc
    mismatch = recipe_mismatch(meta)
    if mismatch:
        raise WorkerRenderError("unsupported_recipe", mismatch)

    png = runner.render_png(
        image_png,
        hint_png,
        meta.width,
        meta.height,
        meta.seed,
        meta.steps,
        meta.guidance_scaled,
        # The contract admits guidance only on a recipe whose runner takes it.
        **({"qwen_edit": meta.recipe.qwen_edit} if meta.recipe.qwen_edit is not None else {}),
    )
    digest = hashlib.sha256(png).hexdigest()
    try:
        validate_worker_result(png, digest, meta.width, meta.height, limits)
    except ContractValidationError as exc:
        raise WorkerRenderError("invalid_output", str(exc)) from exc
    return RenderedJob(png=png, digest=digest)


# ---------------------------------------------------------------------------
# Offline fake used by the gateway tests
# ---------------------------------------------------------------------------


def encode_png_rgb8(
    width: int,
    height: int,
    fill_rgb: Tuple[int, int, int] = (255, 255, 255),
) -> bytes:
    """Generate a single-frame RGB8 PNG with valid IHDR CRC-32 and zlib compression."""
    raw_rows = bytearray()
    row_bytes = bytes([fill_rgb[0] & 0xFF, fill_rgb[1] & 0xFF, fill_rgb[2] & 0xFF]) * width
    for _ in range(height):
        raw_rows.append(0)  # Filter byte: None (0)
        raw_rows.extend(row_bytes)

    compressed = zlib.compress(bytes(raw_rows), level=6)

    ihdr_data = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    ihdr_crc = zlib.crc32(b"IHDR" + ihdr_data) & 0xFFFFFFFF
    ihdr_chunk = struct.pack(">I", 13) + b"IHDR" + ihdr_data + struct.pack(">I", ihdr_crc)

    idat_crc = zlib.crc32(b"IDAT" + compressed) & 0xFFFFFFFF
    idat_chunk = struct.pack(">I", len(compressed)) + b"IDAT" + compressed + struct.pack(">I", idat_crc)

    iend_crc = zlib.crc32(b"IEND") & 0xFFFFFFFF
    iend_chunk = struct.pack(">I", 0) + b"IEND" + struct.pack(">I", iend_crc)

    return b"\x89PNG\r\n\x1a\n" + ihdr_chunk + idat_chunk + iend_chunk


class WorkerLifecycleState(str, Enum):
    UNINITIALIZED = "uninitialized"
    COLD = "cold"
    WARMING = "warming"
    WARMED = "warmed"
    BUSY = "busy"
    FAILED = "failed"


class FluxWorker:
    """Offline fake worker: exact-size PNG output plus fault injection, no model."""

    def __init__(
        self,
        provider: str = "modal",
        limits: Optional[ServiceLimits] = None,
        simulated_cold_start_delay: float = 0.0,
        reported_cost_per_job: Optional[float] = 0.0012,
    ):
        self.provider = provider
        self.limits = limits or provisional_fixture_limits()
        self.simulated_cold_start_delay = simulated_cold_start_delay
        self.reported_cost_per_job = reported_cost_per_job
        self.state = WorkerLifecycleState.UNINITIALIZED
        self.warmup_count = 0
        self.inference_count = 0
        self.last_inferred_at: Optional[float] = None
        self.simulated_fault: Optional[str] = None  # "oom", "timeout", "corrupted_output", "wrong_dimensions"

    @property
    def is_warm(self) -> bool:
        return self.state == WorkerLifecycleState.WARMED

    def warmup(self) -> dict:
        """Explicit potentially billable worker readiness probe."""
        self.state = WorkerLifecycleState.WARMING
        if self.simulated_cold_start_delay > 0:
            time.sleep(self.simulated_cold_start_delay)
        self.state = WorkerLifecycleState.WARMED
        self.warmup_count += 1
        return {
            "status": "warmed",
            "provider": self.provider,
            "worker_state": "warm",
            "warmup_count": self.warmup_count,
        }

    def infer(
        self,
        job_record: JobRecord,
        image_bytes: bytes,
        hint_bytes: bytes,
    ) -> Tuple[bytes, str, Optional[float]]:
        """Return (result_bytes, result_digest, cost) for one job."""
        if self.state != WorkerLifecycleState.WARMED:
            self.state = WorkerLifecycleState.WARMING
            if self.simulated_cold_start_delay > 0:
                time.sleep(self.simulated_cold_start_delay)
            self.state = WorkerLifecycleState.WARMED

        if self.simulated_fault == "oom":
            raise RuntimeError("CUDA out of memory during inference")
        if self.simulated_fault == "timeout":
            raise TimeoutError("Worker execution timed out")
        if self.simulated_fault == "corrupted_output":
            corrupted = b"\x89PNG\r\n\x1a\nCORRUPTED_IDAT_BYTES_TRUNCATED"
            return corrupted, hashlib.sha256(corrupted).hexdigest(), self.reported_cost_per_job
        if self.simulated_fault == "wrong_dimensions":
            wrong_png = encode_png_rgb8(job_record.width + 16, job_record.height)
            return wrong_png, hashlib.sha256(wrong_png).hexdigest(), self.reported_cost_per_job

        result_png = encode_png_rgb8(job_record.width, job_record.height, fill_rgb=(240, 240, 240))
        self.inference_count += 1
        self.last_inferred_at = time.time()
        return result_png, hashlib.sha256(result_png).hexdigest(), self.reported_cost_per_job


FakeFluxWorker = FluxWorker

__all__ = [
    "FakeFluxWorker",
    "FluxWorker",
    "RenderedJob",
    "SdnqFluxRunner",
    "WorkerLifecycleState",
    "WorkerRenderError",
    "encode_png_rgb8",
    "filter_call_kwargs",
    "flux_prompt",
    "guidance_from_scaled",
    "recipe_mismatch",
    "render_job",
    "sampling_mismatch",
    "snap_stride",
    "working_size",
]
