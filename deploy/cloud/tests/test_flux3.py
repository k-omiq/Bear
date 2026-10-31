"""FLUX 3 Image Klein runner with a numpy stand-in for torch and a fake modular pipeline: no model, no GPU."""

from __future__ import annotations

import io
import unittest
from contextlib import contextmanager
from types import SimpleNamespace
from typing import Any, Dict, List, Optional

try:
    import numpy as np
    from PIL import Image, ImageFilter
except ImportError:  # pragma: no cover - the cloud tests run without them too
    np = None

from deploy.cloud.common.contract import LATENT_STRIDE, Flux3Generation, QwenEdit
from deploy.cloud.common.flux import WorkerRenderError, megapixel_size, working_size
from deploy.cloud.common.manifest import (
    FLUX3_PROMPT,
    LOCATION_FLUX3_PROMPT,
    MARKER_FLUX3_PROMPT,
    MODEL_PROD_FLUX3,
    MODEL_PROD_FLUX3_BF16,
    RECIPE_PROD_FLUX3,
    RECIPE_PROD_FLUX3_BF16,
    get_pinned_sampling,
    production_model,
)
from deploy.cloud.common.worker import runner_for


class FakeTensor:
    """The few torch tensor operations the runner uses, over a numpy array."""

    def __init__(self, array: Any, dtype: str = "float32"):
        self.array = np.asarray(array)
        self.dtype = dtype

    @property
    def shape(self) -> tuple:
        return self.array.shape

    @property
    def T(self) -> "FakeTensor":
        return FakeTensor(self.array.T, self.dtype)

    def reshape(self, *shape: int) -> "FakeTensor":
        return FakeTensor(self.array.reshape(*shape), self.dtype)

    def to(self, target: Any) -> "FakeTensor":
        return FakeTensor(self.array, target) if target in ("float32", "bfloat16") else self

    def float(self) -> "FakeTensor":
        return FakeTensor(self.array.astype(np.float32))

    def __getitem__(self, index: Any) -> "FakeTensor":
        return FakeTensor(self.array[index], self.dtype)

    def _other(self, other: Any) -> Any:
        return other.array if isinstance(other, FakeTensor) else other

    def __add__(self, other: Any) -> "FakeTensor":
        return FakeTensor(self.array + self._other(other))

    __radd__ = __add__

    def __mul__(self, other: Any) -> "FakeTensor":
        return FakeTensor(self.array * self._other(other))

    __rmul__ = __mul__

    def __rsub__(self, other: Any) -> "FakeTensor":
        return FakeTensor(self._other(other) - self.array)


class FakeGenerator:
    def manual_seed(self, seed: int) -> "FakeGenerator":
        self.seed = seed
        return self


def fake_torch(cuda: bool = True) -> SimpleNamespace:
    @contextmanager
    def inference_mode():
        yield

    def randn(shape: tuple, generator: FakeGenerator) -> FakeTensor:
        return FakeTensor(np.random.default_rng(generator.seed).standard_normal(shape).astype(np.float32))

    return SimpleNamespace(
        cuda=SimpleNamespace(is_available=lambda: cuda),
        float32="float32",
        bfloat16="bfloat16",
        Generator=FakeGenerator,
        randn=randn,
        from_numpy=FakeTensor,
        inference_mode=inference_mode,
    )


class FakeLoop:
    def __init__(self, sub_blocks: Dict[str, Any]):
        self.sub_blocks = sub_blocks


class FakeHold:
    pass


