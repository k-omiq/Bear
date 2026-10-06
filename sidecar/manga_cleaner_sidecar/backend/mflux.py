"""mflux / MLX backend implementation for FLUX.2 Klein 4B inpainting.

Implements MLX allocator bounding, a working resolution for small crops, and
verified MLX inference hooks for Apple Silicon.

**The hole.** mflux 0.18.1's Klein edit has no mask, so :func:`_generate_masked`
is its denoising loop with one step added: after every step the latents outside
the hint's grown lettering (:func:`hole_for`) are put back to the crop's own,
noised to the next sigma. diffusers' `Flux2KleinInpaintPipeline`, which the sdnq
backend and the cloud worker run, does the same.

**Why no `MemorySaver`.** `mflux`'s `MemorySaver` is the callback that applies
two of the four guards this backend used to echo, and one of them - nulling the
text encoder before the denoising loop whenever `num_seeds <= 1` - makes the
*second* render through one open throw, because FLUX.2 re-encodes the prompt on
every call and has no `prompt_cache` to fall back on. The callback is therefore
not registered: the encoder is kept, one open serves many renders, and what
bounds this process is `mx.set_memory_limit` / `mx.set_cache_limit` plus the
parent's per-region peak check. The price is standing footprint, and
`DEFAULT_DECLARED_WORKING_SET` below is raised to pay it honestly rather than
declaring the evicting figure.

**VAE tiling is per render, not per open**, for the same honesty reason: it
helps a large crop's decode spike and *hurts* a small one, because `mflux` tiles
the encode above 512 px and the decode above a 512 px output, blending the tiles
with a cosine ramp - seams across exactly the working resolution this backend
now upscales small crops to. So it is switched on the crop, and no longer echoed
at open as though it applied to every render.
"""

from __future__ import annotations

import gc
import os
import tempfile
import time
from typing import Any, Dict, List, Optional, Set, Tuple

from ..memory import (
    calculate_dir_size_bytes,
    get_peak_phys_footprint_bytes,
    get_peak_rss_bytes,
)
from ..protocol import Basis, ErrorKind, ImageBuffer, ModelMetadata, SidecarError
from .base import BackendBase, hole_for

DEFAULT_WEIGHTS_DIR = "/Users/caved/dev/manga-cleaner/.sidecar-venv/weights/flux2-klein-4b-mflux-q4"

#: Declared working set, 10 GiB. Sized for a model that **keeps** its text
#: encoder and edits the untiled arm at :data:`WORK_LONG_SIDE`: measured
#: 8.36 GB of peak physical footprint for two renders against 6.58 GB
#: for the evicting path; two renders of two tight crops measured 8.92 GB here,
#: and the largest shape the untiled arm can reach - 768² - measured **9.77 GB**.
#: The rule is that understating this figure is the one direction that is not
#: safe, because it is what the hardware gate admits this process onto a machine
#: with, so the declaration sits above the largest figure measured and not on it.
#: What it still does not cover is the *tiled* arm: a crop past
#: :data:`VAE_TILING_FREE_LONG_SIDE` costs more than this and always did.
DEFAULT_DECLARED_WORKING_SET = 10_737_418_240

#: Long side a small crop is upscaled to before the edit, and downscaled back
#: from afterwards. FLUX.2 edit resolves poorly on tiny inputs - a crop where
#: the glyphs are twenty pixels tall comes back as a smudge - and ~768 px gives
#: the model enough to reconstruct screentone without paying full-page cost.
#: Snapped to :data:`LATENT_STRIDE`.
#:
#: **Swept, and it is this repository's figure now rather than the reference
#: implementation's.** 512, 1024 and the reference's own
#: ~1 megapixel *area* target were each run against 768 on both fixture regions
#: at the winning prompt:
#:
#: * **512** is the failure this constant exists to prevent, still: the balloon
#:   came back 73 grey levels dark with 36 levels of invented texture.
#: * **1024** and **~1MP** both replaced a flat white balloon with a fabric-weave
#:   texture 23 to 28 levels dark under the shipped prompt, were no better than
#:   768 on the sound effect under the winning one, and cost 2–6× the wall clock
#:   (13 s → 25 s → 60 s for one balloon region on an M5).
#:
#: So the good arm is narrow and 768 sits inside it. The upscale filter was swept
#: too: LANCZOS in both directions - which is what the reference uses - measured
#: no better than the bicubic-up / area-down pair below and looked slightly
#: heavier on halftone, so the pair stays.
WORK_LONG_SIDE = 768

