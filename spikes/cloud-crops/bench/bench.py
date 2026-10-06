"""Quality benchmark: Qwen-Image-Edit-2511 variants against FLUX.2 Klein, on real crops.

A throwaway Modal app, not product code. Inputs are the crops `spike-cloud-crops export`
writes; answers come back as PNGs at crop size for `spike-cloud-crops composite`.

    modal run bench.py::seed
    modal run bench.py::main --crops DIR --out DIR --plan plan.json --stage NAME

Everything is pinned: the model repos by commit, the GPU image by version (the
production worker's pins plus peft and torchvision).
"""

from __future__ import annotations

import io
import json
import math
import os
import time
from pathlib import Path

import modal

# Only the local side has the repository; in a container this file is /root/bench.py.
REPO_ROOT = Path(__file__).resolve().parents[3] if modal.is_local() else Path("/root/repo")

app = modal.App("mc-qwen-bench")
volume = modal.Volume.from_name("mc-qwen-bench-weights", create_if_missing=True)
W = "/w"

QWEN = ("Qwen/Qwen-Image-Edit-2511", "6f3ccc0b56e431dc6a0c2b2039706d7d26f22cb9")
QWEN_SDNQ = ("Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32", "a285fceef1439d72d60533e83cb6d8921748a666")
LIGHTNING = ("lightx2v/Qwen-Image-Edit-2511-Lightning", "d74eba145674fd7e31b949324e148e21e7118abd")
LIGHTNING_FILES = {
    4: "Qwen-Image-Edit-2511-Lightning-4steps-V1.0-bf16.safetensors",
    8: "Qwen-Image-Edit-2511-Lightning-8steps-V1.0-bf16.safetensors",
}
KLEIN = {
    "klein4b": ("Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic", "45e9cc76cb70f84473ce5c6c2e2282d0ef3c6ecd"),
    "klein9b": ("Disty0/FLUX.2-klein-9B-SDNQ-4bit-dynamic-svd-r32", "94ef5985edc982fa93f87df6e1c0faf94d9e9004"),
}


def local_dir(repo: str) -> str:
    return f"{W}/{repo.replace('/', '--')}"


seed_image = modal.Image.debian_slim(python_version="3.12").pip_install("huggingface-hub[hf_xet]==1.24.0")

gpu_image = (
    modal.Image.debian_slim(python_version="3.12")
    .pip_install("torch==2.13.0", "torchvision", index_url="https://download.pytorch.org/whl/cu129")
    .pip_install(
        "diffusers==0.39.0",
        "sdnq==0.2.4",
        "transformers==5.15.0",
        "accelerate==1.14.0",
        "safetensors==0.8.0",
        "huggingface-hub==1.24.0",
        "pillow==12.3.0",
        "numpy==2.4.6",
        "peft",
        "psutil",
    )
    .env({"PYTHONPATH": "/root/repo", "HF_HUB_OFFLINE": "1"})
    .add_local_dir(REPO_ROOT / "deploy", "/root/repo/deploy", ignore=["**/__pycache__", "**/tests", "**/fixtures"])
)


@app.function(image=seed_image, volumes={W: volume}, cpu=8, memory=16384, timeout=4 * 3600)
def seed_weights() -> dict:
    from huggingface_hub import snapshot_download

    done = {}
    for repo, rev in [QWEN_SDNQ, *KLEIN.values(), QWEN]:
        started = time.time()
        path = snapshot_download(repo, revision=rev, local_dir=local_dir(repo), max_workers=16)
        size = sum(f.stat().st_size for f in Path(path).rglob("*") if f.is_file())
        done[repo] = {"bytes": size, "seconds": round(time.time() - started, 1)}
        volume.commit()
        print(repo, done[repo], flush=True)
    started = time.time()
    snapshot_download(LIGHTNING[0], revision=LIGHTNING[1], local_dir=local_dir(LIGHTNING[0]),
                      allow_patterns=list(LIGHTNING_FILES.values()))
    done[LIGHTNING[0]] = {"seconds": round(time.time() - started, 1)}
    volume.commit()
    return done


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------

