"""Pinned Recipe and Model Manifest Plumbing.

Maintains verified manifests, pinned model snapshots, and integrity checking
without downloading weights or incurring GPU execution during offline testing.
"""

from __future__ import annotations

import dataclasses
import hashlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Set, Tuple, Union

from deploy.cloud.common.contract import (
    PROTOCOL_VERSION,
    ContractValidationError,
    ModelInfoResponse,
    RenderRecipe,
    ServiceLimits,
    provisional_fixture_limits,
)

DEFAULT_PREPROCESSING_VERSION: str = "1.0.0"
# The client-side preprocessing the production recipes pin. Crop, hint and composite
# are prepared by the client (crates/cleaner-core/src/engines/render.rs,
# PREPROCESSING_VERSION there), and a gateway advertising this version is what moves
# clients onto it; a client that does not know the version refuses the render.
# 2.0.0: the stored lettering is the hole, up to 128 px of context per side (less
# when the crop would exceed the render limits), and the returned tone is aligned to
# the page around the edit. deploy/cloud/tests/
# test_recipe_parity.py holds this equal to the Rust constant.
PROD_PREPROCESSING_VERSION: str = "2.0.0"
# 3.0.0: 2.0.0's crop and hint, and a composite that matches an edit model's whole
# redrawn crop to the page (tone, lighting, the page's own screentone) before it is
# written (engines/postprocess.rs, EDIT_PREPROCESSING_VERSION there). The FLUX 3 edit
# recipe pins it; the recipes already deployed keep 2.0.0.
PROD_EDIT_PREPROCESSING_VERSION: str = "3.0.0"

# Standard test/fixture IDs
RECIPE_TEST_SDNQ: str = "test-sdnq-v1"
MODEL_TEST_FLUX: str = "test-flux-schnell"
REVISION_TEST_FLUX: str = "0123456789abcdef0123456789abcdef01234567"

# Production recipe. The local engine is the single source of truth for every value
# below: crates/cleaner-core/src/engines/flux.rs holds PROMPT, STEPS, SEED and
# GUIDANCE, and sidecar/manga_cleaner_sidecar/backend/sdnq.py holds WORK_LONG_SIDE,
# LATENT_STRIDE and (through backend/base.py) HOLE_GROWTH.
# deploy/cloud/tests/test_recipe_parity.py parses those files so a change on either
# side fails the suite instead of silently drifting. "inpaint": since 2026-10-01 both
# sides redraw only the hint's lettering (common/flux.py), where the "edit" recipes
# redrew the whole crop.
# v2 (2026-10-18, cloud only): the worker fills the hole with LaMa before Klein runs and
# passes no second reference, and the request may carry guidance (`qwen_edit`). Klein's
# inpaint pipeline conditions on the image it is given, so v1 drew bold sound effects
# back into the hole (docs/findings.md, "Cloud Klein v2").
RECIPE_PROD_SDNQ: str = "mc-flux2-klein-inpaint-v2"
MODEL_PROD_FLUX: str = "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic"
REVISION_PROD_FLUX: str = "45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd"
MODEL_PROD_FLUX_9B: str = "Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32"
REVISION_PROD_FLUX_9B: str = "94ef5985edc982fa93f87df6e1c0faf94d9e9004"
RECIPE_PROD_SDNQ_9B: str = "mc-flux2-klein-9b-inpaint-v2"