#: The working resolution is snapped down to a multiple of this, because a
#: latent-space model works in multiples of its patch and VAE downsampling
#: factor and silently resizes anything else. Same number as
#: `cleaner_core::engines::flux::LATENT_STRIDE`, on purpose.
LATENT_STRIDE = 16

#: At or below this working long side the VAE runs untiled. Above it, tiling is
#: applied, which is what this backend used to do for every render.
#:
#: Equal to :data:`WORK_LONG_SIDE` on purpose, and the two must stay equal: the
#: untiled arm is then exactly the arm a small crop was upscaled into, which is
#: the one tiling would seam, and a crop that arrives larger than the working
#: resolution is tiled as it always was. It also bounds the untiled arm's cost -
#: a long side of 768 means an area of at most 768², which is the shape
#: :data:`DEFAULT_DECLARED_WORKING_SET` is measured against.
#:
#: The sweep leaves this threshold where it is *and* retires the
#: question it was going to raise. Tiling was measured on the arm above it - a
#: 1024 long side and a ~1MP one, each rendered tiled and untiled - and it moved
#: the result by under half a grey level while costing roughly twice the wall
#: clock (72 s tiled against 25 s untiled at 1024). It neither rescued the large
#: arm nor spoiled it; the large arm is worse than 768 either way. So no render
#: wants to be above this threshold, and its only remaining job is the one it
#: always had: bounding a crop that *arrives* larger than the working resolution.
VAE_TILING_FREE_LONG_SIDE = WORK_LONG_SIDE


def _snap_stride(value: int) -> int:
    """Largest multiple of :data:`LATENT_STRIDE` not exceeding ``value``."""
    value = int(value)
    return max(LATENT_STRIDE, value - (value % LATENT_STRIDE))



