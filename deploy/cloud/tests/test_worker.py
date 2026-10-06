"""GPU worker code with fake torch, PIL and pipeline modules: no model, no GPU."""

from __future__ import annotations

import hashlib
import io
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Dict, List, Optional, Tuple

from deploy.cloud.common.flux import (
    HOLE_GROWTH,
    SdnqFluxRunner,
    WorkerRenderError,
    encode_png_rgb8,
    filter_call_kwargs,
    guidance_from_scaled,
    recipe_mismatch,
    render_job,
    sampling_mismatch,
    snap_stride,
    working_size,
)
from deploy.cloud.common.jobs import MemoryStore, job_key, run_key
from deploy.cloud.common.manifest import RECIPE_PROMPT, RECIPE_TEST_SDNQ, production_limits
from deploy.cloud.common.worker import MAX_MESSAGE_CHARS, WorkerRuntime, begin_run, failure, outcome_from_result
from deploy.cloud.modal.backend import worker_render
from deploy.cloud.tests.support import FakeRunner, make_job, metadata_json, silence_deploy_logs, write_marker

_restore_logs = None


def setUpModule() -> None:
    global _restore_logs
    _restore_logs = silence_deploy_logs()


def tearDownModule() -> None:
    _restore_logs()


# ---------------------------------------------------------------------------
# Fake modules for SdnqFluxRunner
# ---------------------------------------------------------------------------


class FakeImage:
    def __init__(self, size: Tuple[int, int], mode: str = "RGB", log: Optional[List[Any]] = None):
        self.size = size
        self.mode = mode
        self.log = log if log is not None else []

    def __enter__(self) -> "FakeImage":
        return self

    def __exit__(self, *exc: Any) -> None:
        return None

    def convert(self, mode: str) -> "FakeImage":
        self.log.append(("convert", mode))
        return FakeImage(self.size, mode, self.log)

    def resize(self, size: Tuple[int, int], resample: str) -> "FakeImage":
        self.log.append(("resize", size, resample))
        return FakeImage(size, self.mode, self.log)

    def point(self, table: Any) -> "FakeImage":
        self.log.append(("point", table(0), table(1)))
        return FakeImage(self.size, self.mode, self.log)

    def filter(self, kernel: Any) -> "FakeImage":
        self.log.append(("filter", kernel))
        return FakeImage(self.size, self.mode, self.log)

    def save(self, buffer: io.BytesIO, format: str) -> None:
        self.log.append(("save", self.size, format))
        buffer.write(encode_png_rgb8(*self.size))


class FakeTensor:
    """Every tensor method the runner chains returns the tensor and is logged."""

    def __init__(self, log: List[Any], shape: Tuple[int, int]):
        self.log = log
        self.shape = shape

    def __getattr__(self, name: str):
        def method(*args: Any, **kwargs: Any) -> Any:
            self.log.append((name,) + args)
            if name == "numpy":
                return SimpleNamespace(shape=(self.shape[1], self.shape[0], 3))
            return self

        return method


class FakeGenerator:
    def __init__(self, device: str):
        self.device = device
        self.seed: Optional[int] = None

    def manual_seed(self, seed: int) -> "FakeGenerator":
        self.seed = seed
        return self


class FakePipeline:
    """Flux2KleinInpaintPipeline-like: no max_area parameter, so the runner must drop it."""

    def __init__(self, log: List[Any], error: Optional[Exception] = None):
        self.log = log
        self.error = error
        self.device: Optional[str] = None
        self.calls: List[Dict[str, Any]] = []

    def to(self, device: str) -> "FakePipeline":
        self.device = device
        return self

    def __call__(self, image, prompt, width, height, num_inference_steps, guidance_scale, generator, output_type,
                 mask_image, image_reference, strength):
        self.calls.append(
            {
                "image": image,
                "mask_image": mask_image,
                "image_reference": image_reference,
                "strength": strength,
                "prompt": prompt,
                "width": width,
                "height": height,
                "num_inference_steps": num_inference_steps,
                "guidance_scale": guidance_scale,
                "generator": generator,
                "output_type": output_type,
            }
        )
        if self.error is not None:
            raise self.error
        return SimpleNamespace(images=[FakeTensor(self.log, (width, height))])


