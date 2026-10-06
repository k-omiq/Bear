"""Qwen-Image-Edit-2511 worker: the 4-bit SDNQ base with the Lightning and FukidashiErase LoRAs.

Cloud only; there is no local twin. Loads like `SdnqFluxRunner` (diffusers + sdnq,
everything on the GPU) and renders the same crops: the client prepares crop, hint
and composite exactly as for Klein. Two things differ:

- The pipeline always resizes its reference image to about one megapixel on a
  32 px grid. The crop is resized to exactly that size first, so the reference and
  the answer share one grid; any other size moves the answer by several pixels.
- The LoRAs stay PEFT adapters at runtime, both at weight 1.0. SDNQ layers hold packed 4-bit weights
  that a fuse cannot write into, and the adapter costs little at 8 steps.
- The runner compares changes inside the hint with changes outside it and retries
  low-change outputs with two further seeds. The v4 recipe reports a failed job if
  no seed passes. This measures change, not successful lettering removal; the
  client must show the actual blended result for human review before applying it.
- Target choices and a short optional description build a prompt per request.
  The hint's bounding box supplies a coarse location; it is never a mask channel.

torch, diffusers, sdnq, peft, PIL and numpy are imported when the runner loads.
"""

from __future__ import annotations

import io
import math
from types import SimpleNamespace
from typing import Any, Dict, Optional, Tuple

from deploy.cloud.common.flux import (
    WARMUP_SIDE,
    WorkerRenderError,
    filter_call_kwargs,
    guidance_from_scaled,
    looks_like_memory,
)
from deploy.cloud.common.contract import QwenEdit
from deploy.cloud.common.manifest import ADAPTER_DIR, FUKIDASHI_FILE, LIGHTNING_FILE, QWEN_PROMPT

# Renders after the first when the lettering was not edited, each with the next seed.
EDIT_RETRIES = 2
# The hint's pixels must change this many times more than the rest of the crop for the
# render to count as an edit. Measured on c32 p20 (2026-10-02): an untouched render
# scored 0.6, the five that removed the lettering about 8.
MIN_EDIT_RATIO = 2.0

# The pipeline's VAE reference area (VAE_IMAGE_SIZE in pipeline_qwenimage_edit_plus.py).
REFERENCE_AREA = 1024 * 1024
GRID = 32

# Scheduler the Lightning LoRA was distilled with (lightx2v model card).
LIGHTNING_SCHEDULER: Dict[str, Any] = {
    "base_image_seq_len": 256,
    "base_shift": math.log(3),
    "invert_sigmas": False,
    "max_image_seq_len": 8192,
    "max_shift": math.log(3),
    "num_train_timesteps": 1000,
    "shift": 1.0,
    "shift_terminal": None,
    "stochastic_sampling": False,
    "time_shift_type": "exponential",
    "use_beta_sigmas": False,
    "use_dynamic_shifting": True,
    "use_exponential_sigmas": False,
    "use_karras_sigmas": False,
}


def work_size(width: int, height: int) -> Tuple[int, int]:
    """The size the pipeline gives its reference for this aspect ratio (calculate_dimensions)."""
    ratio = int(width) / int(height)
    w = math.sqrt(REFERENCE_AREA * ratio)
    h = w / ratio
    return round(w / GRID) * GRID, round(h / GRID) * GRID


