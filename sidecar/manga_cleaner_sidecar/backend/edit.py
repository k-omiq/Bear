"""Qwen-Image-Edit-2511 and FLUX 3 Image Klein on this computer, on torch.

Edit models: no hole and no mask, the prompt names where the lettering is and the
parent's composite is the boundary. They run the cloud worker's recipes
(deploy/cloud/common/qwen.py and flux3.py) on the same pinned weights, so a local
result and a cloud one are the same model asked the same way.
deploy/cloud/tests/test_recipe_parity.py holds the numbers below equal to the worker's.

What differs from the worker:

- The pipeline stays where `SdnqBackend` puts it, under its offload and allocator cap.
  The worker owns a GPU and moves everything onto it.
- Target and description guidance, FLUX 3's generation options and the prompt upsampler
  are not offered: the parent sends one prompt per crop.

torch, diffusers, PIL and numpy are imported when a model loads.
"""

from __future__ import annotations

import math
import os
from typing import Any, Dict, Optional, Tuple

#: Renders after the first when the lettering was not edited, each with the next seed,
#: and how many times more the hint's pixels must change than the rest of the crop to
#: count as an edit.
EDIT_RETRIES = 2
MIN_EDIT_RATIO = 2.0

#: The pipeline's reference area, on its grid (VAE_IMAGE_SIZE in
#: pipeline_qwenimage_edit_plus.py). Any other size moves the answer by several pixels.
VAE_IMAGE_AREA = 1024 * 1024
GRID = 32

#: Where the installer puts the LoRAs inside the Qwen snapshot.
ADAPTER_DIR = "adapters"
LIGHTNING_FILE = "Qwen-Image-Edit-2511-Lightning-8steps-V1.0-bf16.safetensors"
FUKIDASHI_FILE = "QIE2511_FukidashiErase_V1_dim4_1e-3-000080_dim1.safetensors"

#: The scheduler the Lightning LoRA was distilled with (lightx2v model card).
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

#: BFL's `alpha` (src/flux3/pipeline.py generate) since 2026-10-17.
TIME_SHIFT = 16.0
#: One token per this many pixels a side.
FLUX3_STRIDE = 16
FLUX3_WORK_LONG_SIDE = 768


def family_of(model_id: str) -> str:
    """Which pipeline a model id runs on: `qwen`, `flux3`, or `klein` for the rest."""
    lowered = (model_id or "").lower().replace("_", "-")
    if "qwen" in lowered:
        return "qwen"
    if "flux3" in lowered or "flux-3" in lowered:
        return "flux3"
    return "klein"


def qwen_work_size(width: int, height: int) -> Tuple[int, int]:
    """The size the pipeline gives its reference for this aspect ratio (calculate_dimensions)."""
    ratio = int(width) / int(height)
    w = math.sqrt(VAE_IMAGE_AREA * ratio)
    h = w / ratio
    return round(w / GRID) * GRID, round(h / GRID) * GRID


def flux3_work_size(width: int, height: int) -> Tuple[int, int]:
    """Upscale only, snapped down to the token grid. A larger crop is on it already: the
    parent snaps every crop to the same stride."""
    long_side = max(int(width), int(height))
    if long_side >= FLUX3_WORK_LONG_SIDE:
        return int(width), int(height)
    scale = FLUX3_WORK_LONG_SIDE / long_side
    snap = lambda value: max(FLUX3_STRIDE, value - value % FLUX3_STRIDE)  # noqa: E731
    return snap(round(width * scale)), snap(round(height * scale))


def edit_ratio(np: Any, before: Any, after: Any, lettering: Any) -> float:
    """Mean change of the lettering's pixels over the mean change of the rest of the crop."""
    diff = np.abs(np.asarray(after.convert("L"), dtype=np.float32) - np.asarray(before.convert("L"), dtype=np.float32))
    return float(diff[lettering].mean()) / max(float(diff[~lettering].mean()), 1.0)


