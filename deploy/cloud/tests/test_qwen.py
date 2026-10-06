"""Qwen-Image-Edit runner with fake torch, PIL and pipeline modules: no model, no GPU."""

from __future__ import annotations

import unittest
from unittest import mock
from types import SimpleNamespace
from typing import Any, Dict, List, Optional

from deploy.cloud.common.flux import SdnqFluxRunner, WorkerRenderError, encode_png_rgb8
from deploy.cloud.common.manifest import (
    ADAPTER_DIR,
    FUKIDASHI_FILE,
    LIGHTNING_FILE,
    MODEL_PROD_FLUX,
    MODEL_PROD_FLUX_9B,
    MODEL_PROD_QWEN,
    QWEN_PROMPT,
    RECIPE_PROD_QWEN,
    get_pinned_sampling,
    get_production_model_info,
    production_model,
)
from deploy.cloud.common import qwen
from deploy.cloud.common.qwen import EDIT_RETRIES, LIGHTNING_SCHEDULER, MIN_EDIT_RATIO, QwenEditRunner, work_size
from deploy.cloud.common.worker import runner_for
from deploy.cloud.tests.test_worker import FakeImage, FakeTensor, fake_modules


class FakeQwenPipeline:
    """QwenImageEditPlusPipeline-like: explicit parameters, so a dropped kwarg shows."""

    def __init__(self, log: List[Any], error: Optional[Exception] = None):
        self.log = log
        self.error = error
        self.device: Optional[str] = None
        self.loras: List[Any] = []
        self.active: Any = None
        self.calls: List[Dict[str, Any]] = []

    def load_lora_weights(self, path: str, weight_name: str, adapter_name: str) -> None:
        self.loras.append((path, weight_name, adapter_name))

    def set_adapters(self, names: List[str], weights: List[float]) -> None:
        self.active = (names, weights)

    def set_progress_bar_config(self, **kwargs: Any) -> None:
        return None

    def to(self, device: str) -> "FakeQwenPipeline":
        self.device = device
        return self

    def __call__(self, image=None, prompt=None, negative_prompt=None, true_cfg_scale=4.0, height=None,
                 width=None, num_inference_steps=50, generator=None, output_type="pil"):
        self.calls.append({"image": image, "prompt": prompt, "negative_prompt": negative_prompt,
                           "true_cfg_scale": true_cfg_scale, "height": height, "width": width,
                           "num_inference_steps": num_inference_steps, "generator": generator,
                           "output_type": output_type})
        if self.error is not None:
            raise self.error
        return SimpleNamespace(images=[FakeTensor(self.log, (width, height))])


def qwen_modules(image_size=(320, 848), cuda=True, call_error=None) -> SimpleNamespace:
    base = fake_modules(cuda=cuda, image_size=image_size)
    base.Image.new = lambda mode, size, color: FakeImage(size, mode, base.log)
    pipeline = FakeQwenPipeline(base.log, call_error)
    loads: List[Any] = []

    def from_pretrained(path: str, **kwargs: Any) -> FakeQwenPipeline:
        loads.append((path, kwargs))
        return pipeline

    return SimpleNamespace(
        torch=base.torch, np=base.np, Image=base.Image, log=base.log, loads=loads, pipeline=pipeline,
        Pipeline=SimpleNamespace(from_pretrained=from_pretrained),
        Scheduler=SimpleNamespace(from_config=lambda config: ("scheduler", config)),
    )


class WorkSizeTest(unittest.TestCase):
    def test_matches_the_pipeline_reference_grid(self) -> None:
        # calculate_dimensions(1024*1024, w/h) in diffusers, rounded to 32.
        self.assertEqual(work_size(64, 64), (1024, 1024))
        self.assertEqual(work_size(320, 848), (640, 1664))
        self.assertEqual(work_size(1080, 1536), (864, 1216))
        for w, h in ((320, 848), (1080, 1536), (2048, 16), (16, 16)):
            ww, wh = work_size(w, h)
            self.assertEqual((ww % 32, wh % 32), (0, 0))


