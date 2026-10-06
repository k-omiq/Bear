"""Per-container concurrency: the VRAM budget, the render pipeline pool, the heartbeat
count and the analysis and denoise caches under concurrent inputs. All offline."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Dict, List
from unittest import mock

from deploy.cloud.common import capacity
from deploy.cloud.common.capacity import (
    ANALYSIS_MAX_INPUTS,
    GATEWAY_TIMEOUT_SECONDS,
    copies_that_fit,
    exclusive_render,
    render_copies,
    render_worker_memory_gib,
)
from deploy.cloud.common.flux import WorkerRenderError
from deploy.cloud.common.gpu import GpuHeartbeat, heartbeat_key
from deploy.cloud.common.jobs import MemoryStore
from deploy.cloud.common.manifest import (
    MODEL_PROD_FLUX,
    MODEL_PROD_FLUX_9B,
    MODEL_PROD_QWEN,
    production_model,
)
from deploy.cloud.common.worker import RunnerPool, WorkerRuntime
from deploy.cloud.tests.support import FakeRunner, make_job, metadata_json, silence_deploy_logs, write_marker

FLUX = production_model(MODEL_PROD_FLUX)
FLUX_9B = production_model(MODEL_PROD_FLUX_9B)
QWEN = production_model(MODEL_PROD_QWEN)


def _installed(*names: str) -> bool:
    return all(importlib.util.find_spec(name) is not None for name in names)


class BudgetTest(unittest.TestCase):
    def test_copies_per_model_and_gpu(self) -> None:
        # 23,034 MiB x 0.85 = 19,579 MiB usable; 2 x 7,180 fits, 3 does not.
        self.assertEqual(render_copies(FLUX, "L4"), 2)
        self.assertEqual(render_copies(FLUX, "A10"), 2)
        self.assertEqual(render_copies(FLUX, "RTX4090"), 2)
        # Room for five on an L40S; capped.
        self.assertEqual(render_copies(FLUX, "L40S"), capacity.MAX_RENDER_COPIES)
        # Qwen and FLUX 9B: two copies only on an L40S, by decision; one elsewhere.
        self.assertEqual(render_copies(QWEN, "L40S"), 2)
        self.assertEqual(render_copies(QWEN, "RTX5090"), 1)
        self.assertIsNone(FLUX_9B.render_peak_vram_mib)
        self.assertEqual(render_copies(FLUX_9B, "RTX5090"), 1)
        self.assertEqual(render_copies(FLUX_9B, "l40s"), 2)
        self.assertEqual(render_copies(FLUX_9B, "L40S"), 2)

    def test_unknown_gpu_or_peak_is_one_copy(self) -> None:
        self.assertEqual(copies_that_fit(7180, "B200"), 1)
        self.assertEqual(copies_that_fit(None, "L4"), 1)
        self.assertEqual(copies_that_fit(0, "L4"), 1)
        # A model that does not even fit once still gets its one copy.
        self.assertEqual(copies_that_fit(30000, "L4"), 1)
        self.assertEqual(copies_that_fit(7180, "l4"), 2)

    def test_headroom_decides_the_boundary(self) -> None:
        self.assertEqual(copies_that_fit(7180, "L4", cap=8), 2)
        self.assertEqual(copies_that_fit(7180, "L4", cap=8, headroom=0.0), 3)

    def test_host_memory_per_provider(self) -> None:
        # Modal: one process, copies loaded one after another; one more resident copy.
        self.assertEqual(render_worker_memory_gib(FLUX, "L4", "modal"), 12 + 7)
        # Beam: one worker process per copy, all loading at once.
        self.assertEqual(render_worker_memory_gib(FLUX, "L4", "beam"), 24)
        # One copy: unchanged.
        self.assertEqual(render_worker_memory_gib(QWEN, "L40S", "modal"), 32 + 24)
        self.assertEqual(render_worker_memory_gib(FLUX_9B, "L40S", "modal"), 24 + 24)

    def test_large_working_sizes_run_alone(self) -> None:
        self.assertFalse(exclusive_render(768, 576))
        self.assertFalse(exclusive_render(1024, 1024))
        self.assertTrue(exclusive_render(1025, 1024))
        self.assertTrue(exclusive_render(2048, 2048))

    def test_gateway_outlasts_every_synchronous_wait(self) -> None:
        # The legacy synchronous wait ends before Modal's 303, and the gateway after it.
        self.assertLess(capacity.SYNC_GPU_WAIT_SECONDS, capacity.MODAL_WEB_REDIRECT_SECONDS)
        self.assertGreater(GATEWAY_TIMEOUT_SECONDS, capacity.SYNC_GPU_WAIT_SECONDS)
        from deploy.cloud.modal.backend import ANALYSIS_CALL_EXPIRY_SECONDS, GPU_JOB_LONGEST_CALL_SECONDS
        # A polled job stays tracked for a stop and an idle release while it can run.
        self.assertGreater(ANALYSIS_CALL_EXPIRY_SECONDS, GPU_JOB_LONGEST_CALL_SECONDS)
        self.assertGreater(ANALYSIS_CALL_EXPIRY_SECONDS, GATEWAY_TIMEOUT_SECONDS)
        self.assertGreaterEqual(ANALYSIS_MAX_INPUTS, 3, "two denoise pages and a tile at once")


class RunnerPoolTest(unittest.TestCase):
    def test_concurrent_borrowers_never_share_a_copy(self) -> None:
        runners = [object(), object()]
        pool = RunnerPool(runners)
        in_use: Dict[int, int] = {}
        peak = [0]
        lock = threading.Lock()
        errors: List[str] = []

        def borrower() -> None:
            for _ in range(25):
                with pool.borrow() as runner:
                    with lock:
                        if in_use.get(id(runner)):
                            errors.append("shared")
                        in_use[id(runner)] = 1
                        peak[0] = max(peak[0], sum(in_use.values()))
                    time.sleep(0.001)
                    with lock:
                        in_use[id(runner)] = 0

        threads = [threading.Thread(target=borrower) for _ in range(6)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(10)
        self.assertEqual(errors, [])
        self.assertEqual(peak[0], 2, "both copies were in use at once")

    def test_exclusive_waits_for_every_copy_and_holds_them(self) -> None:
        pool = RunnerPool(["a", "b"])
        events: List[str] = []
        first = pool.borrow()
        first.__enter__()

        def exclusive() -> None:
            with pool.borrow(exclusive=True):
                events.append("exclusive")
                time.sleep(0.02)
                events.append("exclusive done")

        def shared() -> None:
            with pool.borrow():
                events.append("shared")

        big = threading.Thread(target=exclusive)
        big.start()
        time.sleep(0.02)
        # A copy is free, but the waiting exclusive render keeps new shared ones out.
        small = threading.Thread(target=shared)
        small.start()
        time.sleep(0.02)
        self.assertEqual(events, [])
        first.__exit__(None, None, None)
        big.join(5)
        small.join(5)
        self.assertEqual(events, ["exclusive", "exclusive done", "shared"])

    def test_empty_pool_is_refused(self) -> None:
        with self.assertRaises(ValueError):
            RunnerPool([])


class GatedRunner(FakeRunner):
    """A FakeRunner that records overlap: renders wait at a barrier for a partner."""

    active = 0
    peak = 0
    lock = threading.Lock()

    def __init__(self, path: str = "", barrier: Any = None):
        super().__init__(path)
        self.barrier = barrier
        self.busy = False
        self.warmups = 0

    def warmup(self) -> None:
        self.warmups += 1

    def render_png(self, *args: Any) -> bytes:
        cls = type(self)
        with cls.lock:
            assert not self.busy, "one copy served two renders at once"
            self.busy = True
            cls.active += 1
            cls.peak = max(cls.peak, cls.active)
        try:
            if self.barrier is not None:
                self.barrier.wait(5)
            return super().render_png(*args)
        finally:
            with cls.lock:
                cls.active -= 1
                self.busy = False


class WorkerPoolRuntimeTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        write_marker(self.root)
        GatedRunner.active = GatedRunner.peak = 0

    def test_copies_load_warm_and_render_side_by_side(self) -> None:
        barrier = threading.Barrier(2)
        built: List[GatedRunner] = []
        runtime = WorkerRuntime(str(self.root), copies=2,
                                runner_factory=lambda p: built.append(GatedRunner(p, barrier)) or built[-1])
        runtime.load_for_snapshot()
        self.assertEqual(len(built), 2)
        self.assertEqual([(r.loaded, r.warmups) for r in built], [(1, 1), (1, 1)])
        self.assertIs(runtime.runner, built[0])
        meta, image, hint = make_job()
        results: List[Dict[str, Any]] = []
        threads = [threading.Thread(target=lambda: results.append(runtime.render(metadata_json(meta), image, hint)))
                   for _ in range(2)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(10)
        self.assertEqual([r["ok"] for r in results], [True, True])
        self.assertEqual(GatedRunner.peak, 2)
        self.assertEqual(sorted(len(r.calls) for r in built), [1, 1])
        self.assertEqual(results[0]["digest"], results[1]["digest"])

    def test_a_copy_that_does_not_load_leaves_the_others_serving(self) -> None:
        built: List[Any] = []

        def factory(path: str) -> Any:
            if built:
                raise RuntimeError("CUDA out of memory")
            built.append(FakeRunner(path))
            return built[-1]

        runtime = WorkerRuntime(str(self.root), copies=2, runner_factory=factory)
        self.addCleanup(silence_deploy_logs())
        runtime.load_for_snapshot()
        self.assertEqual(runtime.pool.size, 1)
        meta, image, hint = make_job()
        self.assertTrue(runtime.render(metadata_json(meta), image, hint)["ok"])

    def test_concurrent_first_renders_load_once(self) -> None:
        built: List[FakeRunner] = []

        def slow(path: str) -> FakeRunner:
            time.sleep(0.02)
            built.append(FakeRunner(path))
            return built[-1]

        runtime = WorkerRuntime(str(self.root), copies=2, runner_factory=slow)
        meta, image, hint = make_job()
        results: List[Dict[str, Any]] = []
        threads = [threading.Thread(target=lambda: results.append(runtime.render(metadata_json(meta), image, hint)))
                   for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(10)
        self.assertEqual(len(built), 2, "two copies, not one per render")
        self.assertTrue(all(r["ok"] for r in results))

    def test_large_render_runs_alone(self) -> None:
        runtime = WorkerRuntime(str(self.root), copies=2, runner_factory=GatedRunner)
        runtime.load()
        seen: List[bool] = []
        original = runtime._render_on

        def recording(pool: Any, exclusive: bool, *args: Any) -> Any:
            seen.append(exclusive)
            return original(pool, exclusive, *args)

        runtime._render_on = recording  # type: ignore[method-assign]
        small = make_job()
        large = make_job(width=1040, height=1024)
        self.assertTrue(runtime.render(metadata_json(small[0]), small[1], small[2])["ok"])
        self.assertTrue(runtime.render(metadata_json(large[0]), large[1], large[2])["ok"])
        self.assertEqual(seen, [False, True])

    def test_out_of_memory_beside_another_render_retries_alone(self) -> None:
        attempts: List[bool] = []

        class Flaky(FakeRunner):
            def render_png(self, *args: Any) -> bytes:
                if not attempts:
                    attempts.append(True)
                    raise WorkerRenderError("out_of_memory", "CUDA out of memory")
                return super().render_png(*args)

        runtime = WorkerRuntime(str(self.root), copies=2, runner_factory=Flaky)
        meta, image, hint = make_job()
        self.addCleanup(silence_deploy_logs())
        result = runtime.render(metadata_json(meta), image, hint)
        self.assertTrue(result["ok"], result)

    def test_one_copy_keeps_the_old_behaviour(self) -> None:
        class Oom(FakeRunner):
            def render_png(self, *args: Any) -> bytes:
                raise WorkerRenderError("out_of_memory", "CUDA out of memory")

        runtime = WorkerRuntime(str(self.root), runner_factory=Oom)
        meta, image, hint = make_job()
        self.assertEqual(runtime.render(metadata_json(meta), image, hint)["error_code"], "out_of_memory")
        self.assertEqual(runtime.pool.size, 1)


class HeartbeatCountTest(unittest.TestCase):
    def test_idle_only_after_the_last_call_ends(self) -> None:
        store = MemoryStore()
        beat = GpuHeartbeat(store, "render", "L4", 120, 1200, 600)
        beat.idle()
        states: List[str] = []
        first = beat.serving()
        first.__enter__()
        states.append(store.get(heartbeat_key("render"))["state"])
        second = beat.serving()
        second.__enter__()
        first.__exit__(None, None, None)
        states.append(store.get(heartbeat_key("render"))["state"])
        self.assertEqual(beat.calls, 1)
        second.__exit__(None, None, None)
        states.append(store.get(heartbeat_key("render"))["state"])
        self.assertEqual(states, ["busy", "busy", "idle"])
        self.assertEqual(beat.calls, 0)

    def test_a_failing_call_still_counts_down(self) -> None:
        store = MemoryStore()
        beat = GpuHeartbeat(store, "analysis", "L4", 120, 600, 600)
        with self.assertRaises(RuntimeError):
            with beat.serving():
                raise RuntimeError("tile failed")
        self.assertEqual((beat.calls, store.get(heartbeat_key("analysis"))["state"]), (0, "idle"))

    def test_many_threads_end_idle(self) -> None:
        store = MemoryStore()
        beat = GpuHeartbeat(store, "render", "L4", 120, 1200, 600)

        def call() -> None:
            for _ in range(20):
                with beat.serving():
                    pass

        threads = [threading.Thread(target=call) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(10)
        self.assertEqual((beat.calls, store.get(heartbeat_key("render"))["state"]), (0, "idle"))


@unittest.skipUnless(_installed("numpy", "PIL"), "numpy and Pillow are not installed")
class AnalysisSessionLockTest(unittest.TestCase):
    def test_concurrent_tiles_create_one_session_per_graph(self) -> None:
        from deploy.cloud.common.analysis_runtime import OnDemandAnalysis
        created: List[str] = []

        def factory(path: str) -> Any:
            time.sleep(0.02)
            created.append(path)
            return SimpleNamespace(path=path)

        runtime = OnDemandAnalysis("/nowhere", [], session_factory=factory)
        got: List[Any] = []
        threads = [threading.Thread(target=lambda: got.append(runtime._session("/g/encoder.onnx")))
                   for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(10)
        self.assertEqual(created, ["/g/encoder.onnx"])
        self.assertEqual(len({id(item) for item in got}), 1)

    def test_concurrent_first_tiles_load_the_graphs_once(self) -> None:
        from deploy.cloud.common import analysis_runtime
        loads: List[int] = []

        def installed(root: str, selected: Any, verify_hash: bool) -> Any:
            time.sleep(0.02)
            loads.append(1)
            return {}, {}

        runtime = analysis_runtime.OnDemandAnalysis("/nowhere", [], session_factory=lambda p: None)
        errors: List[Exception] = []

        def first_tile() -> None:
            try:
                runtime._load()
            except RuntimeError as error:
                errors.append(error)

        with mock.patch.object(analysis_runtime, "installed_graphs", installed):
            threads = [threading.Thread(target=first_tile) for _ in range(3)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(10)
        # Nothing installed, so each tries in turn, but never two verifications at once.
        self.assertEqual(len(errors), 3)
        self.assertEqual(len(loads), 3)


class _FakeOrt:
    """onnxruntime surface SessionCache touches when it loads a waifu2x model."""

    created: List[str] = []
    lock = threading.Lock()

    class SessionOptions:
        log_severity_level = 0

    class InferenceSession:
        def __init__(self, path: str, options: Any = None, providers: Any = None):
            time.sleep(0.01)
            with _FakeOrt.lock:
                _FakeOrt.created.append(path)
            self.path = path


class DenoiseCacheLockTest(unittest.TestCase):
    def setUp(self) -> None:
        _FakeOrt.created = []
        fake_torch = SimpleNamespace(cuda=SimpleNamespace(is_available=lambda: False, empty_cache=lambda: None))
        patcher = mock.patch.dict(sys.modules, {"onnxruntime": _FakeOrt, "torch": fake_torch})
        patcher.start()
        self.addCleanup(patcher.stop)

    def _cache(self, count: int) -> Any:
        from deploy.cloud.common import denoise
        models = [denoise.DenoiseModel(**{**denoise.SEAM_FILTER.__dict__, "model_id": f"m{i}", "family": "waifu2x"})
                  for i in range(count)]
        cache = denoise.SessionCache({m.model_id: Path(f"/w/{m.model_id}.onnx") for m in models})
        return cache, models

    def test_concurrent_gets_load_a_model_once(self) -> None:
        cache, models = self._cache(1)
        threads = [threading.Thread(target=cache.get, args=(models[0],)) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(10)
        self.assertEqual(_FakeOrt.created, ["/w/m0.onnx"])

    def test_a_pinned_model_is_never_evicted(self) -> None:
        cache, models = self._cache(6)
        limit = cache.MAX_LOADED
        with cache.pinned(models[0]) as (session,):
            for model in models[1:limit + 1]:
                cache.get(model)
            self.assertIn("m0", cache._loaded, "the page using m0 keeps it")
            self.assertLessEqual(len(cache._loaded), limit)
            self.assertIs(cache.get(models[0]), session)
        # Unpinned, it is the least recently used and goes first.
        cache.get(models[0])
        for model in models[1:]:
            cache.get(model)
        self.assertNotIn("m0", cache._loaded)

    def test_every_model_pinned_lets_the_cache_grow_until_pages_end(self) -> None:
        cache, models = self._cache(6)
        limit = cache.MAX_LOADED
        with cache.pinned(*models[:limit]):
            cache.get(models[limit])
            self.assertEqual(len(cache._loaded), limit + 1)
        cache.get(models[limit + 1])
        self.assertLessEqual(len(cache._loaded), limit)
        self.assertEqual(cache._pins, {})


if __name__ == "__main__":
    unittest.main()