def build_prompt(edit: Optional[QwenEdit], lettering: Any = None, np: Any = None) -> str:
    """Translate a simple target/description into scoped erasure instructions.

    Hint geometry identifies a coarse area; it is not a mask channel and does
    not distinguish text from artwork. Human review is still required.
    """
    edit = edit or QwenEdit()
    location = "in this crop"
    if lettering is not None and np is not None:
        ys, xs = np.nonzero(lettering)
        if len(xs):
            cy = (float(ys.min()) + float(ys.max())) / 2 / lettering.shape[0]
            cx = (float(xs.min()) + float(xs.max())) / 2 / lettering.shape[1]
            row = "upper" if cy < 1/3 else "lower" if cy > 2/3 else "middle"
            col = "left" if cx < 1/3 else "right" if cx > 2/3 else "center"
            location = f"in the {row} {col} area of this crop"
    action = {
        "auto": "Remove the text and lettering",
        "dialogue": "Remove the dialogue lettering inside the speech bubble",
        "sound_effect": "Remove the sound-effect lettering, including thick character strokes cut off by image edges",
        "other": "Remove the unwanted text or lettering",
    }[edit.target]
    preserve_text = {"dialogue": " Preserve sound effects.", "sound_effect": " Preserve dialogue."}.get(edit.target, "")
    description = f" The user describes the lettering to remove as: {edit.description.strip()}." if edit.description.strip() else ""
    return (
        f"{action} {location}." + description + preserve_text
        + " Reconstruct the background and thin lines hidden behind the erased lettering."
        + " Preserve all character artwork, hands, faces, hair, poses, speech-bubble outlines,"
        + " panel borders, surrounding details, and text outside the target area."
        + " Match the original line style and tone. Add no new text or symbols."
    )


def _default_modules() -> SimpleNamespace:
    import numpy
    import torch

    import sdnq  # noqa: F401  registers SDNQConfig before from_pretrained reads it
    from diffusers import FlowMatchEulerDiscreteScheduler, QwenImageEditPlusPipeline
    from PIL import Image

    return SimpleNamespace(torch=torch, np=numpy, Image=Image, Pipeline=QwenImageEditPlusPipeline,
                           Scheduler=FlowMatchEulerDiscreteScheduler)