# Qwen-Image-Edit-2511, also on the local sidecar. The 4-bit SDNQ build of the
# base model plus the lightx2v Lightning LoRA, which distills 40 steps with true CFG
# down to 8 steps without CFG. docs/research/qwen-image-edit-2511-cloud-plan.md holds
# the benchmark that chose every value below. v2 adds the FukidashiErase LoRA and its
# prompt (docs/research/manga-inpaint-models.md).
# v3 (2026-10-02): a render that leaves the lettering untouched is retried with the next
# seed (common/qwen.py EDIT_RETRIES). v4 adds request-bound target/description, the
# matching 8-step adapter, and a failed job when no seed passes the edit check.
RECIPE_PROD_QWEN: str = "mc-qwen-image-edit-2511-v4"
MODEL_PROD_QWEN: str = "Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32"
REVISION_PROD_QWEN: str = "a285fceef1439d72d60533e83cb6d8921748a666"
# FLUX 3 Image Klein (BFL, 2026-10-15), also on the local sidecar: OzzyGT's ungated 4-bit SDNQ build of the
# step-distilled model, with its Qwen3-VL-4B text encoder in 4 bit too. An edit of the crop
# with its lettering in it, not an inpaint (common/flux3.py); its own GPU image
# (deploy/cloud/modal/app.py), because it needs Diffusers 0.42.
# v2 (2026-10-30): lettering the edit left in place is edited again from a LaMa fill
# (common/flux3.py), so the LaMa adapter is seeded beside the snapshot.
RECIPE_PROD_FLUX3: str = "mc-flux3-klein-edit-v2"
MODEL_PROD_FLUX3: str = "OzzyGT/Flux_3_Image_Klein_sdnq_dynamic_4bit"
REVISION_PROD_FLUX3: str = "bf0af016a936bfe422eec96557aee058c3a6174e"
# The same model in BFL's own bf16 weights, the recipe otherwise unchanged. BFL's repository
# is gated; unsloth's copy is not, and at this revision every file pinned below has the same
# SHA-256 as black-forest-labs/flux-3-image-klein at 337337621b8526e120f2275d250242f4a6bedd75
# (checked on the Hub, 2026-10-26). Bench 4 (docs/research/flux3-klein-5_6b.md) measured it.
RECIPE_PROD_FLUX3_BF16: str = "mc-flux3-klein-bf16-edit-v2"
MODEL_PROD_FLUX3_BF16: str = "unsloth/flux-3-image-klein"
REVISION_PROD_FLUX3_BF16: str = "67662613c10c92a1364cb3da8c6f7447977c7ce9"
LIGHTNING_REPO: str = "lightx2v/Qwen-Image-Edit-2511-Lightning"
LIGHTNING_REVISION: str = "d74eba145674fd7e31b949324e148e21e7118abd"
LIGHTNING_FILE: str = "Qwen-Image-Edit-2511-Lightning-8steps-V1.0-bf16.safetensors"
LIGHTNING_BYTES: int = 849608296
# A manga balloon-text removal LoRA for 2511 (Apache-2.0), rank 1, loaded beside Lightning.
FUKIDASHI_REPO: str = "tori29umai/QwenImageEdit2511_LoRA"
FUKIDASHI_REVISION: str = "3ca05a1ece5606d4de68ebc564d46713fd77e549"
FUKIDASHI_FILE: str = "QIE2511_FukidashiErase_V1_dim4_1e-3-000080_dim1.safetensors"
FUKIDASHI_BYTES: int = 18753872
# The app's own rung-2 inpainter (scripts/fetch-models.sh), which fills the hole before
# a Klein render: big-LaMa finetuned on manga, ONNX, fixed 512 x 512 input.
LAMA_REPO: str = "mayocream/lama-manga-onnx"
LAMA_REVISION: str = "b55497aadbfcb9740e1ed16f008268d71b4f3f79"
LAMA_FILE: str = "lama-manga.onnx"
LAMA_BYTES: int = 207482644
# The directory of the snapshot the LoRA (and LaMa) is downloaded into.
ADAPTER_DIR: str = "adapters"

# The prompt belongs to the recipe on the server and never travels on the wire.
RECIPE_PROMPT: str = (
    "Remove all text, including hand-drawn Japanese sound effects and onomatopoeia. Preserve "
    "character line art, screentones, panel borders, and background details exactly as they "
    "appear. Maintain the original contrast and shading, leaving every area where text or a "
    "sound effect was completely blank."
)
# The FukidashiErase training prompt, changed to keep the balloon and to name sound
# effects. Its own prompt erases the balloon too; the 2026-09-30 prompt, which asked to
# continue the "colour", painted peach blobs on black-and-white pages
# (manga-inpaint-models.md, Benchmark 1).
QWEN_PROMPT: str = (
    "Remove the text and dialogue inside the speech bubbles of a manga illustration, and all sound "
    "effect lettering. Keep the speech bubbles and their outlines. Do not alter the character's line "
    "art, facial expression, or pose in any way."
)
# FLUX 3 Image Klein edits the crop with its lettering in it (common/flux3.py), so the
# prompt says where the lettering is: {location} is the hint's place in the crop in words
# (flux.hint_location). The wording is the one the user brought from another manga tool
# on 2026-10-26, less ", including black or shaded fills" after "the original flat fill":
# with it, FLUX 3 painted white balloons black (2 of 17 crops at seed 1, and 1 of 12 on an
# earlier probe); without it, none of 80 renders on four seeds did
# (docs/research/flux3-klein-5_6b.md, bench 3).
FLUX3_PROMPT: str = (
    "Remove all text in the {location} target area of this manga crop, including small Japanese "
    "characters, furigana, hand-drawn sound effects and onomatopoeia. Remove every lettering stroke, "
    "outline and text shadow so the target is completely free of writing. Preserve character line "
    "art, screentones, panel borders and surrounding background details exactly as they appear. "
    "Reconstruct only the background hidden behind the removed lettering, continuing existing lines "
    "and textures. Inside speech bubbles, restore the original flat fill. Match the original "
    "contrast, shading, grain and line weight. Preserve the crop framing and text outside the target "
    "area. Add no lettering, symbols, objects or colours to monochrome artwork."
)
# Manga Cleaner Reproduction's prompt (domain.py DEFAULT_PROMPT), for a request whose
# flux3_generation asks for location_prompt. Its {location} says "centre".
LOCATION_FLUX3_PROMPT: str = (
    "Remove the selected lettering in the {location} area, including hand-drawn sound effects, "
    "every black or white stroke, outline and shadow. The selected bold shapes are lettering, "
    "not objects or character artwork. Reconstruct only the background hidden by this lettering, "
    "continuing existing contours and matching the surrounding fills, shading and fine grain. "
    "Keep flat areas flat; reproduce screentones only where they already exist. Preserve all "
    "unselected artwork, panel borders, text and crop framing. Add no objects, lettering or new textures."
)
# Its marker sentence (inference.py prepare_job), added to either prompt when the
# lettering is painted magenta.
MARKER_FLUX3_PROMPT: str = (
    " The solid magenta paint marks the exact edit selection. Remove the painted shapes "
    "and repair their background; no magenta or lettering may remain there. "
    "Preserve all unmarked text and artwork."
)
# Crops whose long side is below this are upscaled before inference, as locally.
WORK_LONG_SIDE: int = 768


