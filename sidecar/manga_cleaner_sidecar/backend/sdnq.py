"""SDNQ / torch + diffusers backend for FLUX.2 Klein inpainting.

**Why this file exists.** `backend/mflux.py` is MLX, and MLX is Apple Silicon.
Rung 3a reopened for the machine with a discrete GPU, and that machine cannot
run a line of `mflux`. This is the
portable arm: `torch` plus `diffusers`, with `sdnq`'s 4-bit quantized weights,
on CUDA, Intel XPU or Metal.

It is a port of a working implementation rather than a fresh guess -
`mangaTranslator`'s `core/ml/model_manager.py::load_flux_kontext_sdnq` and
`core/image/inpainting.py::FluxKontextInpainter` - and the differences from that
reference are all in one direction: **the mask, the crop, the ramp and the
composite are not this file's business.** They are the parent's
([`cleaner_core::engines::flux`]), so the reference's bounding boxes, distance
transforms and `image_composite_masked` have no counterpart here. What is
ported is the pipeline load, the device and dtype table, the offload guards and
the denoising call.

## What bounds it, and what does not

`torch`'s caching allocator has a **fraction** setter on every accelerator it
runs on - `torch.cuda.set_per_process_memory_fraction`,
`torch.mps.set_per_process_memory_fraction` - and it bounds *reserved* memory,
which is the live allocations and the allocator's own cache together. So the
cache is capped by the same call, not purged in place of being capped, and
rule 9's second guard is met with one knob rather than two.
`torch.*.empty_cache()` is still called between
renders; it is a sweep on top of a cap, exactly as `mx.clear_cache` is on the
MLX arm, and never a substitute for one.

`PYTORCH_MPS_HIGH_WATERMARK_RATIO`
is the ancestor of the MPS call above. Its complaint was never that the guard
was wrong - it was that the guard was aimed at an allocator that was not in the
inference path. Here torch *is* the inference path, and the modern spelling of
that guard is applied to it.

**A machine with no accelerator is refused, not served slowly.** With no CUDA,
XPU or MPS device there is nothing to offload to or from and no fraction to set,
so neither required guard can be applied - and a backend that echoes a guard it
did not apply repeats that same mistake. `open` answers
`501 unbounded_backend`, which the parent reads as a decline and routes to rung
2. A 4B transformer at four bits on a CPU is minutes per step regardless; the
refusal costs nothing anybody wanted.

## The working resolution is this side's, and it is the same number

The crop arrives tight and at the page's own scale - the parent never resamples
- so upscaling
a small crop to something the model can resolve glyphs in is the backend's job
on this arm exactly as it is on the MLX one. :data:`WORK_LONG_SIDE` and
:data:`LATENT_STRIDE` are deliberately the same constants as `mflux.py`'s, and
the filters are the same pair in the same directions, so the two backends differ
in their runtime and not in their framing of the problem.
"""

from __future__ import annotations

import gc
import os
import time
from typing import Any, Dict, List, Optional, Set, Tuple

from ..memory import get_peak_phys_footprint_bytes, get_peak_rss_bytes
from ..protocol import Basis, ErrorKind, ImageBuffer, ModelMetadata, SidecarError
from .base import BackendBase, hole_for

#: The weights root a model id is resolved inside when nothing points elsewhere.
#:
#: The child's working directory is the install root
#: ([`cleaner_core::sidecar::Install::root`]), and `region.rs`'s
#: `list_sidecar_models_from` enumerates `<root>/weights/*` to build the picker.
#: Resolving against the same directory is what makes the id the picker sent
#: name a directory this file can find.
DEFAULT_WEIGHTS_ROOT = "weights"

