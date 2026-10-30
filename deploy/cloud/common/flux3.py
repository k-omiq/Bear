"""FLUX 3 Image Klein worker: an edit of the crop as it is, sampled the way BFL samples it.

The sidecar runs the first edit locally too (sidecar/manga_cleaner_sidecar/backend/edit.py), without
v2's retry below. Klein v2 (common/flux.py) inpaints a hole LaMa has already filled, so Klein
only redraws LaMa's guess. FLUX 3 is an edit model with a stream of its own for the
reference image, so it gets the crop with its lettering and `FLUX3_PROMPT`, which names
where the lettering is (`hint_location`), and removes the lettering itself. The first
FLUX 3 bench (docs/research/flux3-klein-5_6b.md, 2026-10-19) gave it the LaMa-filled crop
under a latent hold and so measured LaMa again.

v2 (2026-10-30): bold lettering often comes back untouched (5 of 19 real crops in
docs/research/inpaint-models-2026-10.md). `lettering_kept` sees it, and the same seed then
edits the crop with its hole filled by the manga LaMa, which removed all five. A clean first
answer is returned as it is, because it scores better against truth than an edit of the fill.

Sampling follows BFL's reference code (github.com/black-forest-labs/flux-3-image-klein,
src/flux3/pipeline.py and sampling.py), not the Diffusers defaults:
- the step-distilled flow map, 16 steps, no CFG;
- time shift 16.0: BFL moved from 6.93 on 2026-10-17, and Diffusers 0.42 still builds
  its scheduler with 6.93;
- the noise drawn as image tokens on the host and cast to bf16, which reproduces BFL's
  sample for a seed on any GPU;
- the autoencoder in float32, the rest in bf16.
The prompt upsampler is not loaded: on the 2026-10-19 bench it added 10 to 14 s and
18.8 GB per crop and misread the pages twice.

`hold`, off by default, holds the image tokens outside the grown hint to the crop's own,
noised to each step's level, the way Klein's inpaint pipeline holds its latents.

A request's `flux3_generation` builds any part Manga Cleaner Reproduction's way (its
generation v2): its prompt, which names where the lettering sits (`location_prompt`,
`LOCATION_FLUX3_PROMPT`), the lettering painted magenta into the crop and named in the
prompt (`marker`, `MARKER_FLUX3_PROMPT`), with a result that kept the paint refused as
`marker_remained` (`marker_remaining`), its time shift 6.93 (`low_shift`, `LOW_TIME_SHIFT`),
and its one-megapixel working size (`megapixel_size`).

The worker returns the whole edited crop. The client writes it only inside the region's
applied mask and aligns its tone to the page (crates/cleaner-core/src/engines/render.rs).

torch, diffusers, sdnq, PIL and numpy are imported when the runner loads.
"""

from __future__ import annotations

import io
import math
from dataclasses import dataclass
from types import SimpleNamespace
from typing import Any, Optional

from deploy.cloud.common.contract import LATENT_STRIDE, Flux3Generation, QwenEdit
from deploy.cloud.common.flux import (
    HOLE_GROWTH,
    WARMUP_SIDE,
    LamaFill,
    WorkerRenderError,
    hint_location,
    looks_like_memory,
    request_work_size,
)
from deploy.cloud.common.manifest import (
    ADAPTER_DIR,
    FLUX3_PROMPT,
    LAMA_FILE,
    LOCATION_FLUX3_PROMPT,
    MARKER_FLUX3_PROMPT,
    WORK_LONG_SIDE,
)

# BFL's `alpha` (src/flux3/pipeline.py generate) since 2026-10-17.
TIME_SHIFT = 16.0
# Its earlier one, which Manga Cleaner Reproduction samples with: `mu` 1.93586 in its
# sampling_schedules.json, a shift of e**mu.
LOW_TIME_SHIFT = math.exp(1.93586)
# The marker colour, and how much of it left in the lettering refuses a result
# (Manga Cleaner Reproduction's imaging.py selection_marker_remaining).
MARKER = (255, 0, 255)
# Correlation of the lettering's grey pixels before and after the edit above which the
# lettering is still there. On 31 crops the five failures read 0.77 and up and every other
# crop 0.21 or less (docs/research/inpaint-models-2026-10.md).
LETTERING_KEPT_CORRELATION = 0.5


def lettering_location(lettering: Any, np: Any) -> str:
    """Manga Cleaner Reproduction's words for where the lettering sits ("upper centre").

    Its prepare_job: the middle of the lettering's bounding box, end exclusive, in thirds.
    """
    ys, xs = np.nonzero(lettering)
    if not len(xs):
        return "middle centre"
    cy = (float(ys.min()) + float(ys.max()) + 1) / 2 / lettering.shape[0]
    cx = (float(xs.min()) + float(xs.max()) + 1) / 2 / lettering.shape[1]
    row = "upper" if cy < 1/3 else "lower" if cy > 2/3 else "middle"
    col = "left" if cx < 1/3 else "right" if cx > 2/3 else "centre"
    return f"{row} {col}"