@dataclass(frozen=True)
class PinnedSampling:
    """Sampling values a recipe fixes. A request that differs is not the same render."""

    seed: int
    steps: int
    guidance_scaled: int


# Wire values: guidance is sent times 100 (GUIDANCE 1.0 -> 100) and divided back on the server.
PINNED_SAMPLING: Dict[str, PinnedSampling] = {
    RECIPE_PROD_SDNQ: PinnedSampling(seed=1, steps=4, guidance_scaled=100),
    RECIPE_PROD_SDNQ_9B: PinnedSampling(seed=1, steps=4, guidance_scaled=100),
    # Lightning runs without CFG: guidance 1.0 is true_cfg_scale 1.0.
    RECIPE_PROD_QWEN: PinnedSampling(seed=1, steps=8, guidance_scaled=100),
    # The distilled flow map at BFL's 16 steps; it samples without CFG.
    RECIPE_PROD_FLUX3: PinnedSampling(seed=1, steps=16, guidance_scaled=100),
    RECIPE_PROD_FLUX3_BF16: PinnedSampling(seed=1, steps=16, guidance_scaled=100),
}

# Files of the pinned snapshot, exactly as the Hub lists them at REVISION_PROD_FLUX.
# The Hub does not publish per-file SHA-256 for the small files, so the seed job checks
# the file set and every size; the commit hash pins the content itself.
PROD_SNAPSHOT_REPO_FILES: Tuple[Tuple[str, int], ...] = (
    (".gitattributes", 1580),
    ("README.md", 2338),
    ("model_index.json", 498),
    ("scheduler/scheduler_config.json", 486),
    ("text_encoder/config.json", 15367),
    ("text_encoder/generation_config.json", 218),
    ("text_encoder/model.safetensors", 2824093272),
    ("text_encoder/quantization_config.json", 13146),
    ("tokenizer/added_tokens.json", 707),
    ("tokenizer/chat_template.jinja", 4168),
    ("tokenizer/merges.txt", 1671853),
    ("tokenizer/special_tokens_map.json", 613),
    ("tokenizer/tokenizer.json", 11422654),
    ("tokenizer/tokenizer_config.json", 5404),
    ("tokenizer/vocab.json", 2776833),
    ("transformer/config.json", 7328),
    ("transformer/diffusion_pytorch_model.safetensors", 2467785416),
    ("transformer/quantization_config.json", 6496),
    ("vae/config.json", 925),
    ("vae/diffusion_pytorch_model.safetensors", 168120878),
)
# The snapshot as seeded: the repository plus LaMa in ADAPTER_DIR.
PROD_SNAPSHOT_FILES: Tuple[Tuple[str, int], ...] = PROD_SNAPSHOT_REPO_FILES + (
    (f"{ADAPTER_DIR}/{LAMA_FILE}", LAMA_BYTES),
)
PROD_SNAPSHOT_TOTAL_BYTES: int = 5475930180 + LAMA_BYTES