#: Declared working set, 10 GiB - the same figure `mflux.py` declares, and for
#: the same reason rather than by copying: what a 4-bit Klein 4B costs is the
#: weights it holds resident plus one denoising step's activations plus the VAE's
#: decode, and the runtime underneath does not change that arithmetic much.
#:
#: **Declared, and on this arm not yet measured on the hardware that matters.**
#: The rule is that understating this is the one direction that is not safe,
#: because it is what the hardware gate admits the process onto a machine with.
#: The figure is above the MPS measurement this repository has and above the
#: weights on disk by a wide margin; what nobody here has run is a CUDA machine,
#: where model CPU offload keeps a host-side copy of the weights *and* a VRAM
#: working set, and only the host side is what this number is about. Recorded as
#: a deferral.
DEFAULT_DECLARED_WORKING_SET = 10_737_418_240

#: Long side a small crop is upscaled to before the edit. `mflux.py`'s constant,
#: swept there and not re-swept here: 512 smudges, 1024 invents
#: fabric texture and costs several times the wall clock, and the sweep's
#: subject was the *model*, which is the same model.
WORK_LONG_SIDE = 768

#: The working resolution is snapped down to a multiple of this. Same number as
#: `cleaner_core::engines::flux::LATENT_STRIDE` and `mflux.py`'s, on purpose.
LATENT_STRIDE = 16

#: Canonical name for "one component of the pipeline is on the accelerator at a
#: time". Matches `cleaner_core::sidecar::backend::CPU_OFFLOAD`.
CPU_OFFLOAD = "cpu_offload"

#: Canonical name for the caching allocator's fraction cap. Matches
#: `cleaner_core::sidecar::backend::MEMORY_FRACTION`.
MEMORY_FRACTION = "memory_fraction"


def _snap_stride(value: int) -> int:
    """Largest multiple of :data:`LATENT_STRIDE` not exceeding ``value``."""
    value = int(value)
    return max(LATENT_STRIDE, value - (value % LATENT_STRIDE))


def _is_pipeline_dir(path: str) -> bool:
    """Whether ``path`` is a `diffusers` snapshot: it has a `model_index.json`."""
    return os.path.isfile(os.path.join(path, "model_index.json"))


def _newest_snapshot(path: str) -> Optional[str]:
    """The newest `snapshots/<revision>` under a HuggingFace cache directory.

    The reference implementation lets `huggingface_hub` do this by passing a
    repository id and a `cache_dir`. This process must never fetch anything, so
    it reads the cache layout directly instead: a
    `models--Org--Repo/snapshots/<sha>/` holding symlinks into `blobs/`.
    """
    snapshots = os.path.join(path, "snapshots")
    if not os.path.isdir(snapshots):
        return None
    candidates = [
        os.path.join(snapshots, name)
        for name in os.listdir(snapshots)
        if _is_pipeline_dir(os.path.join(snapshots, name))
    ]
    if not candidates:
        return None
    return max(candidates, key=os.path.getmtime)


def resolve_pipeline_dir(path: str) -> Optional[str]:
    """The `diffusers` snapshot inside ``path``, or ``None``.

    Three layouts, all of which occur in practice:

    1. ``path`` is the snapshot - `model_index.json` sits in it. What a user
       gets from `git clone` or from an unpacked release.
    2. ``path`` is a HuggingFace cache entry - `models--Org--Repo/` with
       `snapshots/<sha>/` inside. What `huggingface-cli download` leaves, and
       the layout the reference implementation's own `models/flux/` has.
    3. ``path`` holds exactly one of either, one level down. What a weights
       directory looks like when somebody dropped a cache entry into it.
    """
    if not os.path.isdir(path):
        return None
    if _is_pipeline_dir(path):
        return path
    snapshot = _newest_snapshot(path)
    if snapshot is not None:
        return snapshot
    nested = []
    for name in sorted(os.listdir(path)):
        child = os.path.join(path, name)
        if not os.path.isdir(child) or name.startswith("."):
            continue
        if _is_pipeline_dir(child) or _newest_snapshot(child) is not None:
            nested.append(child)
    if len(nested) == 1:
        return resolve_pipeline_dir(nested[0])
    return None


