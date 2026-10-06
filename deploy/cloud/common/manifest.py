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
RECIPE_PROD_SDNQ: str = "mc-flux2-klein-inpaint-v1"
MODEL_PROD_FLUX: str = "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic"
REVISION_PROD_FLUX: str = "45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd"
MODEL_PROD_FLUX_9B: str = "Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32"
REVISION_PROD_FLUX_9B: str = "94ef5985edc982fa93f87df6e1c0faf94d9e9004"
RECIPE_PROD_SDNQ_9B: str = "mc-flux2-klein-9b-inpaint-v1"

# Qwen-Image-Edit-2511, cloud only: no local sidecar twin. The 4-bit SDNQ build of the
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
LIGHTNING_REPO: str = "lightx2v/Qwen-Image-Edit-2511-Lightning"
LIGHTNING_REVISION: str = "d74eba145674fd7e31b949324e148e21e7118abd"
LIGHTNING_FILE: str = "Qwen-Image-Edit-2511-Lightning-8steps-V1.0-bf16.safetensors"
LIGHTNING_BYTES: int = 849608296
# A manga balloon-text removal LoRA for 2511 (Apache-2.0), rank 1, loaded beside Lightning.
FUKIDASHI_REPO: str = "tori29umai/QwenImageEdit2511_LoRA"
FUKIDASHI_REVISION: str = "3ca05a1ece5606d4de68ebc564d46713fd77e549"
FUKIDASHI_FILE: str = "QIE2511_FukidashiErase_V1_dim4_1e-3-000080_dim1.safetensors"
FUKIDASHI_BYTES: int = 18753872
# The directory of the snapshot the LoRA is downloaded into.
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
}

# Files of the pinned snapshot, exactly as the Hub lists them at REVISION_PROD_FLUX.
# The Hub does not publish per-file SHA-256 for the small files, so the seed job checks
# the file set and every size; the commit hash pins the content itself.
PROD_SNAPSHOT_FILES: Tuple[Tuple[str, int], ...] = (
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
PROD_SNAPSHOT_TOTAL_BYTES: int = 5475930180

# Pinned from the Hugging Face repository tree at REVISION_PROD_FLUX_9B.
PROD_SNAPSHOT_9B_FILES: Tuple[Tuple[str, int], ...] = (
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
PROD_SNAPSHOT_9B_TOTAL_BYTES: int = 12606105778

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
    # "flux" (FLUX.2 Klein, the local sdnq recipe) or "qwen" (Qwen-Image-Edit + Lightning).
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

    def required_gpu_for(self, provider: str) -> Optional[str]:
        return dict(self.required_gpu).get(provider)


PRODUCTION_MODELS: Dict[str, ProductionModel] = {
    MODEL_PROD_FLUX: ProductionModel(MODEL_PROD_FLUX, REVISION_PROD_FLUX, RECIPE_PROD_SDNQ,
                                      PROD_SNAPSHOT_FILES, PROD_SNAPSHOT_TOTAL_BYTES, "Apache-2.0",
                                      label="FLUX.2 Klein 4B (4-bit)",
                                      # Live on an L4 (docs/cloud-work-log.md): 7,176 MiB loaded,
                                      # 7,180 MiB peak rendering a 768x576 working size; worker
                                      # RSS 7,121,456 kB after renders, rounded up to 7 GiB.
                                      render_peak_vram_mib=7180, render_copy_host_gib=7),
    MODEL_PROD_FLUX_9B: ProductionModel(MODEL_PROD_FLUX_9B, REVISION_PROD_FLUX_9B,
                                         RECIPE_PROD_SDNQ_9B, PROD_SNAPSHOT_9B_FILES,
                                         PROD_SNAPSHOT_9B_TOTAL_BYTES, "FLUX non-commercial",
                                         label="FLUX.2 Klein 9B (4-bit)",
                                         required_gpu=(("modal", "L40S"), ("beam", "RTX5090")),
                                         worker_memory_gib=24, multi_copy_gpus=("L40S",)),
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
}


def production_model(model_id: str = MODEL_PROD_FLUX) -> ProductionModel:
    try:
        return PRODUCTION_MODELS[model_id]
    except KeyError:
        raise ValueError(f"Unsupported cloud model: {model_id}") from None


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