def load_qwen(weights_dir: str, dtype: Any) -> Any:
    """The pipeline with Lightning and FukidashiErase, kept as PEFT adapters at weight 1.0.

    SDNQ layers hold packed 4-bit weights a fuse cannot write into.
    """
    from diffusers import FlowMatchEulerDiscreteScheduler, QwenImageEditPlusPipeline

    pipeline = QwenImageEditPlusPipeline.from_pretrained(
        weights_dir,
        scheduler=FlowMatchEulerDiscreteScheduler.from_config(LIGHTNING_SCHEDULER),
        torch_dtype=dtype,
        local_files_only=True,
        low_cpu_mem_usage=True,
    )
    adapters = os.path.join(weights_dir, ADAPTER_DIR)
    pipeline.load_lora_weights(adapters, weight_name=LIGHTNING_FILE, adapter_name="lightning")
    pipeline.load_lora_weights(adapters, weight_name=FUKIDASHI_FILE, adapter_name="fukidashi")
    # Loading a second adapter does not by itself keep the first one active.
    pipeline.set_adapters(["lightning", "fukidashi"], [1.0, 1.0])
    pipeline.set_progress_bar_config(disable=True)
    return pipeline


def load_flux3(weights_dir: str, dtype: Any, components_manager: Any = None) -> Any:
    """The modular pipeline less its prompt upsampler and text encoder, the autoencoder in
    float32. `encode_flux3_prompt` makes what the text encoder would have."""
    import torch
    from diffusers import FlowMatchEulerDiscreteScheduler, ModularPipeline

    # Blocks are edited before the pipeline is built: `pipe.blocks` is a copy.
    blocks = ModularPipeline.from_pretrained(weights_dir, local_files_only=True).blocks
    blocks.sub_blocks.pop("prompt_upsampler", None)
    blocks.sub_blocks.pop("text_encoder")
    pipeline = blocks.init_pipeline(weights_dir, components_manager=components_manager)
    pipeline.load_components(dtype={"vae": torch.float32, "default": dtype})
    # load_components logs a component it cannot load and carries on.
    missing = [name for name in ("transformer", "vae")
               if getattr(pipeline, name, None) is None]
    if missing:
        raise RuntimeError(f"components not loaded: {', '.join(missing)}")
    pipeline.update_components(scheduler=FlowMatchEulerDiscreteScheduler(shift=TIME_SHIFT))
    return pipeline


def encode_klein_prompt(weights_dir: str, dtype: Any, device: str, prompt: str) -> Any:
    """Klein's prompt embedding, on the host, from a text encoder that is gone on return."""
    import torch
    from diffusers import Flux2KleinInpaintPipeline

    encoder = Flux2KleinInpaintPipeline.from_pretrained(
        weights_dir,
        transformer=None,
        vae=None,
        torch_dtype=dtype,
        local_files_only=True,
        low_cpu_mem_usage=True,
    ).to(device)
    with torch.inference_mode():
        embeds, _ = encoder.encode_prompt(prompt=prompt, device=device)
    return embeds.to("cpu")


def encode_flux3_prompt(weights_dir: str, dtype: Any, device: str, prompt: str) -> Any:
    """FLUX 3's prompt embedding, on the host, from a text encoder that is gone on return."""
    import torch
    from diffusers import ModularPipeline

    blocks = ModularPipeline.from_pretrained(weights_dir, local_files_only=True).blocks
    encoder = blocks.sub_blocks["text_encoder"].init_pipeline(weights_dir)
    encoder.load_components(dtype=dtype)
    encoder.to(device)
    with torch.inference_mode():
        embeds = encoder(prompt=prompt, output="prompt_embeds")
    return embeds.to("cpu")


def _lettering(hint: Optional[Any], size: Tuple[int, int]) -> Any:
    """The hint as a boolean array, or None when it cannot tell lettering from page."""
    if hint is None:
        return None
    import numpy as np

    lettering = np.asarray(hint.to_pil().convert("L")) > 0
    if lettering.shape != (size[1], size[0]):
        return None
    return lettering if lettering.any() and not lettering.all() else None