class QwenEditRunnerTest(unittest.TestCase):
    def test_render_uses_the_lightning_and_fukidashi_recipe(self) -> None:
        mods = qwen_modules(image_size=(320, 848))
        runner = QwenEditRunner("/weights/snap", modules=mods)
        sampling = get_pinned_sampling(RECIPE_PROD_QWEN)
        png = runner.render_png(b"png", b"", 320, 848, seed=sampling.seed, steps=sampling.steps,
                                guidance_scaled=sampling.guidance_scaled)

        path, kwargs = mods.loads[0]
        self.assertEqual(path, "/weights/snap")
        self.assertEqual(kwargs["scheduler"], ("scheduler", LIGHTNING_SCHEDULER))
        self.assertEqual(mods.pipeline.loras, [(f"/weights/snap/{ADAPTER_DIR}", LIGHTNING_FILE, "lightning"),
                                               (f"/weights/snap/{ADAPTER_DIR}", FUKIDASHI_FILE, "fukidashi")])
        self.assertEqual(mods.pipeline.active, (["lightning", "fukidashi"], [1.0, 1.0]))
        self.assertEqual(mods.pipeline.device, "cuda")

        call = mods.pipeline.calls[0]
        self.assertEqual(call["prompt"], qwen.build_prompt(None))
        self.assertEqual(call["true_cfg_scale"], 1.0)
        self.assertIsNone(call["negative_prompt"])
        self.assertEqual(call["num_inference_steps"], 8)
        self.assertEqual((call["width"], call["height"]), (640, 1664))
        self.assertEqual([image.size for image in call["image"]], [(640, 1664)])
        self.assertEqual((call["generator"].device, call["generator"].seed), ("cpu", 1))
        self.assertEqual(call["output_type"], "pt")
        # Answer larger than the crop: shrunk back with BOX, to exactly the crop.
        self.assertIn(("resize", (320, 848), "box"), mods.log)
        self.assertEqual(png, encode_png_rgb8(320, 848))

    def test_large_crop_is_scaled_up_after_the_render(self) -> None:
        mods = qwen_modules(image_size=(1080, 1536))
        QwenEditRunner("/w", modules=mods).render_png(b"png", b"", 1080, 1536, seed=1, steps=4, guidance_scaled=100)
        self.assertIn(("resize", (864, 1216), "bicubic"), mods.log)
        self.assertIn(("resize", (1080, 1536), "bicubic"), mods.log)

    def test_cfg_above_one_sends_a_negative_prompt(self) -> None:
        mods = qwen_modules(image_size=(64, 64))
        QwenEditRunner("/w", modules=mods).render_png(b"png", b"", 64, 64, seed=1, steps=40, guidance_scaled=400)
        call = mods.pipeline.calls[0]
        self.assertEqual((call["true_cfg_scale"], call["negative_prompt"]), (4.0, " "))

    def test_failures_are_typed(self) -> None:
        with self.assertRaises(WorkerRenderError) as caught:
            QwenEditRunner("/w", modules=qwen_modules(cuda=False)).load()
        self.assertEqual(caught.exception.error_code, "gpu_unavailable")
        mods = qwen_modules(image_size=(64, 64), call_error=RuntimeError("CUDA out of memory"))
        with self.assertRaises(WorkerRenderError) as caught:
            QwenEditRunner("/w", modules=mods).render_png(b"png", b"", 64, 64, seed=1, steps=4, guidance_scaled=100)
        self.assertEqual(caught.exception.error_code, "out_of_memory")
        with self.assertRaises(WorkerRenderError) as caught:
            QwenEditRunner("/w", modules=qwen_modules(image_size=(64, 32))).render_png(
                b"png", b"", 64, 64, seed=1, steps=4, guidance_scaled=100)
        self.assertEqual(caught.exception.error_code, "invalid_image")

    def test_warmup_does_not_count(self) -> None:
        mods = qwen_modules(image_size=(64, 64))
        runner = QwenEditRunner("/w", modules=mods)
        runner.warmup()
        self.assertEqual(runner.render_count, 0)
        self.assertEqual(mods.pipeline.calls[0]["num_inference_steps"], 1)


class Rendered:
    """A finished render the retry loop can only compare and save."""

    def __init__(self, seed: int) -> None:
        self.seed = seed

    def save(self, buffer: Any, format: str) -> None:
        buffer.write(f"seed {self.seed}".encode())