def fake_modules(
    cuda: bool = True,
    load_error: Optional[Exception] = None,
    call_error: Optional[Exception] = None,
    image_size: Tuple[int, int] = (32, 32),
) -> SimpleNamespace:
    log: List[Any] = []
    pipeline = FakePipeline(log, call_error)
    loads: List[Tuple[str, Dict[str, Any]]] = []

    def from_pretrained(path: str, **kwargs: Any) -> FakePipeline:
        loads.append((path, kwargs))
        if load_error is not None:
            raise load_error
        return pipeline

    @contextmanager
    def inference_mode():
        log.append(("inference_mode",))
        yield

    torch = SimpleNamespace(
        cuda=SimpleNamespace(is_available=lambda: cuda),
        bfloat16="bfloat16",
        uint8="uint8",
        Generator=FakeGenerator,
        inference_mode=inference_mode,
        nan_to_num=lambda tensor, **kwargs: (log.append(("nan_to_num", kwargs)), tensor)[1],
    )
    numpy = SimpleNamespace(ascontiguousarray=lambda array: array)
    image_module = SimpleNamespace(
        open=lambda stream: FakeImage(image_size, "RGB", log),
        fromarray=lambda array: FakeImage((array.shape[1], array.shape[0]), "RGB", log),
        Resampling=SimpleNamespace(BICUBIC="bicubic", BOX="box", NEAREST="nearest"),
    )
    return SimpleNamespace(
        torch=torch,
        np=numpy,
        Image=image_module,
        ImageFilter=SimpleNamespace(MaxFilter=lambda size: ("max", size)),
        Pipeline=SimpleNamespace(from_pretrained=from_pretrained),
        log=log,
        pipeline=pipeline,
        loads=loads,
    )


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class RecipeMathTest(unittest.TestCase):
    def test_snap_stride(self) -> None:
        self.assertEqual([snap_stride(v) for v in (1, 15, 16, 31, 32, 770)], [16, 16, 16, 16, 32, 768])

    def test_working_size_upscales_small_crops_only(self) -> None:
        self.assertEqual(working_size(64, 32), (768, 384))
        # 100x60 scales by 7.68 to 768x460.8, snapped down to the stride.
        self.assertEqual(working_size(100, 60), (768, 448))
        self.assertEqual(working_size(800, 600), (800, 600))
        self.assertEqual(working_size(768, 16), (768, 16))

    def test_guidance_travels_scaled(self) -> None:
        self.assertEqual(guidance_from_scaled(100), 1.0)
        self.assertEqual(guidance_from_scaled(350), 3.5)

    def test_filter_call_kwargs(self) -> None:
        class Narrow:
            def __call__(self, image, prompt):
                return None

        class Open:
            def __call__(self, image, **kwargs):
                return None

        candidate = {"image": 1, "prompt": 2, "max_area": 3}
        self.assertEqual(filter_call_kwargs(Narrow(), candidate), {"image": 1, "prompt": 2})
        self.assertEqual(filter_call_kwargs(Open(), candidate), candidate)


class RecipeGateTest(unittest.TestCase):
    def test_pinned_production_sampling_is_accepted(self) -> None:
        meta, _, _ = make_job()
        self.assertIsNone(recipe_mismatch(meta))

    def test_other_sampling_is_rejected(self) -> None:
        for changes in ({"seed": 2}, {"steps": 8}, {"guidance_scaled": 250}):
            meta, _, _ = make_job(**changes)
            message = sampling_mismatch(meta)
            self.assertIsNotNone(message, changes)
            self.assertIn("seed=1, steps=4, guidance_scaled=100", message)

    def test_test_recipe_leaves_sampling_to_the_request(self) -> None:
        meta, _, _ = make_job(recipe_id=RECIPE_TEST_SDNQ, seed=42, guidance_scaled=350)
        self.assertIsNone(recipe_mismatch(meta))