class FakePipeline:
    """The built modular pipeline: records its setup and answers each call with a grey image."""

    def __init__(self, components: Dict[str, Any], error: Optional[Exception] = None):
        self.transformer = SimpleNamespace(config=SimpleNamespace(in_channels=128))
        self.vae = self.text_encoder = self.tokenizer = object()
        for name, value in components.items():
            setattr(self, name, value)
        self.error = error
        # True answers each call with the image it was given, as a model that changed nothing.
        self.echo = False
        # An image answers every call with it, as a model that drew exactly that.
        self.answer: Any = None
        self.dtype: Any = None
        self.scheduler: Any = None
        self.device: Optional[str] = None
        self.calls: List[Dict[str, Any]] = []

    def load_components(self, dtype: Any) -> None:
        self.dtype = dtype

    def update_components(self, scheduler: Any) -> None:
        self.scheduler = scheduler

    def to(self, device: str) -> "FakePipeline":
        self.device = device
        return self

    def __call__(self, **kwargs: Any) -> list:
        self.calls.append(kwargs)
        if self.error is not None:
            raise self.error
        if self.echo:
            return [kwargs["image"].copy()]
        if self.answer is not None:
            return [self.answer.copy()]
        return [Image.new("RGB", (kwargs["width"], kwargs["height"]), (128, 128, 128))]


class FakeLama:
    """Fills the hole with mid grey and records what it was given."""

    error: Optional[Exception] = None
    opened: List[str] = []

    def __init__(self, path: str, device: str):
        FakeLama.opened.append(path)
        self.fills = 0

    def fill(self, crop: Any, hole: Any) -> Any:
        if FakeLama.error is not None:
            raise FakeLama.error
        self.fills += 1
        filled = crop.copy()
        filled.paste((128, 128, 128), mask=hole)
        return filled


def flux3_modules(cuda: bool = True, load_error: Optional[Exception] = None,
                  call_error: Optional[Exception] = None, components: Optional[Dict[str, Any]] = None
                  ) -> SimpleNamespace:
    """A modular pipeline whose blocks hold two workflows, each with its denoising loop."""
    text2img = FakeLoop({"denoiser": "t2i", "after": "step"})
    edit = FakeLoop({"denoiser": "edit"})
    blocks = SimpleNamespace(
        sub_blocks={"prompt_upsampler": "upsampler", "text2img": SimpleNamespace(sub_blocks={"loop": text2img}),
                    "edit": SimpleNamespace(sub_blocks={"denoise": SimpleNamespace(sub_blocks={"loop": edit})})},
    )
    pipeline = FakePipeline(components or {}, call_error)
    loads: List[Any] = []

    def from_pretrained(path: str, **kwargs: Any) -> Any:
        loads.append((path, kwargs))
        if load_error is not None:
            raise load_error
        return SimpleNamespace(blocks=blocks)

    def init_pipeline(path: str) -> FakePipeline:
        loads.append(("init", path))
        return pipeline

    blocks.init_pipeline = init_pipeline
    return SimpleNamespace(
        torch=fake_torch(cuda), np=np, Image=Image, ImageFilter=ImageFilter,
        Modular=SimpleNamespace(from_pretrained=from_pretrained),
        Scheduler=lambda shift: ("scheduler", shift), Loop=FakeLoop, HoldBlock=FakeHold, Lama=FakeLama,
        blocks=blocks, loops=(text2img, edit), pipeline=pipeline, loads=loads,
    )


def png(image: Any) -> bytes:
    buffer = io.BytesIO()
    image.save(buffer, format="PNG")
    return buffer.getvalue()


def crop_and_hint(width: int, height: int, box: tuple) -> tuple:
    hint = Image.new("L", (width, height), 0)
    hint.paste(255, box)
    return png(Image.new("RGB", (width, height), (255, 255, 255))), png(hint)