# Pinned from the Hugging Face repository tree at REVISION_PROD_FLUX_9B.
PROD_SNAPSHOT_9B_REPO_FILES: Tuple[Tuple[str, int], ...] = (
    (".gitattributes", 1580),
    ("README.md", 2367),
    ("model_index.json", 498),
    ("scheduler/scheduler_config.json", 486),
    ("text_encoder/config.json", 15340),
    ("text_encoder/generation_config.json", 218),
    ("text_encoder/model-00001-of-00002.safetensors", 4988757288),
    ("text_encoder/model-00002-of-00002.safetensors", 1748388808),
    ("text_encoder/model.safetensors.index.json", 107995),
    ("text_encoder/quantization_config.json", 13119),
    ("tokenizer/added_tokens.json", 707),
    ("tokenizer/chat_template.jinja", 4168),
    ("tokenizer/merges.txt", 1671853),
    ("tokenizer/special_tokens_map.json", 613),
    ("tokenizer/tokenizer.json", 11422654),
    ("tokenizer/tokenizer_config.json", 5404),
    ("tokenizer/vocab.json", 2776833),
    ("transformer/config.json", 9774),
    ("transformer/diffusion_pytorch_model-00001-of-00002.safetensors", 4940866448),
    ("transformer/diffusion_pytorch_model-00002-of-00002.safetensors", 743843232),
    ("transformer/diffusion_pytorch_model.safetensors.index.json", 85737),
    ("transformer/quantization_config.json", 8853),
    ("vae/config.json", 925),
    ("vae/diffusion_pytorch_model.safetensors", 168120878),
)
PROD_SNAPSHOT_9B_FILES: Tuple[Tuple[str, int], ...] = PROD_SNAPSHOT_9B_REPO_FILES + (
    (f"{ADAPTER_DIR}/{LAMA_FILE}", LAMA_BYTES),
)
PROD_SNAPSHOT_9B_TOTAL_BYTES: int = 12606105778 + LAMA_BYTES

# Pinned from the Hugging Face repository tree at REVISION_PROD_QWEN.
PROD_SNAPSHOT_QWEN_REPO_FILES: Tuple[Tuple[str, int], ...] = (
    (".gitattributes", 1580),
    ("README.md", 1859),
    ("model_index.json", 564),
    ("processor/added_tokens.json", 605),
    ("processor/chat_template.jinja", 1017),
    ("processor/merges.txt", 1671853),
    ("processor/preprocessor_config.json", 826),
    ("processor/special_tokens_map.json", 613),
    ("processor/tokenizer.json", 11421896),
    ("processor/tokenizer_config.json", 4727),
    ("processor/video_preprocessor_config.json", 910),
    ("processor/vocab.json", 2776833),
    ("scheduler/scheduler_config.json", 485),
    ("text_encoder/config.json", 3847),
    ("text_encoder/generation_config.json", 244),
    ("text_encoder/model.safetensors", 5425540400),
    ("text_encoder/quantization_config.json", 623),
    ("tokenizer/added_tokens.json", 605),
    ("tokenizer/chat_template.jinja", 2427),
    ("tokenizer/merges.txt", 1671853),
    ("tokenizer/special_tokens_map.json", 613),
    ("tokenizer/tokenizer_config.json", 4686),
    ("tokenizer/vocab.json", 3383407),
    ("transformer/config.json", 1315),
    ("transformer/diffusion_pytorch_model-00001-of-00003.safetensors", 4983353856),
    ("transformer/diffusion_pytorch_model-00002-of-00003.safetensors", 4974361320),
    ("transformer/diffusion_pytorch_model-00003-of-00003.safetensors", 1635853208),
    ("transformer/diffusion_pytorch_model.safetensors.index.json", 551086),
    ("transformer/quantization_config.json", 753),
    ("vae/config.json", 900),
    ("vae/diffusion_pytorch_model.safetensors", 253806966),
)
# The LoRAs land inside the snapshot directory, so one marker covers all three.
PROD_SNAPSHOT_QWEN_FILES: Tuple[Tuple[str, int], ...] = PROD_SNAPSHOT_QWEN_REPO_FILES + (
    (f"{ADAPTER_DIR}/{LIGHTNING_FILE}", LIGHTNING_BYTES),
    (f"{ADAPTER_DIR}/{FUKIDASHI_FILE}", FUKIDASHI_BYTES),
)
PROD_SNAPSHOT_QWEN_TOTAL_BYTES: int = 17294421877 + LIGHTNING_BYTES + FUKIDASHI_BYTES