class SdnqFluxRunnerTest(unittest.TestCase):
    def test_render_inpaints_the_grown_hint(self) -> None:
        mods = fake_modules(image_size=(32, 16))
        runner = SdnqFluxRunner("/weights/snap", modules=mods)
        png = runner.render_png(b"png", b"hint", 32, 16, seed=1, steps=4, guidance_scaled=100)

        path, kwargs = mods.loads[0]
        self.assertEqual(path, "/weights/snap")
        self.assertEqual(kwargs, {"torch_dtype": "bfloat16", "local_files_only": True, "low_cpu_mem_usage": True})
        self.assertEqual(mods.pipeline.device, "cuda")

        call = mods.pipeline.calls[0]
        self.assertEqual(call["prompt"], RECIPE_PROMPT)
        self.assertEqual((call["width"], call["height"]), (768, 384))
        self.assertEqual(call["num_inference_steps"], 4)
        self.assertEqual(call["guidance_scale"], 1.0)
        self.assertEqual(call["output_type"], "pt")
        self.assertEqual((call["generator"].device, call["generator"].seed), ("cpu", 1))
        self.assertEqual(call["image"].size, (768, 384))
        # The hole: any ink is 255, grown by HOLE_GROWTH, resized without new grey levels.
        self.assertIn(("point", 0, 255), mods.log)
        self.assertIn(("filter", ("max", 2 * HOLE_GROWTH + 1)), mods.log)
        self.assertIn(("resize", (768, 384), "nearest"), mods.log)
        self.assertEqual(call["mask_image"].size, (768, 384))
        self.assertIs(call["image_reference"], call["image"])
        self.assertEqual(call["strength"], 1.0)

        self.assertIn(("resize", (768, 384), "bicubic"), mods.log)
        self.assertIn(("nan_to_num", {"nan": 0.0, "posinf": 1.0, "neginf": 0.0}), mods.log)
        self.assertIn(("clamp", 0.0, 1.0), mods.log)
        self.assertIn(("mul", 255.0), mods.log)
        self.assertIn(("to", "uint8"), mods.log)
        self.assertIn(("permute", 1, 2, 0), mods.log)
        # Output comes back at exactly the requested geometry with a BOX filter.
        self.assertIn(("resize", (32, 16), "box"), mods.log)
        self.assertEqual(png, encode_png_rgb8(32, 16))
        self.assertEqual(runner.render_count, 1)

        runner.render_png(b"png", b"hint", 32, 16, seed=1, steps=4, guidance_scaled=100)
        self.assertEqual(len(mods.loads), 1, "the pipeline loads once per container")

    def test_large_crop_is_not_resized(self) -> None:
        mods = fake_modules(image_size=(800, 800))
        SdnqFluxRunner("/w", modules=mods).render_png(b"png", b"hint", 800, 800, 1, 4, 100)
        self.assertFalse([entry for entry in mods.log if entry[0] == "resize"])

    def test_geometry_mismatch_is_invalid_image(self) -> None:
        mods = fake_modules(image_size=(32, 32))
        with self.assertRaises(WorkerRenderError) as caught:
            SdnqFluxRunner("/w", modules=mods).render_png(b"png", b"hint", 48, 32, 1, 4, 100)
        self.assertEqual(caught.exception.error_code, "invalid_image")

    def test_typed_load_failures(self) -> None:
        cases = [
            (fake_modules(cuda=False), "gpu_unavailable"),
            (fake_modules(load_error=RuntimeError("CUDA out of memory. Tried to allocate")), "out_of_memory"),
            (fake_modules(load_error=OSError("no such file")), "model_load_failed"),
        ]
        for mods, code in cases:
            with self.assertRaises(WorkerRenderError) as caught:
                SdnqFluxRunner("/w", modules=mods).load()
            self.assertEqual(caught.exception.error_code, code)

    def test_typed_inference_failures(self) -> None:
        for error, code in ((RuntimeError("CUDA out of memory"), "out_of_memory"), (ValueError("bad"), "inference_failed")):
            mods = fake_modules(call_error=error)
            with self.assertRaises(WorkerRenderError) as caught:
                SdnqFluxRunner("/w", modules=mods).render_png(b"png", b"hint", 32, 32, 1, 4, 100)
            self.assertEqual(caught.exception.error_code, code)