class QwenEditRunner:
    """Loads the pinned snapshot and its LoRA once per container and renders crops."""

    def __init__(self, snapshot_path: str, device: str = "cuda", modules: Optional[SimpleNamespace] = None):
        self.snapshot_path = str(snapshot_path)
        self.device = device
        self._modules = modules
        self.pipeline: Any = None
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
            pipeline = m.Pipeline.from_pretrained(
                self.snapshot_path,
                scheduler=m.Scheduler.from_config(LIGHTNING_SCHEDULER),
                torch_dtype=m.torch.bfloat16,
                local_files_only=True,
                low_cpu_mem_usage=True,
            )
            adapters = f"{self.snapshot_path}/{ADAPTER_DIR}"
            pipeline.load_lora_weights(adapters, weight_name=LIGHTNING_FILE, adapter_name="lightning")
            pipeline.load_lora_weights(adapters, weight_name=FUKIDASHI_FILE, adapter_name="fukidashi")
            # Loading a second adapter does not by itself keep the first one active.
            pipeline.set_adapters(["lightning", "fukidashi"], [1.0, 1.0])
            # About 18 GB with the adapters; the required GPU (L40S, RTX 5090) holds it all.
            pipeline.to(self.device)
            pipeline.set_progress_bar_config(disable=True)
        except WorkerRenderError:
            raise
        except Exception as exc:
            code = "out_of_memory" if looks_like_memory(str(exc)) else "model_load_failed"
            raise WorkerRenderError(code, f"Failed to load the pinned pipeline: {exc}") from exc
        self.pipeline = pipeline
        return pipeline

    def warmup(self) -> None:
        """One single-step render, output discarded (see SdnqFluxRunner.warmup)."""
        m = self._mods()
        buffer = io.BytesIO()
        m.Image.new("RGB", (WARMUP_SIDE, WARMUP_SIDE), (255, 255, 255)).save(buffer, format="PNG")
        count = self.render_count
        self.render_png(buffer.getvalue(), b"", WARMUP_SIDE, WARMUP_SIDE, seed=0, steps=1, guidance_scaled=100)
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
        """Edit one crop and return an RGB PNG at exactly width x height.

        The edit pipeline takes no mask and redraws the whole crop. The hint only
        decides whether the lettering was edited; when it was not, the next seed runs,
        and if no seed edits it the job fails without offering a patch.
        An empty hint (the warmup) skips the check.
        """
        m = self._mods()
        with m.Image.open(io.BytesIO(image_png)) as source:
            crop = source.convert("RGB")
        if crop.size != (width, height):
            raise WorkerRenderError("invalid_image", "Image geometry differs from the request")
        lettering = self._lettering(hint_png, crop.size)

        prompt = build_prompt(qwen_edit, lettering, m.np)
        best: Optional[Tuple[float, Any]] = None
        for attempt in range(1 + (EDIT_RETRIES if lettering is not None else 0)):
            result = self._render(crop, int(seed) + attempt, steps, guidance_scaled, prompt=prompt)
            if lettering is None:
                break
            ratio = edit_ratio(m.np, crop, result, lettering)
            if best is None or ratio > best[0]:
                best = (ratio, result)
            if ratio >= MIN_EDIT_RATIO:
                break
        if best is not None:
            if best[0] < MIN_EDIT_RATIO:
                raise WorkerRenderError("edit_not_applied", "Qwen did not make a sufficient edit after three seeds. Try describing the lettering.")
            result = best[1]

        buffer = io.BytesIO()
        result.save(buffer, format="PNG")
        self.render_count += 1
        return buffer.getvalue()

    def _lettering(self, hint_png: bytes, size: Tuple[int, int]) -> Any:
        """The hint as a boolean array, or None when it cannot tell lettering from page."""
        if not hint_png:
            return None
        m = self._mods()
        with m.Image.open(io.BytesIO(hint_png)) as source:
            hint = source.convert("L")
        if hint.size != size:
            raise WorkerRenderError("invalid_image", "Image geometry differs from the request")
        lettering = m.np.asarray(hint) > 0
        return lettering if lettering.any() and not lettering.all() else None

    def _render(self, crop: Any, seed: int, steps: int, guidance_scaled: int, prompt: str = QWEN_PROMPT) -> Any:
        """One pipeline call: the edited crop as an RGB image at the crop's size."""
        pipeline = self.load()
        m = self._mods()
        torch, np, Image = m.torch, m.np, m.Image
        width, height = crop.size

        work_w, work_h = work_size(width, height)
        work_crop = crop if crop.size == (work_w, work_h) else crop.resize((work_w, work_h), Image.Resampling.BICUBIC)
        cfg = float(guidance_from_scaled(guidance_scaled))
        generator = torch.Generator(device="cpu").manual_seed(int(seed))
        kwargs = filter_call_kwargs(
            pipeline,
            {
                "image": [work_crop],
                "prompt": prompt,
                # True CFG only above 1.0; Lightning is distilled to run without it.
                "negative_prompt": " " if cfg > 1.0 else None,
                "true_cfg_scale": cfg,
                "width": work_w,
                "height": work_h,
                "num_inference_steps": int(steps),
                "generator": generator,
                "output_type": "pt",
            },
        )

        try:
            with torch.inference_mode():
                out = pipeline(**kwargs)
                tensor = torch.nan_to_num(out.images[0], nan=0.0, posinf=1.0, neginf=0.0).clamp(0.0, 1.0)
                samples = tensor.float().mul(255.0).round().to(torch.uint8).permute(1, 2, 0).cpu().numpy()
        except Exception as exc:
            code = "out_of_memory" if looks_like_memory(str(exc)) else "inference_failed"
            raise WorkerRenderError(code, f"Diffusion inference failed: {exc}") from exc

        result = Image.fromarray(np.ascontiguousarray(samples))
        if result.mode != "RGB":
            result = result.convert("RGB")
        if result.size != (width, height):
            shrink = result.size[0] >= width
            result = result.resize((width, height), Image.Resampling.BOX if shrink else Image.Resampling.BICUBIC)
        return result


def edit_ratio(np: Any, before: Any, after: Any, lettering: Any) -> float:
    """Mean change of the lettering's pixels over the mean change of the rest of the crop.

    Grey levels, so the measure does not care whether the lettering is dark or light.
    The rest of the crop always moves a little (Qwen redraws it), which is the scale.
    """
    diff = np.abs(np.asarray(after.convert("L"), dtype=np.float32) - np.asarray(before.convert("L"), dtype=np.float32))
    return float(diff[lettering].mean()) / max(float(diff[~lettering].mean()), 1.0)


__all__ = ["EDIT_RETRIES", "LIGHTNING_SCHEDULER", "MIN_EDIT_RATIO", "QwenEditRunner", "edit_ratio", "work_size"]