# Pinned from the Hugging Face repository tree at REVISION_PROD_FLUX3. Diffusers reads the
# components from modular_model_index.json; there is no model_index.json.
PROD_SNAPSHOT_FLUX3_FILES: Tuple[Tuple[str, int], ...] = (
    (".gitattributes", 1519),
    ("LICENSE.md", 19593),
    ("NOTICE.md", 330),
    ("README.md", 5666),
    ("modular_model_index.json", 1472),
    ("text_encoder/LICENSE", 11357),
    ("text_encoder/chat_template.json", 5502),
    ("text_encoder/config.json", 37011),
    ("text_encoder/generation_config.json", 242),
    ("text_encoder/merges.txt", 1671839),
    ("text_encoder/model.safetensors", 3062045040),
    ("text_encoder/preprocessor_config.json", 390),
    ("text_encoder/tokenizer.json", 7032403),
    ("text_encoder/tokenizer_config.json", 10868),
    ("text_encoder/video_preprocessor_config.json", 385),
    ("text_encoder/vocab.json", 2776833),
    ("transformer/config.json", 17232),
    ("transformer/diffusion_pytorch_model.safetensors", 3240545888),
    ("vae/config.json", 808),
    ("vae/diffusion_pytorch_model.safetensors", 336213556),
    (f"{ADAPTER_DIR}/{LAMA_FILE}", LAMA_BYTES),
)
PROD_SNAPSHOT_FLUX3_TOTAL_BYTES: int = 6650397934 + LAMA_BYTES

# Pinned from the Hugging Face repository tree at REVISION_PROD_FLUX3_BF16: the folders the
# pipeline loads and the licence. The repository also holds a single-file checkpoint, an
# fp8 and an nvfp4 variant and images (23 GB more), which the seed does not download.
PROD_SNAPSHOT_FLUX3_BF16_FILES: Tuple[Tuple[str, int], ...] = (
    ("LICENSE.md", 19593),
    ("NOTICE.md", 330),
    ("modular_model_index.json", 2115),
    ("text_encoder/LICENSE", 11357),
    ("text_encoder/README.md", 7133),
    ("text_encoder/chat_template.json", 5502),
    ("text_encoder/config.json", 1505),
    ("text_encoder/generation_config.json", 269),
    ("text_encoder/merges.txt", 1671839),
    ("text_encoder/model-00001-of-00002.safetensors", 4967229296),
    ("text_encoder/model-00002-of-00002.safetensors", 3908490048),
    ("text_encoder/model.safetensors.index.json", 64742),
    ("text_encoder/preprocessor_config.json", 390),
    ("text_encoder/tokenizer.json", 7032403),
    ("text_encoder/tokenizer_config.json", 10868),
    ("text_encoder/video_preprocessor_config.json", 385),
    ("text_encoder/vocab.json", 2776833),
    ("transformer/config.json", 436),
    ("transformer/diffusion_pytorch_model-00001-of-00002.safetensors", 9907529352),
    ("transformer/diffusion_pytorch_model-00002-of-00002.safetensors", 1189878064),
    ("transformer/diffusion_pytorch_model.safetensors.index.json", 37951),
    ("vae/config.json", 808),
    ("vae/diffusion_pytorch_model.safetensors", 336213556),
    (f"{ADAPTER_DIR}/{LAMA_FILE}", LAMA_BYTES),
)
PROD_SNAPSHOT_FLUX3_BF16_TOTAL_BYTES: int = 20320984775 + LAMA_BYTES


@dataclass(frozen=True)
class PinnedAdapter:
    """One file from another pinned repository, downloaded into `<snapshot>/ADAPTER_DIR`."""

    repo_id: str
    revision: str
    filename: str
    size_bytes: int


@dataclass(frozen=True)
class ProductionModel:
    """Every per-model fact the deployment needs; nothing else branches on a model id."""

    model_id: str
    revision: str
    recipe_id: str
    files: Tuple[Tuple[str, int], ...]
    total_bytes: int
    license: str
    label: str
    # "flux" (FLUX.2 Klein, the local sdnq recipe), "qwen" (Qwen-Image-Edit + Lightning) or
    # "flux3" (FLUX 3 Image Klein, with its own GPU image).
    runner: str = "flux"
    # GPU a deployment of this model must use, per provider; absent means any allowed GPU.
    required_gpu: Tuple[Tuple[str, str], ...] = ()
    # Host memory of a render worker container holding one pipeline copy.
    worker_memory_gib: int = 12
    adapters: Tuple[PinnedAdapter, ...] = ()
    # Measured peak VRAM of one loaded pipeline while rendering, CUDA context included.
    # None (not measured) keeps the container at one copy (common/capacity.py).
    render_peak_vram_mib: Optional[int] = None
    # Host memory one more copy adds in the same process, measured; None counts a
    # whole worker_memory_gib.
    render_copy_host_gib: Optional[int] = None
    # GPUs on which this model may hold more than one render copy; empty lets the VRAM
    # budget decide on any GPU (common/capacity.py render_copies).
    multi_copy_gpus: Tuple[str, ...] = ()
    # The pinned files are part of the repository, and the seed downloads only them.
    partial_repo: bool = False

    def required_gpu_for(self, provider: str) -> Optional[str]:
        return dict(self.required_gpu).get(provider)


# Seeded beside every Klein snapshot; the FLUX runner fills the hole with it.
LAMA_ADAPTER = PinnedAdapter(LAMA_REPO, LAMA_REVISION, LAMA_FILE, LAMA_BYTES)