LIGHTNING_SCHEDULER = {
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


def _peak_host_gib() -> float:
    import resource

    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / (1024 ** 2)


def _load_qwen(cfg: dict):
    import torch
    import sdnq  # noqa: F401  registers SDNQConfig with diffusers
    from diffusers import FlowMatchEulerDiscreteScheduler, QwenImageEditPlusPipeline, QwenImageTransformer2DModel

    notes = []
    lora = cfg.get("lora")
    scheduler = FlowMatchEulerDiscreteScheduler.from_config(LIGHTNING_SCHEDULER) if lora else None
    extra = {"scheduler": scheduler} if scheduler else {}
    if cfg["weights"] == "bf16":
        transformer = QwenImageTransformer2DModel.from_pretrained(
            local_dir(QWEN[0]), subfolder="transformer", torch_dtype=torch.bfloat16)
        pipe = QwenImageEditPlusPipeline.from_pretrained(
            local_dir(QWEN[0]), transformer=transformer, torch_dtype=torch.bfloat16, **extra)
    elif cfg["weights"] == "sdnq4":
        pipe = QwenImageEditPlusPipeline.from_pretrained(local_dir(QWEN_SDNQ[0]), torch_dtype=torch.bfloat16, **extra)
    else:
        raise ValueError(cfg["weights"])

    if lora:
        if cfg.get("fuse_on_gpu"):
            # Only where the bf16 transformer fits: an 80 GB card.
            pipe.transformer.to("cuda")
        pipe.load_lora_weights(f"{local_dir(LIGHTNING[0])}/{LIGHTNING_FILES[lora]}", adapter_name="lightning")
        if cfg.get("lora_fuse", True):
            pipe.fuse_lora(lora_scale=1.0)
            pipe.unload_lora_weights()
            notes.append("lora fused")
        else:
            notes.append("lora unfused (peft runtime)")

    quant = cfg.get("quant")
    if quant:
        from sdnq.quantizer import sdnq_post_load_quant

        kwargs = {"uint4svd": dict(weights_dtype="uint4", use_svd=True, svd_rank=32),
                  "int8": dict(weights_dtype="int8")}[quant]
        pipe.transformer = sdnq_post_load_quant(
            pipe.transformer, torch_dtype=torch.bfloat16, quantization_device="cuda", return_device="cuda",
            modules_to_not_convert=["norm_out", "txt_in", "proj_out", "transformer_blocks.0.img_mod.1.weight",
                                    "time_text_embed", "img_in"],
            **kwargs)
        notes.append(f"transformer quantized on load: {quant}")
        if cfg.get("quant_encoder"):
            pipe.text_encoder = sdnq_post_load_quant(pipe.text_encoder, torch_dtype=torch.bfloat16,
                                                     quantization_device="cuda", return_device="cuda", **kwargs)
            notes.append(f"encoder quantized on load: {quant}")

    if cfg.get("offload"):
        pipe.enable_model_cpu_offload()
        notes.append("model cpu offload")
    else:
        pipe.to("cuda")
    pipe.set_progress_bar_config(disable=True)
    return pipe, notes


def _qwen_work_size(w: int, h: int, mode: str) -> tuple[int, int]:
    if mode == "1mp":
        # The pipeline resizes its VAE reference to exactly this; an output of
        # the same size means the reference and the answer share one grid.
        width = math.sqrt(1024 * 1024 * w / h)
        return round(width / 32) * 32, round(width / (w / h) / 32) * 32
    if mode == "768":
        long_side = max(w, h)
        if long_side >= 768:
            return w - w % 16, h - h % 16
        s = 768 / long_side
        return max(16, round(w * s) // 16 * 16), max(16, round(h * s) // 16 * 16)
    raise ValueError(mode)


def _mark(crop, alpha, hint, mark: str):
    """Point the model at the region: a red outline just outside the write
    mask (never written back: the composite only writes inside it), or the
    lettering blanked out in white."""
    from PIL import Image, ImageFilter

    if mark == "redbox":
        inside = alpha.point(lambda v: 255 if v > 0 else 0).convert("L")
        outer = inside.filter(ImageFilter.MaxFilter(15))
        inner = inside.filter(ImageFilter.MaxFilter(7))
        ring = Image.composite(outer, Image.new("L", crop.size, 0), Image.eval(inner, lambda v: 255 - v))
        marked = crop.copy()
        marked.paste((255, 0, 0), mask=ring)
        return marked
    if mark == "whitefill":
        hole = hint.convert("L").point(lambda v: 255 if v > 0 else 0).filter(ImageFilter.MaxFilter(5))
        marked = crop.copy()
        marked.paste((255, 255, 255), mask=hole)
        return marked
    raise ValueError(mark)


def _render_qwen(pipe, cfg: dict, crop, hint, alpha=None):
    import torch
    from PIL import Image

    if cfg.get("mark"):
        crop = _mark(crop, alpha, hint, cfg["mark"])
    w, h = crop.size
    work_w, work_h = _qwen_work_size(w, h, cfg.get("work", "1mp"))
    image = crop.resize((work_w, work_h), Image.Resampling.BICUBIC)
    images = [image]
    if cfg.get("hint_image"):
        images.append(hint.convert("RGB").resize((work_w, work_h), Image.Resampling.NEAREST))
    generator = torch.Generator(device="cpu").manual_seed(int(cfg.get("seed", 1)))
    cfg_scale = float(cfg.get("cfg", 4.0))
    out = pipe(
        image=images,
        prompt=cfg["prompt"],
        negative_prompt=" " if cfg_scale > 1 else None,
        true_cfg_scale=cfg_scale,
        guidance_scale=1.0,
        num_inference_steps=int(cfg["steps"]),
        height=work_h,
        width=work_w,
        generator=generator,
    ).images[0]
    return out.convert("RGB").resize((w, h), Image.Resampling.BOX if out.size[0] >= w else Image.Resampling.BICUBIC)


def _render_all(cfg: dict, items: list[dict]) -> dict:
    """Load one variant, render every item, and report timings and memory."""
    import torch
    from PIL import Image

    torch.backends.cuda.matmul.allow_tf32 = True
    stats = {"variant": cfg["name"], "gpu": torch.cuda.get_device_name(0), "config": cfg}
    started = time.time()
    try:
        if cfg["kind"] == "klein":
            from deploy.cloud.common.flux import SdnqFluxRunner
            from deploy.cloud.common.manifest import PINNED_SAMPLING, RECIPE_PROD_SDNQ

            runner = SdnqFluxRunner(local_dir(KLEIN[cfg["weights"]][0]))
            runner.load()
            sampling = PINNED_SAMPLING[RECIPE_PROD_SDNQ]
            notes = [f"production runner, seed {sampling.seed}, steps {sampling.steps}"]
        else:
            pipe, notes = _load_qwen(cfg)
    except Exception as exc:  # the answer to "does this load" is the result
        import traceback

        stats.update(error=f"load: {exc!r}", traceback=traceback.format_exc()[-4000:],
                     load_seconds=round(time.time() - started, 1))
        return {"stats": stats, "answers": []}
    torch.cuda.synchronize()
    stats["load_seconds"] = round(time.time() - started, 1)
    stats["notes"] = notes
    stats["vram_after_load_gib"] = round(torch.cuda.memory_allocated() / 2 ** 30, 2)
    torch.cuda.reset_peak_memory_stats()

    answers = []
    for index, item in enumerate(items):
        crop = Image.open(io.BytesIO(item["crop"])).convert("RGB")
        hint = Image.open(io.BytesIO(item["hint"]))
        alpha = Image.open(io.BytesIO(item["alpha"])).convert("L")
        t0 = time.time()
        try:
            if cfg["kind"] == "klein":
                png = runner.render_png(item["crop"], crop.width, crop.height,
                                        seed=sampling.seed, steps=sampling.steps,
                                        guidance_scaled=sampling.guidance_scaled)
            else:
                with torch.inference_mode():
                    result = _render_qwen(pipe, cfg, crop, hint, alpha)
                buffer = io.BytesIO()
                result.save(buffer, format="PNG")
                png = buffer.getvalue()
        except Exception as exc:
            import traceback

            answers.append({"id": item["id"], "error": repr(exc), "traceback": traceback.format_exc()[-2000:]})
            continue
        torch.cuda.synchronize()
        answers.append({"id": item["id"], "png": png, "seconds": round(time.time() - t0, 3),
                        "work_px": _qwen_work_size(crop.width, crop.height, cfg.get("work", "1mp"))
                        if cfg["kind"] == "qwen" else None})
        if index == 0:
            stats["first_render_seconds"] = answers[-1]["seconds"]
    stats["peak_vram_gib"] = round(torch.cuda.max_memory_allocated() / 2 ** 30, 2)
    stats["peak_host_gib"] = round(_peak_host_gib(), 2)
    stats["total_seconds"] = round(time.time() - started, 1)
    return {"stats": stats, "answers": answers}


# Host memory is billed as reserved: 72 GiB holds (59 GiB measured peak) the bf16 pipeline while it
# loads; the L40S gets more for quantizing the bf16 transformer on load.
COMMON = dict(image=gpu_image, volumes={W: volume}, timeout=3 * 3600, cpu=4)


@app.function(gpu="H100!", memory=73728, **COMMON)
def render_h100(cfg: dict, items: list[dict]) -> dict:
    return _render_all(cfg, items)


@app.function(gpu="L40S", memory=98304, **COMMON)
def render_l40s(cfg: dict, items: list[dict]) -> dict:
    return _render_all(cfg, items)


@app.function(gpu="L4", memory=65536, **COMMON)
def render_l4(cfg: dict, items: list[dict]) -> dict:
    return _render_all(cfg, items)


RENDERERS = {"H100": render_h100, "L40S": render_l40s, "L4": render_l4}


# ---------------------------------------------------------------------------
# Local driver
# ---------------------------------------------------------------------------


@app.local_entrypoint()
def main(crops: str, out: str, plan: str, stage: str, only: str = ""):
    """Run one stage of `plan`: every variant in it, in parallel, on its GPU."""
    spec = json.loads(Path(plan).read_text())
    ids = spec["sets"][spec["stages"][stage]["set"]]
    items = [{"id": i,
              "crop": (Path(crops) / i / "crop.png").read_bytes(),
              "hint": (Path(crops) / i / "hint.png").read_bytes(),
              "alpha": (Path(crops) / i / "alpha.png").read_bytes()} for i in ids]
    variants = [v for v in spec["stages"][stage]["variants"] if not only or v["name"] in only.split(",")]
    calls = []
    for v in variants:
        cfg = {**spec.get("defaults", {}), **v}
        calls.append((cfg, RENDERERS[cfg["gpu"]].spawn(cfg, items)))
    for cfg, call in calls:
        result = call.get()
        target = Path(out) / cfg["name"]
        target.mkdir(parents=True, exist_ok=True)
        errors = 0
        seconds = []
        for answer in result["answers"]:
            if "png" in answer:
                (target / f"{answer['id']}.png").write_bytes(answer["png"])
                seconds.append(answer["seconds"])
            else:
                errors += 1
        stats = result["stats"]
        stats["errors"] = errors
        stats["answer_errors"] = [a for a in result["answers"] if "error" in a][:3]
        stats["per_crop_seconds"] = seconds
        (target / "stats.json").write_text(json.dumps(stats, indent=2))
        warm = sorted(seconds[1:])
        median = warm[len(warm) // 2] if warm else None
        print(f"{cfg['name']:<24} {stats.get('gpu', '?'):<24} load {stats.get('load_seconds')} s  "
              f"median {median} s  peak VRAM {stats.get('peak_vram_gib')} GiB  "
              f"answers {len(seconds)}  errors {errors}  {stats.get('error', '')}")


@app.local_entrypoint()
def seed():
    print(json.dumps(seed_weights.remote(), indent=2))