class EditRetryTest(unittest.TestCase):
    """A seed that leaves the lettering untouched is followed by the next seed."""

    def run_with(self, ratios: Dict[int, float], hint: bytes = b"hint") -> tuple:
        runner = QwenEditRunner("/w", modules=qwen_modules(image_size=(64, 64)))
        seeds: List[int] = []

        def render(crop: Any, seed: int, steps: int, guidance_scaled: int, prompt: str) -> Rendered:
            seeds.append(seed)
            return Rendered(seed)

        runner._render = render
        runner._lettering = lambda hint_png, size: "lettering" if hint_png else None
        with mock.patch.object(qwen, "build_prompt", return_value="prompt"), mock.patch.object(qwen, "edit_ratio", lambda np, before, after, mask: ratios[after.seed]):
            png = runner.render_png(b"png", hint, 64, 64, seed=1, steps=4, guidance_scaled=100)
        return png, seeds, runner.render_count

    def test_an_edit_on_the_first_seed_renders_once(self) -> None:
        self.assertEqual(self.run_with({1: 8.0}), (b"seed 1", [1], 1))

    def test_an_untouched_render_tries_the_next_seed(self) -> None:
        # c32 p20: seed 1 scored 0.6 and left both sound effects; seed 2 removed them.
        self.assertEqual(self.run_with({1: 0.6, 2: 8.0}), (b"seed 2", [1, 2], 1))

    def test_no_seed_edits_reports_failure(self) -> None:
        ratios = {1: 0.6, 2: 1.4, 3: 0.9}
        self.assertEqual(len(ratios), 1 + EDIT_RETRIES)
        self.assertTrue(all(r < MIN_EDIT_RATIO for r in ratios.values()))
        with self.assertRaises(WorkerRenderError) as raised:
            self.run_with(ratios)
        self.assertEqual(raised.exception.error_code, "edit_not_applied")

    def test_no_hint_is_never_retried(self) -> None:
        self.assertEqual(self.run_with({1: 0.0}, hint=b""), (b"seed 1", [1], 1))


class EditRatioTest(unittest.TestCase):
    def test_lettering_change_is_measured_against_the_page(self) -> None:
        try:
            import numpy as np
            from PIL import Image
        except ImportError:
            self.skipTest("numpy and Pillow are worker-image dependencies")
        page = np.full((40, 40), 200, np.uint8)
        page[10:20, 10:20] = 0  # the lettering
        lettering = page == 0
        before = Image.fromarray(page).convert("RGB")
        kept = page.copy()
        kept[30:, 30:] = 180  # Qwen redraws the page a little
        removed = kept.copy()
        removed[10:20, 10:20] = 200
        self.assertLess(qwen.edit_ratio(np, before, Image.fromarray(kept).convert("RGB"), lettering), MIN_EDIT_RATIO)
        self.assertGreater(qwen.edit_ratio(np, before, Image.fromarray(removed).convert("RGB"), lettering), MIN_EDIT_RATIO)


class ModelSpecTest(unittest.TestCase):
    def test_runner_follows_the_model(self) -> None:
        self.assertIs(runner_for(MODEL_PROD_QWEN), QwenEditRunner)
        self.assertIs(runner_for(MODEL_PROD_FLUX), SdnqFluxRunner)
        self.assertIs(runner_for(MODEL_PROD_FLUX_9B), SdnqFluxRunner)

    def test_qwen_spec(self) -> None:
        model = production_model(MODEL_PROD_QWEN)
        self.assertEqual(model.total_bytes, sum(size for _, size in model.files))
        self.assertEqual(model.required_gpu_for("modal"), "L40S")
        self.assertEqual(model.worker_memory_gib, 32)
        self.assertIsNone(production_model(MODEL_PROD_FLUX).required_gpu_for("modal"))
        info = get_production_model_info("modal", MODEL_PROD_QWEN)
        self.assertEqual((info.model_id, info.recipe_id), (MODEL_PROD_QWEN, RECIPE_PROD_QWEN))


if __name__ == "__main__":
    unittest.main()