def find_weights_dir(model_id: str, override: Optional[str] = None) -> Optional[str]:
    """Where this model's weights are, or ``None``.

    ``override`` is honoured first and may be either the snapshot itself or a
    weights root to look the model id up inside - a user who set
    ``MC_SIDECAR_WEIGHTS_DIR`` to one specific model and a user who set it to
    the folder holding several are both being reasonable, and guessing which
    they meant is cheaper than making them find out.
    """
    roots: List[str] = []
    if override:
        direct = resolve_pipeline_dir(override)
        if direct is not None:
            return direct
        roots.append(override)
    roots.append(os.environ.get("MC_SIDECAR_WEIGHTS_ROOT") or DEFAULT_WEIGHTS_ROOT)

    for root in roots:
        if not os.path.isdir(root):
            continue
        entries = [
            name
            for name in sorted(os.listdir(root))
            if os.path.isdir(os.path.join(root, name)) and not name.startswith(".")
        ]
        # The id the picker sent, spelled as the directory is, and then as a
        # HuggingFace cache entry spells the same repository.
        for name in entries:
            if name == model_id or name.replace("models--", "").replace("--", "/") == model_id:
                found = resolve_pipeline_dir(os.path.join(root, name))
                if found is not None:
                    return found
        # Then a directory whose name contains the id, which is what a cache
        # entry for `Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic` looks like when
        # the picker derived `flux2-klein-4b` from something else.
        lowered = model_id.lower().replace("_", "-").replace(".", "")
        for name in entries:
            if lowered and lowered in name.lower().replace("_", "-").replace(".", ""):
                found = resolve_pipeline_dir(os.path.join(root, name))
                if found is not None:
                    return found
        # And last, the root itself or a single candidate inside it.
        found = resolve_pipeline_dir(root)
        if found is not None:
            return found
    return None


def weights_bytes_of(path: Optional[str]) -> Optional[int]:
    """Bytes on disk under ``path``, following symlinks, counting inodes once.

    :func:`..memory.calculate_dir_size_bytes` deliberately skips symlinks, which
    is right for a directory of real files and answers **zero** for a
    HuggingFace snapshot - every file in one is a symlink into `blobs/`. The
    weights figure is the floor the hardware gate refuses below, so answering
    zero for the ordinary layout would be a gate that never fires.
    """
    if not path or not os.path.exists(path):
        return None
    if os.path.isfile(path):
        return os.path.getsize(path)
    total = 0
    seen: Set[Tuple[int, int]] = set()
    for root, _, files in os.walk(path):
        for name in files:
            try:
                stat = os.stat(os.path.join(root, name))
            except OSError:
                continue
            key = (stat.st_dev, stat.st_ino)
            if key in seen:
                continue
            seen.add(key)
            total += stat.st_size
    return total or None