def lettering_kept(before: Any, after: Any, ink: Any, np: Any) -> bool:
    """Whether the edit left the lettering where it was: its pixels still follow the input's.

    `before` and `after` are RGB arrays, `ink` the hint as it came. A flat side has no
    correlation to read: a flat input had nothing to remove, a flat answer removed it.
    """
    if not ink.any():
        return False
    was, now = before.mean(axis=2)[ink], after.mean(axis=2)[ink]
    if was.std() < 1.0 or now.std() < 1.0:
        return False
    return float(np.corrcoef(was, now)[0, 1]) > LETTERING_KEPT_CORRELATION


def magenta_pixels(samples: Any, np: Any) -> Any:
    """Where an RGB image reads as the marker: red and blue above 160, each more than 90 above green."""
    pixels = samples.astype(np.int16)
    magenta = (pixels[:, :, 0] > 160) & (pixels[:, :, 2] > 160)
    return magenta & (pixels[:, :, 0] - pixels[:, :, 1] > 90) & (pixels[:, :, 2] - pixels[:, :, 1] > 90)


def marker_remaining(samples: Any, lettering: Any, np: Any, unpainted: Any = None) -> bool:
    """Whether an RGB result kept the magenta marker in the lettering.

    More than max(16, 1 % of the lettering) magenta pixels (`magenta_pixels`). A pixel
    already magenta in the `unpainted` crop is not counted: pink art in the grown hint
    the model drew back is not paint left behind.
    """
    kept = magenta_pixels(samples, np) & lettering
    if unpainted is not None:
        kept &= ~magenta_pixels(unpainted, np)
    return int(np.count_nonzero(kept)) > max(16, int(np.count_nonzero(lettering)) * 0.01)


def flux3_prompt(edit: Optional[QwenEdit], location: Optional[str]) -> str:
    """The recipe prompt for lettering at `location`, plus the request's description.

    The prompt names every kind of lettering already, so the target adds nothing; a
    description says what lies under the lettering, as for Klein v2.
    """
    prompt = FLUX3_PROMPT.format(location=location or "central")
    if edit is not None and edit.description.strip():
        prompt += f" Behind the lettering: {edit.description.strip()}."
    return prompt


@dataclass
class LatentHold:
    """The image tokens outside the hole, held to the reference's own at every step.

    `keep` is 1 for a token outside the hole, shape (1, tokens, 1); `noise` the initial
    noise tokens. After a step that lands on noise level `sigma`, a kept token becomes the
    reference token noised to that level, so the last step leaves it exactly the
    reference's.
    """

    keep: Any
    noise: Any

    def apply(self, latents: Any, reference: Any, sigma: Any) -> Any:
        if reference is None or tuple(reference.shape) != tuple(latents.shape):
            raise WorkerRenderError("inference_failed",
                                    "The reference tokens do not line up with the image tokens")
        level = float(sigma)
        held = (1.0 - level) * reference.float() + level * self.noise.float()
        return (self.keep * held + (1.0 - self.keep) * latents.float()).to(latents.dtype)


def _hold_block(base: Any, input_param: Any) -> Any:
    """A denoising-loop block that applies the request's `LatentHold`, if it has one.

    The way Diffusers documents adding a step to a modular denoising loop
    (docs/source/en/using-diffusers/callback.md, "Modular pipelines"): it runs after the
    scheduler step and edits the loop's `latents` in place.
    """

    class HoldOutsideHole(base):
        @property
        def inputs(self) -> list:
            return [
                input_param("hold"),
                input_param("latents", required=True),
                input_param("image_cond_hidden_states"),
            ]

        def __call__(self, components: Any, block_state: Any, i: int, t: Any) -> Any:
            hold = block_state.hold
            if hold is not None:
                block_state.latents = hold.apply(block_state.latents, block_state.image_cond_hidden_states,
                                                 components.scheduler.sigmas[i + 1])
            return components, block_state

    return HoldOutsideHole


def _denoise_loops(block: Any, loop_class: Any) -> list:
    """Every denoising loop under `block`, whichever workflow it belongs to."""
    found = [block] if isinstance(block, loop_class) and "denoiser" in block.sub_blocks else []
    for sub in getattr(block, "sub_blocks", {}).values():
        found += _denoise_loops(sub, loop_class)
    return found