@unittest.skipIf(np is None, "numpy and Pillow are not installed")
class Flux3EditRunnerTest(unittest.TestCase):
    def setUp(self) -> None:
        from deploy.cloud.common.flux3 import Flux3EditRunner

        self.Runner = Flux3EditRunner

    def test_production_model_dispatches_to_the_flux3_runner(self) -> None:
        self.assertIs(runner_for(MODEL_PROD_FLUX3), self.Runner)
        self.assertEqual(production_model(MODEL_PROD_FLUX3).recipe_id, RECIPE_PROD_FLUX3)
        self.assertEqual(FLUX3_PROMPT.count("{location}"), 1)

    def test_the_bf16_weights_are_the_same_recipe_on_a_bigger_gpu(self) -> None:
        self.assertIs(runner_for(MODEL_PROD_FLUX3_BF16), self.Runner)
        bf16 = production_model(MODEL_PROD_FLUX3_BF16)
        self.assertEqual(bf16.recipe_id, RECIPE_PROD_FLUX3_BF16)
        self.assertEqual(get_pinned_sampling(RECIPE_PROD_FLUX3_BF16), get_pinned_sampling(RECIPE_PROD_FLUX3))
        self.assertEqual((bf16.required_gpu_for("modal"), bf16.required_gpu_for("beam")), ("L40S", "RTX5090"))
        self.assertTrue(bf16.partial_repo)
        folders = {path.split("/")[0] for path, _size in bf16.files if "/" in path}
        self.assertEqual(folders, {"adapters", "text_encoder", "transformer", "vae"})

    def test_a_request_working_size_scales_the_crop_to_it(self) -> None:
        mods = flux3_modules()
        runner = self.Runner("/weights/snap", modules=mods)
        crop, hint = crop_and_hint(300, 200, (220, 10, 290, 50))
        runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100, work_long_side=1280)
        call = mods.pipeline.calls[0]
        self.assertEqual((call["width"], call["height"]), working_size(300, 200, 1280))
        self.assertEqual((call["width"], call["height"]), (1280, 848))

    def test_the_ported_generation_builds_each_part_its_way(self) -> None:
        from deploy.cloud.common.flux3 import LOW_TIME_SHIFT, TIME_SHIFT

        mods = flux3_modules()
        runner = self.Runner("/weights/snap", modules=mods)
        crop, hint = crop_and_hint(300, 200, (220, 10, 290, 50))
        every = Flux3Generation(location_prompt=True, marker=True, low_shift=True, megapixel_size=True)
        runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100,
                          qwen_edit=QwenEdit("auto", "a brick wall"), flux3_generation=every)
        call = mods.pipeline.calls[0]
        self.assertEqual(call["prompt"], LOCATION_FLUX3_PROMPT.replace("{location}", "upper right")
                         + " Target description: a brick wall" + MARKER_FLUX3_PROMPT)
        self.assertEqual((call["width"], call["height"]), megapixel_size(300, 200))
        self.assertEqual(megapixel_size(300, 200), (1248, 832))
        # The grown lettering is magenta in the image the model edits, the page round it is not.
        painted = np.asarray(call["image"].resize((300, 200), Image.Resampling.NEAREST))
        self.assertEqual(tuple(painted[30, 255]), (255, 0, 255))
        self.assertEqual(tuple(painted[150, 50]), (255, 255, 255))
        self.assertAlmostEqual(LOW_TIME_SHIFT, 6.93, places=2)
        self.assertEqual(mods.pipeline.scheduler, ("scheduler", LOW_TIME_SHIFT))

        # Each part alone; the shift goes back for a request that does not ask.
        runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100,
                          flux3_generation=Flux3Generation(location_prompt=True))
        call = mods.pipeline.calls[1]
        self.assertEqual(call["prompt"], LOCATION_FLUX3_PROMPT.replace("{location}", "upper right"))
        self.assertEqual((call["width"], call["height"]), working_size(300, 200))
        self.assertEqual(tuple(np.asarray(call["image"])[30, 600]), (255, 255, 255))
        self.assertEqual(mods.pipeline.scheduler, ("scheduler", TIME_SHIFT))

    def lettered_crop(self) -> tuple:
        """A white crop with dark, uneven lettering in the hint."""
        image = Image.new("RGB", (300, 200), (255, 255, 255))
        for x in range(220, 290, 6):
            image.paste((20, 20, 20), (x, 10, x + 3, 50))
        hint = Image.new("L", (300, 200), 0)
        hint.paste(255, (220, 10, 290, 50))
        return png(image), png(hint)

    def test_lettering_left_in_place_is_edited_again_from_a_lama_fill(self) -> None:
        mods = flux3_modules()
        mods.pipeline.echo = True
        runner = self.Runner("/weights/snap", modules=mods)
        crop, hint = self.lettered_crop()
        answer = runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100)
        self.assertEqual(runner.lama.fills, 1)
        self.assertEqual(len(mods.pipeline.calls), 2)
        # The second edit reads the filled crop, and its answer is the one returned.
        work_w, work_h = mods.pipeline.calls[1]["width"], mods.pipeline.calls[1]["height"]
        second = mods.pipeline.calls[1]["image"].convert("L")
        self.assertEqual(second.getpixel((255 * work_w // 300, 30 * work_h // 200)), 128)
        with Image.open(io.BytesIO(answer)) as result:
            self.assertEqual(result.size, (300, 200))
            self.assertEqual(result.convert("L").getpixel((255, 30)), 128)
        self.assertEqual(runner.render_count, 1)

    def test_a_clean_first_edit_is_not_edited_again(self) -> None:
        mods = flux3_modules()
        runner = self.Runner("/weights/snap", modules=mods)
        crop, hint = self.lettered_crop()
        runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100)
        self.assertEqual(runner.lama.fills, 0)
        self.assertEqual(len(mods.pipeline.calls), 1)

    def test_the_marker_keeps_its_own_check_and_no_second_edit(self) -> None:
        mods = flux3_modules()
        mods.pipeline.echo = True
        runner = self.Runner("/weights/snap", modules=mods)
        crop, hint = self.lettered_crop()
        with self.assertRaises(WorkerRenderError):
            runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100,
                              flux3_generation=Flux3Generation(marker=True))
        self.assertEqual(runner.lama.fills, 0)

    def test_a_failed_lama_fill_fails_the_render(self) -> None:
        mods = flux3_modules()
        mods.pipeline.echo = True
        runner = self.Runner("/weights/snap", modules=mods)
        crop, hint = self.lettered_crop()
        FakeLama.error = RuntimeError("no session")
        self.addCleanup(setattr, FakeLama, "error", None)
        with self.assertRaises(WorkerRenderError) as caught:
            runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100)
        self.assertEqual(caught.exception.error_code, "inference_failed")

    def test_load_opens_the_seeded_lama_graph(self) -> None:
        FakeLama.opened.clear()
        runner = self.Runner("/weights/snap", modules=flux3_modules())
        runner.load()
        self.assertEqual(FakeLama.opened, ["/weights/snap/adapters/lama-manga.onnx"])

    def test_lettering_kept_reads_correlation_and_flat_sides(self) -> None:
        from deploy.cloud.common.flux3 import lettering_kept

        ink = np.ones((4, 4), bool)
        was = np.array([[0, 255, 0, 255]] * 4, dtype=np.float32)[..., None].repeat(3, axis=2)
        self.assertTrue(lettering_kept(was, was * 0.9 + 5, ink, np))
        self.assertFalse(lettering_kept(was, 255 - was, ink, np))
        flat = np.full((4, 4, 3), 200, dtype=np.float32)
        self.assertFalse(lettering_kept(was, flat, ink, np))
        self.assertFalse(lettering_kept(flat, was, ink, np))
        self.assertFalse(lettering_kept(was, was, np.zeros((4, 4), bool), np))

    def test_a_result_that_kept_the_marker_is_refused(self) -> None:
        mods = flux3_modules()
        mods.pipeline.echo = True
        runner = self.Runner("/weights/snap", modules=mods)
        crop, hint = crop_and_hint(300, 200, (220, 10, 290, 50))
        with self.assertRaises(WorkerRenderError) as caught:
            runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100,
                              flux3_generation=Flux3Generation(marker=True))
        self.assertEqual(caught.exception.error_code, "marker_remained")
        # Without the marker the same answer is fine.
        runner.render_png(crop, hint, 300, 200, seed=1, steps=16, guidance_scaled=100)

    def test_the_marker_limit_is_more_than_max_16_or_1_percent(self) -> None:
        from deploy.cloud.common.flux3 import MARKER, marker_remaining

        def kept(lettering_px: int, painted_px: int) -> bool:
            lettering = np.zeros((100, 100), dtype=bool)
            lettering.flat[:lettering_px] = True
            result = np.full((100, 100, 3), 255, dtype=np.uint8)
            result.reshape(-1, 3)[:painted_px] = MARKER
            return marker_remaining(result, lettering, np)

        self.assertFalse(kept(300, 16))
        self.assertTrue(kept(300, 17))
        self.assertFalse(kept(5000, 50))
        self.assertTrue(kept(5000, 51))

    def test_pink_art_the_model_drew_back_is_not_the_marker(self) -> None:
        mods = flux3_modules()
        runner = self.Runner("/weights/snap", modules=mods)
        # Pink hair in the ring the hint grows into, which the marker paints over too.
        page = Image.new("RGB", (300, 200), (255, 255, 255))
        page.paste((220, 100, 210), (212, 2, 298, 58))
        hint = Image.new("L", (300, 200), 0)
        hint.paste(255, (220, 10, 290, 50))
        page_png = png(page)
        # A model that took the paint off and drew the hair back: the unpainted crop.
        mods.pipeline.answer = page
        runner.render_png(page_png, png(hint), 300, 200, seed=1, steps=16, guidance_scaled=100,
                          flux3_generation=Flux3Generation(marker=True))
        from deploy.cloud.common.flux3 import marker_remaining
        lettering = np.asarray(hint.filter(ImageFilter.MaxFilter(17))) > 0
        self.assertTrue(marker_remaining(np.asarray(page), lettering, np), "the colour test alone would refuse it")
        self.assertFalse(marker_remaining(np.asarray(page), lettering, np, np.asarray(page)))

    def test_load_drops_the_upsampler_adds_the_hold_and_sets_bfl_sampling(self) -> None:
        from deploy.cloud.common.flux3 import TIME_SHIFT

        mods = flux3_modules()
        pipeline = self.Runner("/weights/snap", modules=mods).load()
        self.assertEqual(mods.loads, [("/weights/snap", {"local_files_only": True}), ("init", "/weights/snap")])
        self.assertNotIn("prompt_upsampler", mods.blocks.sub_blocks)
        for loop in mods.loops:
            self.assertIsInstance(loop.sub_blocks["hold"], FakeHold)
            self.assertEqual(list(loop.sub_blocks)[-1], "hold")
        self.assertEqual(pipeline.dtype, {"vae": "float32", "default": "bfloat16"})
        self.assertEqual((TIME_SHIFT, pipeline.scheduler), (16.0, ("scheduler", 16.0)))
        self.assertEqual(pipeline.device, "cuda")

    def test_load_failures_name_their_cause(self) -> None:
        cases = [
            (flux3_modules(cuda=False), "gpu_unavailable"),
            (flux3_modules(load_error=RuntimeError("CUDA out of memory")), "out_of_memory"),
            (flux3_modules(load_error=OSError("no such file")), "model_load_failed"),
            # load_components logs a component it cannot load and carries on.
            (flux3_modules(components={"vae": None}), "model_load_failed"),
        ]
        for mods, code in cases:
            with self.assertRaises(WorkerRenderError) as caught:
                self.Runner("/weights/snap", modules=mods).load()
            self.assertEqual(caught.exception.error_code, code)

    def test_render_edits_the_crop_with_the_recipe_and_returns_its_size(self) -> None:
        mods = flux3_modules()
        runner = self.Runner("/weights/snap", modules=mods)
        sampling = get_pinned_sampling(RECIPE_PROD_FLUX3)
        crop, hint = crop_and_hint(300, 200, (220, 10, 290, 50))
        result = runner.render_png(crop, hint, 300, 200, seed=sampling.seed, steps=sampling.steps,
                                   guidance_scaled=sampling.guidance_scaled,
                                   qwen_edit=QwenEdit("sound_effect", "a brick wall"))

        work_w, work_h = working_size(300, 200)
        call = mods.pipeline.calls[0]
        self.assertTrue(call["prompt"].startswith(FLUX3_PROMPT.format(location="upper right")))
        self.assertTrue(call["prompt"].endswith(" Behind the lettering: a brick wall."))
        self.assertEqual(call["image"].size, (work_w, work_h))
        self.assertEqual((call["width"], call["height"], call["num_inference_steps"]), (work_w, work_h, 16))
        self.assertEqual(call["latents"].shape, (1, 128, work_h // LATENT_STRIDE, work_w // LATENT_STRIDE))
        self.assertEqual(call["latents"].dtype, "bfloat16")
        self.assertIsNone(call["hold"])
        self.assertEqual(call["output"], "images")
        with Image.open(io.BytesIO(result)) as answer:
            self.assertEqual((answer.size, answer.mode), ((300, 200), "RGB"))
        self.assertEqual(runner.render_count, 1)

    def test_same_seed_same_noise_and_a_blank_hint_reads_central(self) -> None:
        mods = flux3_modules()
        runner = self.Runner("/weights/snap", modules=mods)
        crop, _ = crop_and_hint(64, 64, (0, 0, 1, 1))
        blank = png(Image.new("L", (64, 64), 0))
        for _ in range(2):
            runner.render_png(crop, blank, 64, 64, seed=7, steps=16, guidance_scaled=100)
        first, second = mods.pipeline.calls
        np.testing.assert_array_equal(first["latents"].array, second["latents"].array)
        self.assertEqual(first["prompt"], FLUX3_PROMPT.format(location="central"))

    def test_hold_keeps_only_tokens_outside_the_grown_hint(self) -> None:
        mods = flux3_modules()
        runner = self.Runner("/weights/snap", modules=mods)
        runner.hold = True
        crop, hint = crop_and_hint(256, 256, (100, 100, 110, 110))
        runner.render_png(crop, hint, 256, 256, seed=1, steps=16, guidance_scaled=100)
        hold = mods.pipeline.calls[0]["hold"]
        work_w, work_h = working_size(256, 256)
        keep = hold.keep.array.reshape(work_h // LATENT_STRIDE, work_w // LATENT_STRIDE)
        self.assertGreater(keep.sum(), 0)
        self.assertLess(keep.sum(), keep.size)
        # The middle of the hint is a hole token; the far corner is kept.
        scale = work_w / 256
        self.assertEqual(keep[int(105 * scale) // LATENT_STRIDE, int(105 * scale) // LATENT_STRIDE], 0)
        self.assertEqual(keep[0, 0], 1)

    def test_latent_hold_lands_on_the_reference_and_refuses_a_mismatch(self) -> None:
        from deploy.cloud.common.flux3 import LatentHold

        keep = FakeTensor(np.array([[[1.0], [0.0]]], np.float32))
        noise = FakeTensor(np.full((1, 2, 3), 5.0, np.float32))
        reference = FakeTensor(np.full((1, 2, 3), 2.0, np.float32))
        latents = FakeTensor(np.full((1, 2, 3), 9.0, np.float32), "bfloat16")
        midway = LatentHold(keep, noise).apply(latents, reference, 0.5).array
        np.testing.assert_allclose(midway[0, 0], 3.5)
        np.testing.assert_allclose(midway[0, 1], 9.0)
        np.testing.assert_allclose(LatentHold(keep, noise).apply(latents, reference, 0.0).array[0, 0], 2.0)
        with self.assertRaises(WorkerRenderError):
            LatentHold(keep, noise).apply(latents, FakeTensor(np.zeros((1, 3, 3))), 0.5)

    def test_bad_geometry_and_inference_errors_are_worker_errors(self) -> None:
        crop, hint = crop_and_hint(64, 64, (10, 10, 20, 20))
        with self.assertRaises(WorkerRenderError) as caught:
            self.Runner("/weights/snap", modules=flux3_modules()).render_png(crop, hint, 64, 48, 1, 16, 100)
        self.assertEqual(caught.exception.error_code, "invalid_image")
        for error, code in ((RuntimeError("CUDA out of memory"), "out_of_memory"),
                            (ValueError("bad shape"), "inference_failed")):
            runner = self.Runner("/weights/snap", modules=flux3_modules(call_error=error))
            with self.assertRaises(WorkerRenderError) as caught:
                runner.render_png(crop, hint, 64, 64, 1, 16, 100)
            self.assertEqual(caught.exception.error_code, code)


if __name__ == "__main__":
    unittest.main()