class SdnqBackend(BackendBase):
    """`torch` + `diffusers` + `sdnq`, on CUDA, XPU or Metal."""

    # Canonical guard names from cleaner-core::sidecar::backend::SDNQ. Two, and
    # both are things this backend does on every render for every model: the
    # offload hook is installed at open and stays installed, and the fraction
    # is a property of the allocator rather than of a call site. A guard that
    # cannot be promised for every render must not be echoed, and
    # neither `vae_tiling` nor `text_encoder_eviction` clears that bar here -
    # see `get_applied_guards`.
    CANONICAL_GUARDS = [CPU_OFFLOAD, MEMORY_FRACTION]

    GUARD_ALIASES: Dict[str, str] = {
        CPU_OFFLOAD: CPU_OFFLOAD,
        "model_cpu_offload": CPU_OFFLOAD,
        "sequential_cpu_offload": CPU_OFFLOAD,
        "enable_model_cpu_offload": CPU_OFFLOAD,
        MEMORY_FRACTION: MEMORY_FRACTION,
        "set_per_process_memory_fraction": MEMORY_FRACTION,
        "torch.cuda.set_per_process_memory_fraction": MEMORY_FRACTION,
        "torch.mps.set_per_process_memory_fraction": MEMORY_FRACTION,
        "PYTORCH_MPS_HIGH_WATERMARK_RATIO": MEMORY_FRACTION,
    }

    def __init__(self, weights_dir: Optional[str] = None) -> None:
        self.weights_dir_override = weights_dir or os.environ.get("MC_SIDECAR_WEIGHTS_DIR")
        self.active_model_id: str = os.environ.get("MC_SIDECAR_MODEL", "flux2-klein-4b")
        self.weights_dir: Optional[str] = find_weights_dir(
            self.active_model_id, self.weights_dir_override
        )
        self.pipeline: Optional[Any] = None
        self.device: Optional[str] = None
        self.basis_state: Basis = Basis.DECLARED
        self.measured_working_set: Optional[int] = None
        self.applied_guards_set: Set[str] = set()
        #: The concrete calls, spelled as the runtime spells them, so the log
        #: names what actually ran and not only the neutral contract name.
        self.applied_calls: List[str] = []
        self.sequential_offload = False

    @property
    def backend_id(self) -> str:
        return "sdnq"

    # -- device and dtype ------------------------------------------------

    @staticmethod
    def pick_device() -> Tuple[str, Any]:
        """The device to run on and the dtype to run in.

        `core/device.py`'s table, unchanged: CUDA > XPU > MPS > CPU, with
        bfloat16 on the first two, float16 on Metal - which has no bfloat16 -
        and float32 on the CPU, which has no accelerator and is refused at
        :meth:`open` anyway.
        """
        import torch

        if torch.cuda.is_available():
            return "cuda", torch.bfloat16
        if hasattr(torch, "xpu") and torch.xpu.is_available():
            return "xpu", torch.bfloat16
        if torch.backends.mps.is_available():
            return "mps", torch.float16
        return "cpu", torch.float32

    @staticmethod
    def _accelerator_total_bytes(device: str) -> Optional[int]:
        """What the accelerator has, for the fraction the cap is expressed in."""
        import torch

        try:
            if device == "cuda":
                return int(torch.cuda.get_device_properties(0).total_memory)
            if device == "xpu":
                return int(torch.xpu.get_device_properties(0).total_memory)
            if device == "mps":
                return int(torch.mps.recommended_max_memory())
        except Exception:
            return None
        return None

    def _apply_memory_fraction(self, device: str, total_bytes: int) -> Optional[str]:
        """Cap the caching allocator, and return the call's own name.

        The wire carries a byte count and torch takes a fraction of the device's
        own capacity, so the conversion happens here. A budget at or above what
        the device has becomes 1.0 rather than being skipped: the cap is still
        applied, it is simply not the binding constraint, and skipping it would
        make the echo depend on the machine.
        """
        import torch

        capacity = self._accelerator_total_bytes(device)
        if capacity is None or capacity <= 0:
            return None
        fraction = min(1.0, max(0.05, total_bytes / capacity))
        try:
            if device == "cuda":
                torch.cuda.set_per_process_memory_fraction(fraction)
                return "torch.cuda.set_per_process_memory_fraction"
            if device == "mps":
                os.environ["PYTORCH_MPS_HIGH_WATERMARK_RATIO"] = str(fraction)
                return "PYTORCH_MPS_HIGH_WATERMARK_RATIO"
            if device == "xpu" and hasattr(torch.xpu, "set_per_process_memory_fraction"):
                torch.xpu.set_per_process_memory_fraction(fraction)
                return "torch.xpu.set_per_process_memory_fraction"
        except Exception:
            return None
        return None

    def _empty_cache(self) -> None:
        """Sweep the caching allocator. A sweep on top of a cap, never instead."""
        try:
            import torch

            if self.device == "cuda":
                torch.cuda.empty_cache()
            elif self.device == "mps":
                torch.mps.empty_cache()
            elif self.device == "xpu" and hasattr(torch.xpu, "empty_cache"):
                torch.xpu.empty_cache()
        except Exception:
            pass

    # -- reporting -------------------------------------------------------

    def get_model_metadata(self) -> ModelMetadata:
        source = os.path.basename(self.weights_dir) if self.weights_dir else None
        if self.weights_dir and "models--" in self.weights_dir:
            # Recover the repository name a cache entry was created from, which
            # is the provenance a reviewer wants rather than a content hash.
            for part in self.weights_dir.split(os.sep):
                if part.startswith("models--"):
                    source = part[len("models--") :].replace("--", "/")
                    break
        return ModelMetadata(
            id=self.active_model_id,
            quantization="int4",
            source=source or "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic",
        )

    def get_weights_bytes(self) -> Optional[int]:
        return weights_bytes_of(self.weights_dir)

    def get_working_set_bytes(self) -> Optional[int]:
        if self.basis_state == Basis.MEASURED and self.measured_working_set is not None:
            return self.measured_working_set
        weights = self.get_weights_bytes()
        if weights is not None:
            # The gate refuses a working set below the weights it sits on top of.
            return max(DEFAULT_DECLARED_WORKING_SET, weights + 1_932_735_284)
        return DEFAULT_DECLARED_WORKING_SET

    def get_basis(self) -> Basis:
        return self.basis_state

    def get_applied_guards(self) -> List[str]:
        """What was applied, canonical names first and concrete calls after.

        **Two names, and two that are deliberately absent.** `vae_tiling` is not
        echoed because `diffusers` tiles above its VAE's own sample threshold,
        which is under :data:`WORK_LONG_SIDE` - so switching it on would seam
        exactly the resolution a small crop was upscaled into, which is the
        measurement that withdrew the same guard from the MLX arm.
        `text_encoder_eviction` is not echoed as a *separate* guard because the
        offload hook is what performs it: `diffusers` moves the encoder back to
        the host after encoding and moves it out again next time, so the
        behaviour is real but it is not a second promise - it is this one.
        """
        if not self.applied_guards_set:
            return []
        return list(self.CANONICAL_GUARDS) + list(self.applied_calls)

    # -- lifecycle -------------------------------------------------------

    def open(
        self,
        model_id: str,
        limits: Dict[str, int],
        require: List[str],
    ) -> Dict[str, Any]:
        """Resolve the weights, bound the allocator, and load the pipeline."""
        self.active_model_id = model_id or self.active_model_id
        self.weights_dir = find_weights_dir(self.active_model_id, self.weights_dir_override)
        if self.weights_dir is None:
            raise SidecarError(
                503,
                ErrorKind.WEIGHTS_MISSING,
                f"No diffusers snapshot for model '{self.active_model_id}' under "
                f"'{self.weights_dir_override or os.path.abspath(DEFAULT_WEIGHTS_ROOT)}'. "
                "Expected a directory holding model_index.json, or a HuggingFace "
                "cache entry (models--Org--Repo/snapshots/<sha>/).",
            )

        for req in require:
            canonical = self.GUARD_ALIASES.get(req)
            if not canonical or canonical not in self.CANONICAL_GUARDS:
                raise SidecarError(
                    422,
                    ErrorKind.BAD_REQUEST,
                    f"Requested memory guard '{req}' cannot be satisfied by backend "
                    f"'{self.backend_id}'.",
                )

        try:
            import torch  # noqa: F401
            # Importing `sdnq` is what registers `SDNQConfig` with diffusers'
            # quantization loader, so it must happen before `from_pretrained`
            # reads a `quantization_config.json` naming it.
            import sdnq  # noqa: F401
            from diffusers import Flux2KleinInpaintPipeline
        except ImportError as exc:
            raise SidecarError(
                500,
                ErrorKind.INTERNAL,
                "Failed to import the sdnq backend's dependencies "
                f"(torch, diffusers, sdnq): {exc}. Install "
                "sidecar/requirements-sdnq.txt into this virtual environment.",
            ) from exc

        device, dtype = self.pick_device()
        if device == "cpu":
            # Rule 9's first guard, refused from this side rather than faked.
            # With no accelerator there is nothing to offload to and no fraction
            # to set, so neither required guard can be applied - and echoing one
            # that was not is exactly that same failure.
            raise SidecarError(
                501,
                ErrorKind.UNBOUNDED_BACKEND,
                "No CUDA, XPU or Metal device is available, so torch has no "
                "accelerator allocator to bound: neither cpu_offload nor "
                "memory_fraction can be applied, and this backend will not echo "
                "a guard it did not apply. CPU-only diffusion of a 4B "
                "transformer is not offered.",
            )
        self.device = device

        total_bytes = int(
            limits.get("total_bytes")
            or os.environ.get("MC_SIDECAR_TOTAL_BYTES", 12_000_000_000)
        )
        fraction_call = self._apply_memory_fraction(device, total_bytes)
        if fraction_call is None:
            raise SidecarError(
                501,
                ErrorKind.UNBOUNDED_BACKEND,
                f"torch on device '{device}' would not accept a per-process memory "
                "fraction, so the caching allocator cannot be capped in this "
                "runtime's own units.",
            )

        # Sequential offload moves one *submodule* at a time and is much slower;
        # it is taken only when the accelerator plainly cannot hold a component,
        # which on a discrete GPU is a real and common case and on unified
        # memory never happens.
        capacity = self._accelerator_total_bytes(device) or 0
        weights = self.get_weights_bytes() or 0
        env_low = os.environ.get("MC_SIDECAR_LOW_VRAM", "").strip().lower()
        if env_low in ("1", "true", "yes"):
            self.sequential_offload = True
        elif env_low in ("0", "false", "no"):
            self.sequential_offload = False
        else:
            self.sequential_offload = bool(
                device in ("cuda", "xpu") and capacity and weights and capacity < weights * 3 // 2
            )

        try:
            # The snapshot names Flux2KleinPipeline; the inpaint pipeline takes the
            # same modules and reads is_distilled from the same model_index.json.
            pipeline = Flux2KleinInpaintPipeline.from_pretrained(
                self.weights_dir,
                torch_dtype=dtype,
                local_files_only=True,
                low_cpu_mem_usage=True,
            )
        except Exception as exc:
            detail = str(exc)
            if _looks_like_memory(detail):
                raise SidecarError(
                    507,
                    ErrorKind.OUT_OF_MEMORY,
                    f"torch allocation failure while loading the pipeline: {exc}",
                ) from exc
            raise SidecarError(
                500,
                ErrorKind.INTERNAL,
                f"Failed to load the diffusers pipeline from '{self.weights_dir}': {exc}",
            ) from exc

        # Triton INT8 matmul, where there is a Triton to use. CUDA and XPU only;
        # the reference guards it the same way and it is skipped on Metal.
        try:
            from sdnq.common import use_torch_compile as triton_is_available
            from sdnq.loader import apply_sdnq_options_to_model

            if triton_is_available and device in ("cuda", "xpu"):
                for name in ("transformer", "text_encoder_2", "text_encoder"):
                    module = getattr(pipeline, name, None)
                    if module is not None:
                        setattr(
                            pipeline,
                            name,
                            apply_sdnq_options_to_model(module, use_quantized_matmul=True),
                        )
        except Exception:
            # An optimisation, not a guard. Nothing is echoed for it and a
            # runtime without Triton simply runs eager kernels.
            pass

        try:
            if self.sequential_offload:
                pipeline.enable_sequential_cpu_offload(device=device)
                offload_call = "enable_sequential_cpu_offload"
            else:
                pipeline.enable_model_cpu_offload(device=device)
                offload_call = "enable_model_cpu_offload"
        except Exception as exc:
            raise SidecarError(
                501,
                ErrorKind.UNBOUNDED_BACKEND,
                f"diffusers would not install a CPU offload hook on device "
                f"'{device}': {exc}. Without it the whole pipeline sits on the "
                "accelerator at once, which this backend will not do silently.",
            ) from exc

        self.pipeline = pipeline
        self.applied_guards_set = set(self.CANONICAL_GUARDS)
        self.applied_calls = [fraction_call, offload_call]

        return {
            "applied": self.get_applied_guards(),
            "weights_bytes": self.get_weights_bytes(),
            "working_set_bytes": self.get_working_set_bytes(),
            "basis": self.basis_state.value,
        }

    @staticmethod
    def _working_size(width: int, height: int) -> Tuple[int, int]:
        """The resolution this crop is edited at. `mflux.py`'s rule exactly.

        Upscale only, never downscale, and snap **down** to
        :data:`LATENT_STRIDE` so nothing silently resizes a shape the latent
        grid cannot hold.
        """
        long_side = max(int(width), int(height))
        if long_side >= WORK_LONG_SIDE:
            return int(width), int(height)
        scale = WORK_LONG_SIDE / long_side
        return _snap_stride(round(width * scale)), _snap_stride(round(height * scale))

    def _call_kwargs(self, candidate: Dict[str, Any]) -> Dict[str, Any]:
        """``candidate``, less anything this pipeline's `__call__` will not take.

        The reference passes `pooled_prompt_embeds` and `max_area`, which the
        Kontext pipeline accepts and `Flux2KleinPipeline` does not, and a
        `diffusers` upgrade moves that line either way. Filtering against the
        signature means an argument that stops existing is dropped instead of
        raising, and one that appears is passed the moment it does.
        """
        import inspect

        try:
            accepted = inspect.signature(type(self.pipeline).__call__).parameters
        except (TypeError, ValueError):
            return candidate
        if any(p.kind == inspect.Parameter.VAR_KEYWORD for p in accepted.values()):
            return candidate
        return {name: value for name, value in candidate.items() if name in accepted}

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
        """Inpaint the hint's lettering in one crop, at exactly the geometry asked.

        The hole is :func:`hole_for`: the latents outside it are held to the
        crop's own at every step, so the model redraws only the lettering. The
        model takes no mask channel, and the composite is still the parent's
        ([`cleaner_core::engines::flux`]).
        """
        if self.pipeline is None:
            raise SidecarError(412, ErrorKind.NOT_OPEN, "Model has not been opened.")

        import numpy as np
        import torch
        from PIL import Image as PILImage

        pil_crop = image.to_pil()
        work_w, work_h = self._working_size(image.width, image.height)
        work_crop = (
            pil_crop
            if (work_w, work_h) == (pil_crop.width, pil_crop.height)
            else pil_crop.resize((work_w, work_h), PILImage.Resampling.BICUBIC)
        )
        hole = hole_for(hint, image.width, image.height, work_w, work_h)

        # The generator is always on the host. `randn_tensor` accepts a CPU
        # generator for a non-CPU device and seeds identically either way, so
        # one seed means one result whichever accelerator ran it - which is what
        # `flux::SEED` being fixed is *for*.
        generator = torch.Generator(device="cpu").manual_seed(int(seed))
        kwargs = self._call_kwargs(
            {
                "image": work_crop,
                "prompt": str(prompt),
                "width": work_w,
                "height": work_h,
                "num_inference_steps": int(steps),
                "guidance_scale": float(guidance),
                "generator": generator,
                "output_type": "pt",
                "max_area": work_w * work_h,
            }
        )
        # Never filtered: a pipeline without them would redraw the whole crop. The
        # crop is also the reference image, so the model sees the page around the
        # hole (deploy/cloud/common/flux.py makes the same call).
        kwargs.update(mask_image=hole, image_reference=work_crop, strength=1.0)

        start_time = time.time()
        try:
            # **All of it inside `inference_mode`, and none of it in place.** A
            # tensor produced under `inference_mode` is an inference tensor, and
            # torch refuses both an in-place write to one *outside* the block and
            # a version bump on one inside it - so the reference's
            # `nan_to_num_` / `clamp_` pair cannot be transcribed here. The
            # out-of-place forms cost one 768²×3 float buffer and are correct in
            # both places.
            with torch.inference_mode():
                out = self.pipeline(**kwargs)
                tensor = torch.nan_to_num(
                    out.images[0], nan=0.0, posinf=1.0, neginf=0.0
                ).clamp(0.0, 1.0)
                samples = (
                    tensor.float()
                    .mul(255.0)
                    .round()
                    .to(torch.uint8)
                    .permute(1, 2, 0)
                    .cpu()
                    .numpy()
                )
            result_pil = PILImage.fromarray(np.ascontiguousarray(samples), mode="RGB")
            if result_pil.size != (image.width, image.height):
                result_pil = result_pil.resize(
                    (image.width, image.height), PILImage.Resampling.BOX
                )
        except torch.cuda.OutOfMemoryError as exc:  # type: ignore[attr-defined]
            raise SidecarError(
                507,
                ErrorKind.OUT_OF_MEMORY,
                f"torch CUDA allocator cap fired during render: {exc}",
            ) from exc
        except MemoryError as exc:
            raise SidecarError(
                507,
                ErrorKind.OUT_OF_MEMORY,
                f"Host allocation failure during diffusion inference: {exc}",
            ) from exc
        except Exception as exc:
            if _looks_like_memory(str(exc)):
                raise SidecarError(
                    507,
                    ErrorKind.OUT_OF_MEMORY,
                    f"torch allocation cap fired during render: {exc}",
                ) from exc
            raise SidecarError(
                500,
                ErrorKind.INTERNAL,
                f"Diffusion inference failed: {exc}",
            ) from exc
        finally:
            # One region at a time, and the next one gets a swept allocator.
            gc.collect()
            self._empty_cache()

        elapsed_ms = int((time.time() - start_time) * 1000)

        # The physical footprint and not the resident set size, for
        # `memory.py`'s reason: on unified memory a resident reading misses the
        # accelerator's buffers entirely, and this figure is what the hardware
        # gate admits the process onto a machine with.
        peak = get_peak_phys_footprint_bytes()
        self.measured_working_set = peak if peak is not None else get_peak_rss_bytes()
        self.basis_state = Basis.MEASURED

        out_buffer = ImageBuffer.from_pil(
            image=result_pil,
            target_width=image.width,
            target_height=image.height,
            encoding=image.encoding,
        )
        return out_buffer, elapsed_ms

    def release(self) -> None:
        """Drop the pipeline and give the allocator's cache back."""
        pipeline = self.pipeline
        self.pipeline = None
        self.applied_guards_set.clear()
        self.applied_calls = []
        if pipeline is not None:
            for name in ("transformer", "text_encoder", "text_encoder_2", "vae"):
                try:
                    setattr(pipeline, name, None)
                except Exception:
                    pass
            del pipeline
        gc.collect()
        self._empty_cache()


def _looks_like_memory(detail: str) -> bool:
    """Whether a runtime's message is about memory.

    `torch` raises `RuntimeError` for an MPS allocation past the watermark and
    for a CUDA OOM under some builds, so the kind has to be read out of the
    text. Deliberately narrow: a false positive turns a fault into a decline,
    which loses a bug report.
    """
    lowered = detail.lower()
    return any(
        needle in lowered
        for needle in (
            "out of memory",
            "outofmemory",
            "cannot allocate",
            "can't allocate",
            "allocation failed",
            "insufficient memory",
            "watermark",
            "mps backend out of memory",
        )
    )


def importable() -> bool:
    """Whether this virtual environment can run this backend.

    Checked without importing torch, which costs seconds and hundreds of
    megabytes: :func:`importlib.util.find_spec` reads the metadata and stops.
    """
    import importlib.util

    for module in ("torch", "diffusers", "sdnq"):
        try:
            if importlib.util.find_spec(module) is None:
                return False
        except (ImportError, ValueError):
            return False
    return True