def _qwen_once(pipeline: Any, crop: Any, prompt: str, steps: int, seed: int, guidance: float) -> Any:
    """One pipeline call: the edited crop as an RGB image at the crop's size."""
    import numpy as np
    import torch
    from PIL import Image

    width, height = crop.size
    work_w, work_h = qwen_work_size(width, height)
    # Lanczos both ways: almost every crop is stretched, and bicubic in with box out
    # lost about a fifth of the fine tone and line detail (docs/findings.md, 2026-10-19).
    work_crop = crop if crop.size == (work_w, work_h) else crop.resize((work_w, work_h), Image.Resampling.LANCZOS)
    generator = torch.Generator(device="cpu").manual_seed(int(seed))
    with torch.inference_mode():
        out = pipeline(
            image=[work_crop],
            prompt=prompt,
            # True CFG only above 1.0; Lightning is distilled to run without it.
            negative_prompt=" " if guidance > 1.0 else None,
            true_cfg_scale=float(guidance),
            width=work_w,
            height=work_h,
            num_inference_steps=int(steps),
            generator=generator,
            output_type="pt",
        )
        tensor = torch.nan_to_num(out.images[0], nan=0.0, posinf=1.0, neginf=0.0).clamp(0.0, 1.0)
        samples = tensor.float().mul(255.0).round().to(torch.uint8).permute(1, 2, 0).cpu().numpy()
    result = Image.fromarray(np.ascontiguousarray(samples)).convert("RGB")
    return result if result.size == (width, height) else result.resize((width, height), Image.Resampling.LANCZOS)


def render_qwen(pipeline: Any, crop: Any, hint: Optional[Any], prompt: str, steps: int, seed: int, guidance: float) -> Any:
    """Edit one crop, retrying with the next seed while the lettering is untouched.

    If no seed edits it, the render that changed the lettering most comes back: the
    user rejects a result, this does not. A hint that cannot tell lettering from page
    skips the check.
    """
    import numpy as np

    lettering = _lettering(hint, crop.size)
    best: Optional[Tuple[float, Any]] = None
    result = None
    for attempt in range(1 + (EDIT_RETRIES if lettering is not None else 0)):
        result = _qwen_once(pipeline, crop, prompt, steps, int(seed) + attempt, guidance)
        if lettering is None:
            break
        ratio = edit_ratio(np, crop, result, lettering)
        if best is None or ratio > best[0]:
            best = (ratio, result)
        if ratio >= MIN_EDIT_RATIO:
            break
    return best[1] if best is not None else result


def render_flux3(pipeline: Any, crop: Any, prompt_embeds: Any, steps: int, seed: int) -> Any:
    """Edit one crop, sampled the way BFL samples it: 16 steps, no CFG, its time shift.

    The noise is drawn as image tokens on the host and cast to bf16, which reproduces
    BFL's sample for a seed on any device.
    """
    import torch
    from PIL import Image

    width, height = crop.size
    work_w, work_h = flux3_work_size(width, height)
    work_crop = crop if crop.size == (work_w, work_h) else crop.resize((work_w, work_h), Image.Resampling.LANCZOS)
    rows, cols = work_h // FLUX3_STRIDE, work_w // FLUX3_STRIDE
    channels = int(pipeline.transformer.config.in_channels)
    noise = torch.randn((rows * cols, channels), generator=torch.Generator().manual_seed(int(seed))).to(torch.bfloat16)
    with torch.inference_mode():
        images = pipeline(
            prompt_embeds=prompt_embeds,
            image=work_crop,
            latents=noise.T.reshape(1, channels, rows, cols),
            height=work_h,
            width=work_w,
            num_inference_steps=int(steps),
            output="images",
        )
    result = images[0].convert("RGB")
    return result if result.size == (width, height) else result.resize((width, height), Image.Resampling.LANCZOS)