PRODUCTION_MODELS: Dict[str, ProductionModel] = {
    MODEL_PROD_FLUX: ProductionModel(MODEL_PROD_FLUX, REVISION_PROD_FLUX, RECIPE_PROD_SDNQ,
                                      PROD_SNAPSHOT_FILES, PROD_SNAPSHOT_TOTAL_BYTES, "Apache-2.0",
                                      label="FLUX.2 Klein 4B (4-bit)",
                                      # Bench on an L4, 2026-10-18 (docs/findings.md): 8,757 MiB in
                                      # nvidia-smi with the LaMa session, 14 real crops up to a
                                      # 1024 px working side. Worker RSS before LaMa (cloud-work-log,
                                      # 2026-09-25): 7,121,456 kB, rounded up to 7 GiB.
                                      render_peak_vram_mib=8757, render_copy_host_gib=7,
                                      adapters=(LAMA_ADAPTER,)),
    MODEL_PROD_FLUX_9B: ProductionModel(MODEL_PROD_FLUX_9B, REVISION_PROD_FLUX_9B,
                                         RECIPE_PROD_SDNQ_9B, PROD_SNAPSHOT_9B_FILES,
                                         PROD_SNAPSHOT_9B_TOTAL_BYTES, "FLUX non-commercial",
                                         label="FLUX.2 Klein 9B (4-bit)",
                                         # One copy with LaMa: 16,540 MiB in nvidia-smi on an L40S,
                                         # 16,207 on an L4 (bench 2026-10-18). A second copy runs
                                         # an L4 out of memory while loading.
                                         required_gpu=(("modal", "L40S"), ("beam", "RTX5090")),
                                         worker_memory_gib=24, multi_copy_gpus=("L40S",),
                                         adapters=(LAMA_ADAPTER,)),
    # Measured on an L40S: 21.3 GiB peak VRAM, 23.6 GiB peak host memory while loading.
    MODEL_PROD_QWEN: ProductionModel(MODEL_PROD_QWEN, REVISION_PROD_QWEN, RECIPE_PROD_QWEN,
                                      PROD_SNAPSHOT_QWEN_FILES, PROD_SNAPSHOT_QWEN_TOTAL_BYTES,
                                      "Apache-2.0", label="Qwen-Image-Edit-2511 (4-bit, Lightning)",
                                      runner="qwen",
                                      required_gpu=(("modal", "L40S"), ("beam", "RTX5090")),
                                      worker_memory_gib=32, render_peak_vram_mib=22016,
                                      # Second copy: the measured 23.6 GiB loading peak, rounded up.
                                      render_copy_host_gib=24, multi_copy_gpus=("L40S",),
                                      adapters=(PinnedAdapter(LIGHTNING_REPO, LIGHTNING_REVISION,
                                                              LIGHTNING_FILE, LIGHTNING_BYTES),
                                                PinnedAdapter(FUKIDASHI_REPO, FUKIDASHI_REVISION,
                                                              FUKIDASHI_FILE, FUKIDASHI_BYTES))),
    # Bench on an L40S, 2026-10-26 (docs/research/flux3-klein-5_6b.md): 8.4 GiB peak allocated
    # by torch on 17 real crops, in a 24 GiB container. Neither nvidia-smi nor the host's peak
    # memory was read, so one copy per container, with the bench's 24 GiB.
    MODEL_PROD_FLUX3: ProductionModel(MODEL_PROD_FLUX3, REVISION_PROD_FLUX3, RECIPE_PROD_FLUX3,
                                       PROD_SNAPSHOT_FLUX3_FILES, PROD_SNAPSHOT_FLUX3_TOTAL_BYTES,
                                       "FLUX Kommunity", label="FLUX 3 Image Klein (4-bit)",
                                       runner="flux3", worker_memory_gib=24, adapters=(LAMA_ADAPTER,)),
    # Bench 4 and 5 on an L40S, 2026-10-26: 21.1 GiB peak allocated by torch at the 768
    # working size, 24.9 GiB at 1280, 19.0 GiB after load; too close to an L4's 24 GB and
    # over it at 1280. Bench 5 ran in a 40 GiB container; the host's peak was not read.
    MODEL_PROD_FLUX3_BF16: ProductionModel(MODEL_PROD_FLUX3_BF16, REVISION_PROD_FLUX3_BF16,
                                            RECIPE_PROD_FLUX3_BF16, PROD_SNAPSHOT_FLUX3_BF16_FILES,
                                            PROD_SNAPSHOT_FLUX3_BF16_TOTAL_BYTES, "FLUX Kommunity",
                                            label="FLUX 3 Image Klein (bf16)", runner="flux3",
                                            required_gpu=(("modal", "L40S"), ("beam", "RTX5090")),
                                            worker_memory_gib=40, partial_repo=True, adapters=(LAMA_ADAPTER,)),
}