class RenderJobTest(unittest.TestCase):
    def test_valid_job_renders_and_is_digested(self) -> None:
        meta, image, hint = make_job(width=48, height=32)
        runner = FakeRunner()
        rendered = render_job(runner, meta, image, hint, production_limits())
        self.assertEqual(runner.calls, [(48, 32, 1, 4, 100)])
        self.assertEqual(rendered.digest, hashlib.sha256(rendered.png).hexdigest())

    def test_sampling_mismatch_never_reaches_the_model(self) -> None:
        meta, image, hint = make_job(seed=7)
        runner = FakeRunner()
        with self.assertRaises(WorkerRenderError) as caught:
            render_job(runner, meta, image, hint, production_limits())
        self.assertEqual(caught.exception.error_code, "unsupported_recipe")
        self.assertEqual(runner.calls, [])

    def test_payload_is_validated_again_in_the_worker(self) -> None:
        meta, image, _ = make_job()
        with self.assertRaises(WorkerRenderError) as caught:
            render_job(FakeRunner(), meta, image, image, production_limits())
        self.assertEqual(caught.exception.error_code, "invalid_request")

    def test_wrong_output_geometry_is_invalid_output(self) -> None:
        class WrongSize(FakeRunner):
            def render_png(self, image_png, hint_png, width, height, seed, steps, guidance_scaled):
                return encode_png_rgb8(width + 16, height)

        meta, image, hint = make_job()
        with self.assertRaises(WorkerRenderError) as caught:
            render_job(WrongSize(), meta, image, hint, production_limits())
        self.assertEqual(caught.exception.error_code, "invalid_output")


class WorkerRuntimeTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.runners: List[FakeRunner] = []

    def factory(self, path: str) -> FakeRunner:
        runner = FakeRunner(path)
        self.runners.append(runner)
        return runner

    def test_missing_weights_is_typed_and_recovers_after_seeding(self) -> None:
        sleeps: List[float] = []
        runtime = WorkerRuntime(
            str(self.root), runner_factory=self.factory, marker_attempts=2, marker_delay_seconds=5, sleep=sleeps.append
        )
        meta, image, hint = make_job()
        result = runtime.render(metadata_json(meta), image, hint)
        self.assertEqual((result["ok"], result["error_code"]), (False, "weights_missing"))
        self.assertEqual(sleeps, [5])
        self.assertEqual(self.runners, [])

        write_marker(self.root)
        result = runtime.render(metadata_json(meta), image, hint)
        self.assertTrue(result["ok"])
        self.assertEqual(result["digest"], hashlib.sha256(result["png"]).hexdigest())
        self.assertEqual(len(self.runners), 1)
        self.assertEqual(self.runners[0].loaded, 1)

    def test_bad_metadata_is_invalid_request(self) -> None:
        runtime = WorkerRuntime(str(self.root), runner_factory=self.factory)
        result = runtime.render("{not json", b"", b"")
        self.assertEqual(result["error_code"], "invalid_request")

    def test_load_failures_are_typed(self) -> None:
        write_marker(self.root)
        meta, image, hint = make_job()

        def gpu_missing(path: str) -> Any:
            raise WorkerRenderError("gpu_unavailable", "no CUDA")

        def broken(path: str) -> Any:
            raise RuntimeError("weights unreadable")

        for factory, code in ((gpu_missing, "gpu_unavailable"), (broken, "model_load_failed")):
            result = WorkerRuntime(str(self.root), runner_factory=factory).render(metadata_json(meta), image, hint)
            self.assertEqual(result["error_code"], code)

    def test_render_failures_are_typed(self) -> None:
        write_marker(self.root)
        meta, image, hint = make_job()

        class Failing(FakeRunner):
            def __init__(self, path: str, error: Exception):
                super().__init__(path)
                self.error = error

            def render_png(self, *args: Any) -> bytes:
                raise self.error

        for error, code in ((WorkerRenderError("out_of_memory", "OOM"), "out_of_memory"), (KeyError("x"), "inference_failed")):
            runtime = WorkerRuntime(str(self.root), runner_factory=lambda path, e=error: Failing(path, e))
            self.assertEqual(runtime.render(metadata_json(meta), image, hint)["error_code"], code)