def _default_modules() -> SimpleNamespace:
    import numpy
    import torch

    # Registers SDNQConfig before the transformer's quantization config is read.
    import sdnq  # noqa: F401
    from diffusers import FlowMatchEulerDiscreteScheduler, ModularPipeline
    from diffusers.modular_pipelines import InputParam, LoopSequentialPipelineBlocks, ModularPipelineBlocks
    from PIL import Image, ImageFilter

    return SimpleNamespace(torch=torch, np=numpy, Image=Image, ImageFilter=ImageFilter,
                           Modular=ModularPipeline, Scheduler=FlowMatchEulerDiscreteScheduler, Lama=LamaFill,
                           Loop=LoopSequentialPipelineBlocks, HoldBlock=_hold_block(ModularPipelineBlocks, InputParam))


class Flux3EditRunner:
    """Loads the pinned snapshot once per container and edits crops with the recipe."""

    hold = False

    def __init__(self, snapshot_path: str, device: str = "cuda", modules: Optional[SimpleNamespace] = None):
        self.snapshot_path = str(snapshot_path)
        self.device = device
        self._modules = modules
        self.pipeline: Any = None
        self.lama: Any = None
        self.shift = TIME_SHIFT
        self.render_count = 0

    def _mods(self) -> SimpleNamespace:
        if self._modules is None:
            self._modules = _default_modules()
        return self._modules

    def use_shift(self, shift: float) -> None:
        """Sample the next render at `shift`: a new scheduler only when it changes."""
        if shift != self.shift:
            self.pipeline.update_components(scheduler=self._mods().Scheduler(shift=shift))
            self.shift = shift

    def load(self) -> Any:
        if self.pipeline is not None:
            return self.pipeline
        m = self._mods()
        if self.device == "cuda" and not bool(m.torch.cuda.is_available()):
            raise WorkerRenderError("gpu_unavailable", "The worker container has no CUDA device.")
        try:
            # Blocks are edited before the pipeline is built: `pipe.blocks` is a copy.
            blocks = m.Modular.from_pretrained(self.snapshot_path, local_files_only=True).blocks
            blocks.sub_blocks.pop("prompt_upsampler", None)
            for loop in _denoise_loops(blocks, m.Loop):
                loop.sub_blocks["hold"] = m.HoldBlock()
            pipeline = blocks.init_pipeline(self.snapshot_path)
            pipeline.load_components(dtype={"vae": m.torch.float32, "default": m.torch.bfloat16})
            # load_components logs a component it cannot load and carries on.
            missing = [name for name in ("transformer", "vae", "text_encoder", "tokenizer")
                       if getattr(pipeline, name, None) is None]
            if missing:
                raise RuntimeError(f"components not loaded: {', '.join(missing)}")
            pipeline.update_components(scheduler=m.Scheduler(shift=TIME_SHIFT))
            # The 4-bit pipeline is about 7 GB, the bf16 one 20; it moves to the GPU once, as Klein's does.
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
        """One single-step render at the working size, output discarded (see SdnqFluxRunner.warmup)."""
        m = self._mods()
        image, hint = io.BytesIO(), io.BytesIO()
        m.Image.new("RGB", (WARMUP_SIDE, WARMUP_SIDE), (255, 255, 255)).save(image, format="PNG")
        lettering = m.Image.new("L", (WARMUP_SIDE, WARMUP_SIDE), 0)
        lettering.paste(255, (WARMUP_SIDE // 4, WARMUP_SIDE // 4, WARMUP_SIDE // 2, WARMUP_SIDE // 2))
        lettering.save(hint, format="PNG")
        count = self.render_count
        self.render_png(image.getvalue(), hint.getvalue(), WARMUP_SIDE, WARMUP_SIDE,
                        seed=0, steps=1, guidance_scaled=100)
        self.render_count = count

    def prompt(self, edit: Optional[QwenEdit], lettering: Any, generation: Optional[Flux3Generation] = None) -> str:
        """The prompt for one crop; `lettering` is the grown hint at crop size."""
        np = self._mods().np
        if generation is not None and generation.location_prompt:
            prompt = LOCATION_FLUX3_PROMPT.replace("{location}", lettering_location(lettering, np))
            if edit is not None and edit.description.strip():
                prompt += " Target description: " + edit.description.strip()
        else:
            prompt = flux3_prompt(edit, hint_location(lettering, np))
        if generation is not None and generation.marker:
            prompt += MARKER_FLUX3_PROMPT
        return prompt

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
        work_long_side: int = WORK_LONG_SIDE,
        flux3_generation: Optional[Flux3Generation] = None,
    ) -> bytes:
        """Edit the hint's lettering out of one crop; an RGB PNG at exactly width x height.

        The distilled model takes no guidance scale; the recipe pins it at 1.0. A crop whose
        long side is below `work_long_side` is edited scaled up to it, or at one megapixel
        when `flux3_generation` asks for Manga Cleaner Reproduction's size. Lettering the
        edit left in place is edited again from a LaMa fill; the marker has its own check.
        """
        generation = flux3_generation or Flux3Generation()
        self.load()
        m = self._mods()
        np, Image = m.np, m.Image

        with Image.open(io.BytesIO(image_png)) as source:
            crop = source.convert("RGB")
        with Image.open(io.BytesIO(hint_png)) as source:
            hint = source.convert("L")
        if crop.size != (width, height) or hint.size != (width, height):
            raise WorkerRenderError("invalid_image", "Image geometry differs from the request")

        grown = hint.point(lambda v: 255 if v else 0).filter(m.ImageFilter.MaxFilter(2 * HOLE_GROWTH + 1))
        lettering = np.asarray(grown) > 0
        unpainted = None
        if generation.marker:
            # Painted before the resize, as Manga Cleaner Reproduction does.
            unpainted = np.asarray(crop)
            painted = np.array(crop)
            painted[lettering] = MARKER
            crop = Image.fromarray(painted)

        def edit(source: Any) -> Any:
            return self._edit(source, grown, lettering, seed, steps, qwen_edit, work_long_side, generation)

        result = edit(crop)
        if generation.marker:
            if marker_remaining(np.asarray(result), lettering, np, unpainted):
                raise WorkerRenderError("marker_remained",
                                        "The model left the magenta marker in the lettering; try another seed.")
        elif lettering_kept(np.asarray(crop), np.asarray(result), np.asarray(hint) > 0, np):
            try:
                filled = self.lama.fill(crop, grown)
            except Exception as exc:
                code = "out_of_memory" if looks_like_memory(str(exc)) else "inference_failed"
                raise WorkerRenderError(code, f"LaMa fill failed: {exc}") from exc
            result = edit(filled)
        buffer = io.BytesIO()
        result.save(buffer, format="PNG")
        self.render_count += 1
        return buffer.getvalue()

    def _edit(self, crop: Any, grown: Any, lettering: Any, seed: int, steps: int, qwen_edit: Optional[QwenEdit],
              work_long_side: int, generation: Flux3Generation) -> Any:
        """One sampling of the pipeline over `crop`: an RGB image at the crop's own size."""
        pipeline = self.pipeline
        m = self._mods()
        torch, np, Image = m.torch, m.np, m.Image
        width, height = crop.size
        work_w, work_h = request_work_size(width, height, work_long_side, generation)
        # Lanczos both ways, as for Qwen: the crop is usually stretched, and bicubic in and
        # box out lost fine tone and line detail (docs/findings.md, 2026-10-19).
        work_crop = crop if crop.size == (work_w, work_h) else crop.resize((work_w, work_h), Image.Resampling.LANCZOS)

        rows, cols = work_h // LATENT_STRIDE, work_w // LATENT_STRIDE
        channels = int(pipeline.transformer.config.in_channels)
        # BFL draws its noise as tokens on the CPU and casts it to bf16 (the Diffusers FLUX.3
        # docs, "Reproducible generation"); the same seed gives the same sample anywhere.
        noise = torch.randn((rows * cols, channels), generator=torch.Generator().manual_seed(int(seed)))
        noise = noise.to(torch.bfloat16)
        hold = None
        if self.hold and lettering.any():
            hole = np.asarray(grown.resize((work_w, work_h), Image.Resampling.NEAREST)) > 0
            # A token is in the hole when any of its 16 x 16 pixels is.
            tokens = hole.reshape(rows, LATENT_STRIDE, cols, LATENT_STRIDE).any(axis=(1, 3))
            keep = torch.from_numpy((~tokens).reshape(1, rows * cols, 1).astype(np.float32)).to(self.device)
            hold = LatentHold(keep=keep, noise=noise[None].to(self.device))

        self.use_shift(LOW_TIME_SHIFT if generation.low_shift else TIME_SHIFT)
        try:
            with torch.inference_mode():
                images = pipeline(
                    prompt=self.prompt(qwen_edit, lettering, generation),
                    image=work_crop,
                    latents=noise.T.reshape(1, channels, rows, cols),
                    height=work_h,
                    width=work_w,
                    num_inference_steps=int(steps),
                    hold=hold,
                    output="images",
                )
        except WorkerRenderError:
            raise
        except Exception as exc:
            code = "out_of_memory" if looks_like_memory(str(exc)) else "inference_failed"
            raise WorkerRenderError(code, f"Diffusion inference failed: {exc}") from exc

        result = images[0].convert("RGB")
        if result.size != (width, height):
            result = result.resize((width, height), Image.Resampling.LANCZOS)
        return result


__all__ = ["Flux3EditRunner", "LETTERING_KEPT_CORRELATION", "LatentHold", "LOW_TIME_SHIFT", "TIME_SHIFT", "flux3_prompt",
           "lettering_kept", "lettering_location",
           "magenta_pixels", "marker_remaining"]