def _generate_masked(
    model: Any,
    *,
    seed: int,
    prompt: str,
    steps: int,
    width: int,
    height: int,
    guidance: float,
    image_path: str,
    hole: Any,
) -> Any:
    """`Flux2KleinEdit.generate_image` (mflux 0.18.1), holding the latents outside `hole`.

    The reference image is the crop itself, encoded the way the edit encodes it,
    so its packed latents are the crop's own on the generation grid. The edit's
    KV cache stays off, as it is for Klein 4B in `generate_image` (only the
    9B KV model supports it).
    """
    import mlx.core as mx
    import numpy as np
    from mflux.models.common.config.config import Config
    from mflux.models.flux2.variants.edit.flux2_klein_edit_helpers import _Flux2KleinEditHelpers as helpers
    from mflux.utils.image_util import ImageUtil

    config = Config(
        model_config=model.model_config,
        num_inference_steps=steps,
        height=height,
        width=width,
        guidance=guidance,
        image_path=image_path,
        image_strength=None,
        scheduler="flow_match_euler_discrete",
    )
    prompt_embeds, text_ids, negative_embeds, negative_ids = model._encode_prompt_pair(
        prompt=prompt, negative_prompt=" ", guidance=guidance
    )
    latents, latent_ids, latent_h, latent_w = helpers.prepare_generation_latents(
        seed=seed, height=config.height, width=config.width
    )
    noise = latents
    image_latents, image_latent_ids = helpers.prepare_reference_image_conditioning(
        vae=model.vae,
        tiling_config=model.tiling_config,
        image_paths=[image_path],
        height=config.height,
        width=config.width,
        batch_size=latents.shape[0],
    )
    if image_latents.shape != latents.shape:
        raise RuntimeError(f"reference latents {image_latents.shape} are not the generation grid {latents.shape}")
    # One packed token per 16 by 16 pixel block, row by row, weighted as diffusers
    # weights it: the binarized hole resized bilinearly (no antialias) to the token
    # grid samples each block at its centre, the mean of its middle 2 by 2 pixels.
    hole01 = (np.asarray(hole, dtype=np.uint8)[: latent_h * 16, : latent_w * 16] > 127).astype(np.float32)
    centre = hole01.reshape(latent_h, 16, latent_w, 16)[:, 7:9, :, 7:9].mean(axis=(1, 3))
    keep = mx.array((1.0 - centre).reshape(1, -1, 1)).astype(latents.dtype)

    predict = model._predict(model.transformer)
    sigmas = config.scheduler.sigmas
    for t in config.time_steps:
        predicted = predict(
            latents=latents,
            image_latents=image_latents,
            latent_ids=latent_ids,
            image_latent_ids=image_latent_ids,
            prompt_embeds=prompt_embeds,
            text_ids=text_ids,
            negative_prompt_embeds=negative_embeds,
            negative_text_ids=negative_ids,
            guidance=guidance,
            timestep=config.scheduler.timesteps[t],
        )
        latents = config.scheduler.step(noise=predicted, timestep=t, latents=latents, sigmas=sigmas)
        # The crop as it is at the next step's noise level; the last sigma is 0.
        sigma = sigmas[t + 1]
        # The sigmas are float32: cast back, or every later step runs the
        # transformer in float32 (twice the time, half again the memory).
        latents = (keep * (sigma * noise + (1 - sigma) * image_latents) + (1 - keep) * latents).astype(latents.dtype)
        mx.eval(latents)

    packed = latents.reshape(latents.shape[0], latent_h, latent_w, latents.shape[-1]).transpose(0, 3, 1, 2)
    decoded = model.vae.decode_packed_latents(packed)
    return ImageUtil.to_image(
        decoded_latents=decoded,
        config=config,
        seed=seed,
        prompt=prompt,
        negative_prompt=None,
        quantization=model.bits,
        image_paths=[image_path],
        image_path=config.image_path,
        generation_time=0,
    )