class OutcomeTest(unittest.TestCase):
    def test_failure_is_redacted_and_bounded(self) -> None:
        result = failure("inference_failed", "Authorization: Bearer abcdefgh12345678 " + "x" * 5000)
        self.assertNotIn("abcdefgh12345678", result["message"])
        self.assertEqual(len(result["message"]), MAX_MESSAGE_CHARS)

    def test_outcome_from_result_trusts_nothing(self) -> None:
        png = encode_png_rgb8(16, 16)
        self.assertEqual(outcome_from_result(None).error_code, "invalid_output")
        self.assertEqual(outcome_from_result({"ok": True, "png": "text", "digest": "d"}).error_code, "invalid_output")
        done = outcome_from_result({"ok": True, "png": png, "digest": "d"})
        self.assertEqual((done.state, done.png, done.digest), ("completed", png, "d"))
        odd = outcome_from_result({"ok": False, "error_code": "Not A Code", "message": "token=abcdefgh1234"})
        self.assertEqual((odd.state, odd.error_code), ("failed", "worker_failed"))
        self.assertNotIn("abcdefgh1234", odd.message)
        typed = outcome_from_result({"ok": False, "error_code": "weights_missing", "message": ""})
        self.assertEqual((typed.error_code, typed.message), ("weights_missing", "The GPU worker failed"))


class RecordingStore(MemoryStore):
    def __init__(self, fail_put: bool = False, fail_get: bool = False):
        super().__init__()
        self.ops: List[Tuple[str, str]] = []
        self.fail_put = fail_put
        self.fail_get = fail_get

    def put(self, key: str, value: Dict[str, Any]) -> None:
        self.ops.append(("put", key))
        if self.fail_put:
            raise ConnectionError("store down")
        super().put(key, value)

    def get(self, key: str) -> Optional[Dict[str, Any]]:
        self.ops.append(("get", key))
        if self.fail_get:
            raise ConnectionError("store down")
        return super().get(key)


class BeginRunTest(unittest.TestCase):
    def test_marker_is_written_before_the_cancel_flag_is_read(self) -> None:
        store = RecordingStore()
        self.assertIsNone(begin_run(store, "h1", clock=lambda: 12.5))
        self.assertEqual(store.ops, [("put", run_key("h1")), ("get", job_key("h1"))])
        self.assertEqual(MemoryStore.get(store, run_key("h1")), {"started_at": 12.5})

    def test_cancelled_job_is_refused(self) -> None:
        store = RecordingStore()
        MemoryStore.put(store, job_key("h1"), {"cancel_requested": True})
        result = begin_run(store, "h1")
        self.assertEqual((result["ok"], result["error_code"]), (False, "cancelled"))

    def test_store_trouble_does_not_block_the_render(self) -> None:
        self.assertIsNone(begin_run(RecordingStore(fail_put=True), "h1"))
        self.assertIsNone(begin_run(RecordingStore(fail_get=True), "h1"))

    def test_modal_worker_body_honours_cancel(self) -> None:
        class Runtime:
            calls = 0

            def render(self, *args: Any) -> Dict[str, Any]:
                Runtime.calls += 1
                return {"ok": True}

        store = MemoryStore()
        store.put(job_key("h1"), {"cancel_requested": True})
        self.assertEqual(worker_render(Runtime(), store, "h1", "{}", b"", b"")["error_code"], "cancelled")
        self.assertEqual(Runtime.calls, 0)
        self.assertEqual(worker_render(Runtime(), store, "h2", "{}", b"", b""), {"ok": True})
        self.assertIsNotNone(store.get(run_key("h2")))


if __name__ == "__main__":
    unittest.main()