def production_model(model_id: str = MODEL_PROD_FLUX) -> ProductionModel:
    try:
        return PRODUCTION_MODELS[model_id]
    except KeyError:
        raise ValueError(f"Unsupported cloud model: {model_id}") from None


# The `model_id` of an installation set up without a cleaning model: it serves the
# analysis graphs and page denoise only, and deploys no render worker.
MODEL_NONE: str = "none"


def deployable_model_id(model_id: Any) -> str:
    """A production model's id or MODEL_NONE; anything else is a ValueError."""
    if model_id == MODEL_NONE:
        return MODEL_NONE
    if not isinstance(model_id, str):
        raise ValueError("model_id is not a supported cloud render model")
    return production_model(model_id).model_id


@dataclass(frozen=True)
class ManifestFileRecord:
    relative_path: str
    sha256: str
    size_bytes: int

    def to_dict(self) -> Dict[str, Any]:
        return dataclasses.asdict(self)


@dataclass(frozen=True)
class ModelManifest:
    model_id: str
    model_revision: str
    recipe_id: str
    preprocessing_version: str
    files: List[ManifestFileRecord]
    total_bytes: int
    native_mask_conditioning: bool = False

    def to_dict(self) -> Dict[str, Any]:
        return {
            "model_id": self.model_id,
            "model_revision": self.model_revision,
            "recipe_id": self.recipe_id,
            "preprocessing_version": self.preprocessing_version,
            "files": [f.to_dict() for f in self.files],
            "total_bytes": self.total_bytes,
            "native_mask_conditioning": self.native_mask_conditioning,
        }


# Canonical pinned test manifest
TEST_FLUX_MANIFEST = ModelManifest(
    model_id=MODEL_TEST_FLUX,
    model_revision=REVISION_TEST_FLUX,
    recipe_id=RECIPE_TEST_SDNQ,
    preprocessing_version=DEFAULT_PREPROCESSING_VERSION,
    files=[
        ManifestFileRecord(
            relative_path="flux1-schnell-sdnq.safetensors",
            sha256="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            size_bytes=5368709120,
        ),
        ManifestFileRecord(
            relative_path="config.json",
            sha256="fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
            size_bytes=1024,
        ),
    ],
    total_bytes=5368710144,
    native_mask_conditioning=False,
)

# Registry of pinned recipes allowed on worker gateways
PINNED_RECIPES: Dict[str, RenderRecipe] = {
    RECIPE_TEST_SDNQ: RenderRecipe(
        recipe_id=RECIPE_TEST_SDNQ,
        preprocessing_version=DEFAULT_PREPROCESSING_VERSION,
        model_id=MODEL_TEST_FLUX,
        model_revision=REVISION_TEST_FLUX,
        native_mask_conditioning=False,
    ),
    RECIPE_PROD_SDNQ: RenderRecipe(
        recipe_id=RECIPE_PROD_SDNQ,
        preprocessing_version=PROD_PREPROCESSING_VERSION,
        model_id=MODEL_PROD_FLUX,
        model_revision=REVISION_PROD_FLUX,
        native_mask_conditioning=False,
    ),
    RECIPE_PROD_SDNQ_9B: RenderRecipe(
        recipe_id=RECIPE_PROD_SDNQ_9B,
        preprocessing_version=PROD_PREPROCESSING_VERSION,
        model_id=MODEL_PROD_FLUX_9B,
        model_revision=REVISION_PROD_FLUX_9B,
        native_mask_conditioning=False,
    ),
    # Same client preprocessing: the worker resizes the crop onto the model's own grid
    # and back, so the crop, hint and composite are the ones Klein gets.
    RECIPE_PROD_QWEN: RenderRecipe(
        recipe_id=RECIPE_PROD_QWEN,
        preprocessing_version=PROD_PREPROCESSING_VERSION,
        model_id=MODEL_PROD_QWEN,
        model_revision=REVISION_PROD_QWEN,
        native_mask_conditioning=False,
    ),
    # Same client preprocessing again: the worker scales the crop up to the working size and
    # back, and the client writes the answer only inside the region's applied mask.
    RECIPE_PROD_FLUX3: RenderRecipe(
        recipe_id=RECIPE_PROD_FLUX3,
        preprocessing_version=PROD_EDIT_PREPROCESSING_VERSION,
        model_id=MODEL_PROD_FLUX3,
        model_revision=REVISION_PROD_FLUX3,
        native_mask_conditioning=False,
    ),
    RECIPE_PROD_FLUX3_BF16: RenderRecipe(
        recipe_id=RECIPE_PROD_FLUX3_BF16,
        preprocessing_version=PROD_EDIT_PREPROCESSING_VERSION,
        model_id=MODEL_PROD_FLUX3_BF16,
        model_revision=REVISION_PROD_FLUX3_BF16,
        native_mask_conditioning=False,
    ),
}