class MfluxBackend(BackendBase):
    """mflux backend managing FLUX.2 Klein 4B with rigorous memory bounds."""

    # Canonical guard names from cleaner-core::sidecar::backend::MFLUX. Two, and
    # both are MLX allocator setters - the two behaviours that used to be here
    # are gone for the reasons in the module docstring, and a guard this backend
    # cannot promise for every render is a guard it must not echo.
    CANONICAL_GUARDS = [
        "mx.set_memory_limit",
        "mx.set_cache_limit",
    ]

    # Accepted synonyms/aliases for required guard matching
    GUARD_ALIASES: Dict[str, str] = {
        "memory_limit": "mx.set_memory_limit",
        "mx.set_memory_limit": "mx.set_memory_limit",
        "cache_limit": "mx.set_cache_limit",
        "mx.set_cache_limit": "mx.set_cache_limit",
    }

    def __init__(self, weights_dir: Optional[str] = None) -> None:
        self.weights_dir = (
            weights_dir
            or os.environ.get("MC_SIDECAR_WEIGHTS_DIR")
            or DEFAULT_WEIGHTS_DIR
        )
        self.model: Optional[Any] = None
        self.active_model_id: str = os.environ.get("MC_SIDECAR_MODEL", "flux2-klein-4b")
        self.basis_state: Basis = Basis.DECLARED
        self.measured_working_set: Optional[int] = None
        self.applied_guards_set: Set[str] = set()

    @property
    def backend_id(self) -> str:
        return "mflux"

    def get_model_metadata(self) -> ModelMetadata:
        return ModelMetadata(
            id=self.active_model_id,
            quantization="int4",
            source="filipstrand/FLUX.2-klein-4bit",
        )

    def get_weights_bytes(self) -> Optional[int]:
        return calculate_dir_size_bytes(self.weights_dir)

    def get_working_set_bytes(self) -> Optional[int]:
        if self.basis_state == Basis.MEASURED and self.measured_working_set is not None:
            return self.measured_working_set

        weights = self.get_weights_bytes()
        if weights is not None:
            # The hardware gate requires working_set_bytes >= weights_bytes
            return max(DEFAULT_DECLARED_WORKING_SET, weights + 1_932_735_284)
        return DEFAULT_DECLARED_WORKING_SET

    def get_basis(self) -> Basis:
        return self.basis_state

    def get_applied_guards(self) -> List[str]:
        if not self.applied_guards_set:
            return []
        # Return all canonical names and their aliases to satisfy client expectations
        return [
            "mx.set_memory_limit",
            "mx.set_cache_limit",
            "memory_limit",
            "cache_limit",
        ]

    def open(
        self,
        model_id: str,
        limits: Dict[str, int],
        require: List[str],
    ) -> Dict[str, Any]:
        """Apply memory limits, validate overrides, and instantiate the model."""
        if not os.path.exists(self.weights_dir):
            raise SidecarError(
                503,
                ErrorKind.WEIGHTS_MISSING,
                f"Weights directory '{self.weights_dir}' does not exist on disk.",
            )

        # Validate that every required guard in the request is known and supported
        for req in require:
            canonical = self.GUARD_ALIASES.get(req)
            if not canonical or canonical not in self.CANONICAL_GUARDS:
                raise SidecarError(
                    422,
                    ErrorKind.BAD_REQUEST,
                    f"Requested memory guard '{req}' cannot be satisfied by backend '{self.backend_id}'.",
                )

        total_bytes = limits.get("total_bytes") or int(
            os.environ.get("MC_SIDECAR_TOTAL_BYTES", 12_000_000_000)
        )
        cache_bytes = limits.get("cache_bytes") or int(
            os.environ.get("MC_SIDECAR_CACHE_BYTES", 1_000_000_000)
        )

        try:
            import mlx.core as mx
            from mflux.models.common.config import ModelConfig
            from mflux.models.flux2.variants import Flux2KleinEdit
        except ImportError as exc:
            raise SidecarError(
                500,
                ErrorKind.INTERNAL,
                f"Failed to import mflux or mlx dependencies: {exc}",
            ) from exc

        # Apply memory constraints on the MLX allocator prior to model loading
        try:
            mx.set_cache_limit(cache_bytes)
            mx.set_memory_limit(total_bytes)
            mx.clear_cache()
            mx.reset_peak_memory()
        except Exception as exc:
            raise SidecarError(
                507,
                ErrorKind.OUT_OF_MEMORY,
                f"Failed to apply MLX allocator limits: {exc}",
            ) from exc

        # mflux 0.18.1 has a live bug: config_resolution.py _create_config rebuilds
        # ModelConfig field by field and DROPS text_encoder_overrides, so resolving
        # by repository name yields {} and Qwen3TextEncoder is built from bare defaults.
        # Always build from ModelConfig.flux2_klein_4b(), and assert text_encoder_overrides ==
        # {"hidden_size": 2560, "intermediate_size": 9728} before loading, raising if empty.
        # Klein 4B survives the bug only by coincidence, because Qwen3 defaults happen
        # to match; 9B would silently get a 2560-wide encoder instead of 4096.
        cfg = ModelConfig.flux2_klein_4b()
        expected_overrides = {"hidden_size": 2560, "intermediate_size": 9728}
        if cfg.text_encoder_overrides != expected_overrides:
            raise SidecarError(
                500,
                ErrorKind.INTERNAL,
                f"Text encoder overrides check failed. Expected {expected_overrides}, "
                f"got {cfg.text_encoder_overrides}. Refusing to load corrupted model configuration.",
            )

        try:
            model = Flux2KleinEdit(model_config=cfg, model_path=self.weights_dir)
            # `tiling_config` is left as the initializer left it - None - and set
            # per render by `_tiling_for`. Registering `MemorySaver` here is what
            # used to set it, and registering it is what used to break the second
            # render; see the module docstring.
            self.model = model
            self.active_model_id = model_id
            self.applied_guards_set = set(self.CANONICAL_GUARDS)
        except MemoryError as exc:
            raise SidecarError(
                507,
                ErrorKind.OUT_OF_MEMORY,
                f"MLX allocator exceeded memory budget during model initialization: {exc}",
            ) from exc
        except Exception as exc:
            err_msg = str(exc).lower()
            if "memory" in err_msg or "allocate" in err_msg or "out of" in err_msg:
                raise SidecarError(
                    507,
                    ErrorKind.OUT_OF_MEMORY,
                    f"MLX allocation failure during model open: {exc}",
                ) from exc
            raise SidecarError(
                500,
                ErrorKind.INTERNAL,
                f"Failed to instantiate FLUX.2 Klein model: {exc}",
            ) from exc

        return {
            "applied": self.get_applied_guards(),
            "weights_bytes": self.get_weights_bytes(),
            "working_set_bytes": self.get_working_set_bytes(),
            "basis": self.basis_state.value,
        }

    @staticmethod
    def _working_size(width: int, height: int) -> Tuple[int, int]:
        """The resolution this crop is actually edited at.

        Upscale only, never downscale: a crop already past
        :data:`WORK_LONG_SIDE` is the model's own working size and shrinking it
        would throw away page the parent decoded on purpose. The result is
        snapped **down** to :data:`LATENT_STRIDE`, which costs at most fifteen
        pixels of scale and buys a size the model will not silently resize.
        """
        long_side = max(int(width), int(height))
        if long_side >= WORK_LONG_SIDE:
            return int(width), int(height)
        scale = WORK_LONG_SIDE / long_side
        return _snap_stride(round(width * scale)), _snap_stride(round(height * scale))

    @staticmethod
    def _tiling_for(work_w: int, work_h: int) -> Optional[Any]:
        """VAE tiling for this working resolution, or ``None`` for none.

        See the module docstring: tiling bounds a large decode and seams a small
        one, so it is a per-render decision rather than a property of the open.
        """
        if max(work_w, work_h) <= VAE_TILING_FREE_LONG_SIDE:
            return None
        from mflux.models.common.vae.tiling_config import TilingConfig

        return TilingConfig()

    def render(
        self,
        image: ImageBuffer,
        hint: Optional[ImageBuffer],
        prompt: str,
        steps: int,
        seed: int,
        guidance: float,
        deadline_ms: int,
    ) -> Tuple[ImageBuffer, int]:
        """Perform diffusion inpainting on the crop and return exact geometry."""
        if self.model is None:
            raise SidecarError(412, ErrorKind.NOT_OPEN, "Model has not been opened.")

        from PIL import Image as PILImage

        # Decode input image to PIL
        pil_crop = image.to_pil()

        # The working resolution. A crop whose long side is already past
        # WORK_LONG_SIDE is edited at its own size; a smaller one is upscaled
        # bicubically, edited, and brought back by an area average - the two
        # named filters, in the two directions, so no resample happens anywhere
        # this file cannot point at.
        work_w, work_h = self._working_size(image.width, image.height)
        work_crop = (
            pil_crop
            if (work_w, work_h) == (pil_crop.width, pil_crop.height)
            else pil_crop.resize((work_w, work_h), PILImage.Resampling.BICUBIC)
        )
        self.model.tiling_config = self._tiling_for(work_w, work_h)

        # Write crop to a temporary file because mflux generate_image requires image_paths
        tmp_file = tempfile.NamedTemporaryFile(suffix=".png", delete=False)
        tmp_path = tmp_file.name
        tmp_file.close()

        start_time = time.time()
        try:
            work_crop.save(tmp_path, format="PNG")

            out = _generate_masked(
                self.model,
                seed=int(seed),
                prompt=str(prompt),
                steps=int(steps),
                height=work_h,
                width=work_w,
                guidance=float(guidance),
                image_path=tmp_path,
                hole=hole_for(hint, image.width, image.height, work_w, work_h),
            )
            result_pil = out.image
            if result_pil.size != (image.width, image.height):
                result_pil = result_pil.resize(
                    (image.width, image.height), PILImage.Resampling.BOX
                )
        except MemoryError as exc:
            raise SidecarError(
                507,
                ErrorKind.OUT_OF_MEMORY,
                f"MLX out of memory during diffusion inference: {exc}",
            ) from exc
        except Exception as exc:
            err_msg = str(exc).lower()
            if "memory" in err_msg or "allocate" in err_msg or "limit" in err_msg:
                raise SidecarError(
                    507,
                    ErrorKind.OUT_OF_MEMORY,
                    f"MLX allocation cap fired during render: {exc}",
                ) from exc
            raise SidecarError(
                500,
                ErrorKind.INTERNAL,
                f"Diffusion inference failed: {exc}",
            ) from exc
        finally:
            if os.path.exists(tmp_path):
                try:
                    os.unlink(tmp_path)
                except OSError:
                    pass
            # What `MemorySaver.call_after_loop` used to do on the way out, done
            # here instead: without a cache sweep between renders MLX keeps every
            # freed buffer from every denoising pass, and this rung renders one
            # region after another through one open (the cap is the second
            # guard; this is the sweep the cap does not replace).
            gc.collect()
            try:
                import mlx.core as mx

                mx.clear_cache()
            except Exception:
                pass

        elapsed_ms = int((time.time() - start_time) * 1000)

        # Update basis to measured working set.
        #
        # This must be the physical footprint and not the resident set size.
        # The working set is what the hardware gate admits this process onto a
        # machine with, so understating it is the one direction that is not
        # safe, and `ru_maxrss` understates a Metal workload badly: it counts
        # no unified-memory buffer at all. Measured on an Apple M5, a 512²
        # render peaked at 6.58 GB of footprint against 2.88 GB of resident,
        # and a 1024² render at 11.46 GB against 2.90 GB - the resident
        # reading barely moved while the true cost went up by 4.9 GB. Writing
        # that figure here replaced an honest declaration of 6.87 GB with a
        # "measured" 2.88 GB and stamped it Basis.MEASURED, which is a
        # downgrade wearing the word measured.
        peak_footprint = get_peak_phys_footprint_bytes()
        self.measured_working_set = (
            peak_footprint if peak_footprint is not None else get_peak_rss_bytes()
        )
        self.basis_state = Basis.MEASURED

        # Convert back to raw sample ImageBuffer maintaining exact request geometry
        out_buffer = ImageBuffer.from_pil(
            image=result_pil,
            target_width=image.width,
            target_height=image.height,
            encoding=image.encoding,
        )

        return out_buffer, elapsed_ms

    def release(self) -> None:
        """Drop model references, force garbage collection, and clear MLX cache."""
        self.model = None
        self.applied_guards_set.clear()

        gc.collect()
        try:
            import mlx.core as mx
            mx.clear_cache()
        except Exception:
            pass


def importable() -> bool:
    """Whether this virtual environment can run this backend.

    Checked without importing MLX, which costs seconds:
    :func:`importlib.util.find_spec` reads the metadata and stops. A venv holding
    only the other backend's dependencies answers ``False`` here and is not
    thereby "not an install" - see `backend/__init__.py`.
    """
    import importlib.util

    for module in ("mlx", "mflux"):
        try:
            if importlib.util.find_spec(module) is None:
                return False
        except (ImportError, ValueError):
            return False
    return True