PINNED_MANIFESTS: Dict[str, ModelManifest] = {
    f"{MODEL_TEST_FLUX}@{REVISION_TEST_FLUX}": TEST_FLUX_MANIFEST,
}


def get_pinned_recipe(recipe_id: str) -> Optional[RenderRecipe]:
    """Lookup a pinned recipe by recipe_id."""
    return PINNED_RECIPES.get(recipe_id)


def is_recipe_supported(recipe: RenderRecipe) -> bool:
    """Verify whether a RenderRecipe exactly matches a pinned allowed recipe."""
    pinned = PINNED_RECIPES.get(recipe.recipe_id)
    if pinned is None:
        return False
    return (
        pinned.recipe_id == recipe.recipe_id
        and pinned.preprocessing_version == recipe.preprocessing_version
        and pinned.model_id == recipe.model_id
        and pinned.model_revision == recipe.model_revision
        and pinned.native_mask_conditioning == recipe.native_mask_conditioning
        and recipe.native_mask_conditioning is False
    )


def verify_model_manifest(
    model_dir: Union[str, Path],
    manifest: ModelManifest,
) -> Dict[str, Any]:
    """Verify existing local files against the declared model manifest without downloading.

    Returns a report dictionary:
    {
        "valid": bool,
        "verified_files": list[str],
        "missing_files": list[str],
        "corrupted_files": list[str],
        "total_verified_bytes": int
    }
    """
    dir_path = Path(model_dir)
    verified_files: List[str] = []
    missing_files: List[str] = []
    corrupted_files: List[str] = []
    total_bytes = 0

    if not dir_path.is_dir():
        return {
            "valid": False,
            "verified_files": [],
            "missing_files": [f.relative_path for f in manifest.files],
            "corrupted_files": [],
            "total_verified_bytes": 0,
        }

    for file_rec in manifest.files:
        target_file = dir_path / file_rec.relative_path
        if not target_file.is_file():
            missing_files.append(file_rec.relative_path)
            continue

        stat = target_file.stat()
        if stat.st_size != file_rec.size_bytes:
            corrupted_files.append(file_rec.relative_path)
            continue

        # Compute SHA-256
        hasher = hashlib.sha256()
        with open(target_file, "rb") as f:
            for chunk in iter(lambda: f.read(65536), b""):
                hasher.update(chunk)

        computed_sha = hasher.hexdigest()
        if computed_sha.lower() != file_rec.sha256.lower():
            corrupted_files.append(file_rec.relative_path)
            continue

        verified_files.append(file_rec.relative_path)
        total_bytes += stat.st_size

    is_valid = (len(missing_files) == 0) and (len(corrupted_files) == 0)
    return {
        "valid": is_valid,
        "verified_files": verified_files,
        "missing_files": missing_files,
        "corrupted_files": corrupted_files,
        "total_verified_bytes": total_bytes,
    }


def get_pinned_sampling(recipe_id: str) -> Optional[PinnedSampling]:
    """Sampling values a recipe fixes, or None when the recipe leaves them to the request."""
    return PINNED_SAMPLING.get(recipe_id)


def production_limits() -> ServiceLimits:
    """Limits a deployed gateway advertises.

    Same payload bounds as the fixture limits; the worker deadline matches the provider
    job timeout (600 s) so the client does not give up on a job the worker still runs.
    """
    return dataclasses.replace(provisional_fixture_limits(), worker_timeout_seconds=600)


def get_default_model_info(
    provider: str,
    recipe_id: str = RECIPE_TEST_SDNQ,
    limits: Optional[ServiceLimits] = None,
) -> ModelInfoResponse:
    """Construct a ModelInfoResponse bound to a pinned recipe."""
    recipe = PINNED_RECIPES.get(recipe_id)
    if recipe is None:
        recipe = PINNED_RECIPES[RECIPE_TEST_SDNQ]

    return ModelInfoResponse(
        protocol_version=PROTOCOL_VERSION,
        provider=provider,
        model_id=recipe.model_id,
        model_revision=recipe.model_revision,
        recipe_id=recipe.recipe_id,
        preprocessing_version=recipe.preprocessing_version,
        native_mask_conditioning=False,
        limits=limits or provisional_fixture_limits(),
    )


def get_production_model_info(provider: str, model_id: str = MODEL_PROD_FLUX) -> ModelInfoResponse:
    """Model info a deployed gateway serves on GET /model-info."""
    return get_default_model_info(provider, production_model(model_id).recipe_id, production_limits())
