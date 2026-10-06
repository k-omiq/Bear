"""Memory snapshots of the Modal GPU classes, offline.

Three layers:
- the SDK facts the design leans on (snapshot taken only after every snap=True enter
  returned; an exception there fails the container first), read from the installed
  modal sources so an SDK upgrade that changes them fails here;
- the options each app definition passes to Modal, recorded while the app imports;
- the real `Worker` and `AnalysisGPU` classes driven through a small lifecycle that
  mirrors the SDK order (snap=True enters, snapshot, snap=False enters, calls), with
  a restore modelled as a deep copy of the instance at snapshot time. `Worker` has
  its snapshot off, so each of its cold starts runs every enter in order.
"""

from __future__ import annotations

import copy
import importlib
import inspect
import pickle
import sys
import tempfile
import time
import unittest
import warnings
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Dict, List, Optional
from unittest import mock

from deploy.cloud.common.flux import WARMUP_SIDE, SdnqFluxRunner
from deploy.cloud.common.capacity import ANALYSIS_MAX_INPUTS, GATEWAY_TIMEOUT_SECONDS
from deploy.cloud.common.gpu import GpuHeartbeat, heartbeat_key, release_key
from deploy.cloud.common.worker import NOT_READY_TAG, WorkerNotReady, WorkerRuntime, not_ready_code
from deploy.cloud.modal.backend import (
    ModalDictStore,
    ModalWorkerDispatcher,
    prepare_render_container,
    start_gpu_container,
)
from deploy.cloud.modal.settings import ModalSettings
from deploy.cloud.tests.support import (
    FakeFunctionCall,
    FakeModalDict,
    FakeRunner,
    ModalErrors,
    silence_deploy_logs,
    write_marker,
)
from deploy.cloud.tests.test_adapters import _installed, patched_environ
from deploy.cloud.tests.test_worker import FakeImage, fake_modules

_restore_logs = None


def setUpModule() -> None:
    global _restore_logs
    _restore_logs = silence_deploy_logs()


def tearDownModule() -> None:
    _restore_logs()


class Clock:
    def __init__(self, now: float = 1_000.0):
        self.now = now

    def __call__(self) -> float:
        return self.now


class WarmRunner(FakeRunner):
    def __init__(self, snapshot_path: str = "", warmup_error: Optional[Exception] = None):
        super().__init__(snapshot_path)
        self.warmups = 0
        self.warmup_error = warmup_error

    def warmup(self) -> None:
        self.warmups += 1
        if self.warmup_error is not None:
            raise self.warmup_error


class WorkerNotReadyTest(unittest.TestCase):
    def test_startup_retry_with_no_provider_output_ends_from_recorded_failure(self) -> None:
        from deploy.cloud.modal.backend import RENDER_STARTUP_FAILURE_KEY
        call = FakeFunctionCall("fc-1")  # TimeoutError: Modal is retrying initialization.
        store = ModalDictStore(FakeModalDict())
        store.put("job:h1", {"created_at": 1000})
        dispatcher = ModalWorkerDispatcher(lambda *a: call, {"fc-1": call}.__getitem__, ModalErrors, store)
        store.put(RENDER_STARTUP_FAILURE_KEY, {"at": 999, "error_code": "weights_missing", "message": "old failure"})
        self.assertEqual(dispatcher.poll("h1", "fc-1").state, "pending")
        self.assertEqual(call.cancels, [])
        store.put(RENDER_STARTUP_FAILURE_KEY, {"at": 1001, "error_code": "weights_missing", "message": "seed first"})
        outcome = dispatcher.poll("h1", "fc-1")
        self.assertEqual((outcome.state, outcome.error_code), ("failed", "weights_missing"))
        self.assertEqual(call.cancels, [{"terminate_containers": False}])


    def test_code_survives_pickle_and_text(self) -> None:
        error = WorkerNotReady("weights_missing", "seed first")
        again = pickle.loads(pickle.dumps(error))
        self.assertEqual((type(again), again.error_code, again.detail), (WorkerNotReady, "weights_missing", "seed first"))
        self.assertEqual(not_ready_code(again), "weights_missing")
        # Modal falls back to a RemoteError / ExecutionError carrying only the text.
        self.assertEqual(not_ready_code(RuntimeError(f"Traceback ... {error!s}")), "weights_missing")
        self.assertIsNone(not_ready_code(RuntimeError("boom")))
        self.assertIsNone(not_ready_code(RuntimeError(f"{NOT_READY_TAG}Bad Code] x")))

    def test_dispatcher_keeps_the_typed_code_of_a_refused_start(self) -> None:
        call = FakeFunctionCall("fc-1")
        dispatcher = ModalWorkerDispatcher(lambda *a: call, {"fc-1": call}.__getitem__, ModalErrors,
                                           ModalDictStore(FakeModalDict()))
        call.error = WorkerNotReady("weights_missing", "Model weights are not on the volume yet.")
        outcome = dispatcher.poll("h1", "fc-1")
        self.assertEqual((outcome.state, outcome.error_code, outcome.message),
                         ("failed", "weights_missing", "Model weights are not on the volume yet."))
        call.error = ModalErrors.RemoteError(f"WorkerNotReady: {NOT_READY_TAG}model_load_failed] no cuda")
        self.assertEqual(dispatcher.poll("h1", "fc-1").error_code, "model_load_failed")
        call.error = ModalErrors.RemoteError("something else")
        self.assertEqual(dispatcher.poll("h1", "fc-1").error_code, "worker_failed")


class LoadForSnapshotTest(unittest.TestCase):
    """Requirement: the snapshot phase never completes with the pipeline unloaded."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = self._tmp.name

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_unseeded_volume_raises_and_never_builds_a_runner(self) -> None:
        built: List[str] = []
        runtime = WorkerRuntime(self.root, runner_factory=lambda path: built.append(path) or WarmRunner(path),
                                marker_attempts=2, sleep=lambda s: None)
        with self.assertRaises(WorkerNotReady) as caught:
            runtime.load_for_snapshot()
        self.assertEqual(caught.exception.error_code, "weights_missing")
        self.assertEqual((built, runtime.loaded), ([], False))

    def test_load_failure_raises_with_its_code(self) -> None:
        write_marker(self.root)

        class Broken(WarmRunner):
            def load(self) -> None:
                from deploy.cloud.common.flux import WorkerRenderError
                raise WorkerRenderError("gpu_unavailable", "no CUDA")

        with self.assertRaises(WorkerNotReady) as caught:
            WorkerRuntime(self.root, runner_factory=Broken).load_for_snapshot()
        self.assertEqual(caught.exception.error_code, "gpu_unavailable")

    def test_warmup_failure_raises_so_a_broken_context_is_not_snapshotted(self) -> None:
        write_marker(self.root)
        runtime = WorkerRuntime(self.root, runner_factory=lambda p: WarmRunner(p, RuntimeError("CUDA error")))
        with self.assertRaises(WorkerNotReady) as caught:
            runtime.load_for_snapshot()
        self.assertEqual(caught.exception.error_code, "model_load_failed")

    def test_seeded_volume_loads_and_warms_once(self) -> None:
        write_marker(self.root)
        runners: List[WarmRunner] = []
        runtime = WorkerRuntime(self.root, runner_factory=lambda p: runners.append(WarmRunner(p)) or runners[-1])
        runtime.load_for_snapshot()
        self.assertTrue(runtime.loaded)
        self.assertEqual((runners[0].loaded, runners[0].warmups), (1, 1))

    def test_sdnq_warmup_is_one_short_render_at_the_working_size(self) -> None:
        mods = fake_modules(image_size=(WARMUP_SIDE, WARMUP_SIDE))
        mods.Image.new = lambda mode, size, color: FakeImage(size, mode, mods.log)
        runner = SdnqFluxRunner("/w", modules=mods)
        runner.load()
        runner.warmup()
        call = mods.pipeline.calls[0]
        self.assertEqual((call["width"], call["height"], call["num_inference_steps"]), (768, 768, 1))
        self.assertEqual((call["generator"].device, call["generator"].seed), ("cpu", 0))
        self.assertEqual(runner.render_count, 0, "the warmup is not a job")


class ContainerLifecycleHelpersTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = self._tmp.name
        self.store = ModalDictStore(FakeModalDict())
        self.clock = Clock()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def heartbeat(self) -> GpuHeartbeat:
        return GpuHeartbeat(self.store, "render", "L4", 120, 1200, 600, clock=self.clock)

    def runtime(self) -> WorkerRuntime:
        return WorkerRuntime(self.root, runner_factory=WarmRunner, sleep=lambda s: None)

    def test_a_failed_snapshot_phase_leaves_no_starting_heartbeat(self) -> None:
        with self.assertRaises(WorkerNotReady):
            prepare_render_container(self.store, self.heartbeat, self.runtime, self.clock)
        self.assertIsNone(self.store.get(heartbeat_key("render")))
        from deploy.cloud.modal.backend import RENDER_STARTUP_FAILURE_KEY
        self.assertEqual(self.store.get(RENDER_STARTUP_FAILURE_KEY)["error_code"], "weights_missing")
        write_marker(self.root)
        prepare_render_container(self.store, self.heartbeat, self.runtime, self.clock)
        self.assertIsNone(self.store.get(RENDER_STARTUP_FAILURE_KEY))

    def test_a_release_started_container_raises_before_loading(self) -> None:
        write_marker(self.root)
        self.store.put(release_key("render"), {"at": self.clock.now - 5})
        built: List[int] = []
        with self.assertRaises(WorkerNotReady) as caught:
            prepare_render_container(self.store, self.heartbeat, lambda: built.append(1), self.clock)
        self.assertEqual((caught.exception.error_code, built), ("released", []))
        self.assertIsNone(self.store.get(heartbeat_key("render")))

    def test_a_loaded_worker_survives_failure_to_clear_an_old_failure(self) -> None:
        from deploy.cloud.modal.backend import RENDER_STARTUP_FAILURE_KEY
        write_marker(self.root)
        original_delete = self.store.delete

        def unavailable(key: str) -> None:
            if key == RENDER_STARTUP_FAILURE_KEY:
                raise OSError("Store temporarily unavailable")
            original_delete(key)

        with mock.patch.object(self.store, "delete", side_effect=unavailable):
            runtime = prepare_render_container(self.store, self.heartbeat, self.runtime, self.clock)
        self.assertTrue(runtime.loaded)

    def test_loading_shows_starting_then_start_writes_its_own_heartbeat(self) -> None:
        write_marker(self.root)
        runtime = prepare_render_container(self.store, self.heartbeat, self.runtime, self.clock)
        loading = self.store.get(heartbeat_key("render"))
        self.assertEqual(loading["state"], "starting")
        self.clock.now += 50
        live = self.heartbeat()
        self.assertTrue(start_gpu_container(live, runtime, self.clock))
        doc = self.store.get(heartbeat_key("render"))
        self.assertEqual((doc["state"], doc["started_at"], doc["container"]), ("idle", self.clock.now, live.container))
        self.assertNotEqual(doc["container"], loading["container"])

    def test_start_loads_on_demand_only_when_the_snapshot_phase_did_not(self) -> None:
        write_marker(self.root)
        runtime = self.runtime()
        states: List[str] = []
        live = self.heartbeat()
        live._write = states.append  # type: ignore[method-assign]
        start_gpu_container(live, runtime, self.clock)
        self.assertEqual((states, runtime.loaded), (["starting", "idle"], True))
        states.clear()
        start_gpu_container(live, runtime, self.clock)
        self.assertEqual(states, ["idle"])

    def test_release_marker_is_read_on_every_start(self) -> None:
        self.store.put(release_key("render"), {"at": self.clock.now - 1})
        runtime = self.runtime()
        self.assertFalse(start_gpu_container(self.heartbeat(), runtime, self.clock))
        self.assertIsNone(self.store.get(heartbeat_key("render")))
        self.assertFalse(runtime.loaded)


# ---------------------------------------------------------------------------
# The SDK behaviour the snapshot phase relies on
# ---------------------------------------------------------------------------


@unittest.skipUnless(_installed("modal"), "modal is not installed")
class SdkSnapshotFidelityTest(unittest.TestCase):
    def test_snapshot_is_taken_only_after_every_snap_enter_returned(self) -> None:
        from modal._runtime import user_code_imports as uci
        context = inspect.getsource(uci.Service.lifecycle_context)
        pre, snap, post = (context.index(needle) for needle in (
            "with self.lifecycle_presnapshot(", "maybe_snapshot(self.function_def", "with self.lifecycle_postsnapshot("))
        self.assertLess(pre, snap)
        self.assertLess(snap, post)
        presnapshot = inspect.getsource(uci._LifecycleManager.lifecycle_presnapshot)
        self.assertIn("ENTER_PRE_SNAPSHOT", presnapshot)
        self.assertIn("call_lifecycle_functions", presnapshot)
        postsnapshot = inspect.getsource(uci._LifecycleManager.lifecycle_postsnapshot)
        self.assertIn("ENTER_POST_SNAPSHOT", postsnapshot)

    def test_an_exception_in_an_enter_fails_the_container(self) -> None:
        from modal._runtime import task_lifecycle_manager as tlm
        from modal._runtime import user_code_imports as uci
        self.assertIn("handle_task_lifecycle_exception", inspect.getsource(uci.call_lifecycle_functions))
        handler = inspect.getsource(tlm._TaskLifecycleManager.handle_task_lifecycle_exception)
        self.assertIn("raise UserException()", handler)

    def test_gpu_snapshot_is_read_from_the_function_definition(self) -> None:
        from modal._runtime import task_lifecycle_manager as tlm
        source = inspect.getsource(tlm._TaskLifecycleManager.memory_snapshot)
        self.assertIn("_experimental_enable_gpu_snapshot", source)
        self.assertIn("CudaCheckpointSession", source)

    def test_snap_enter_requires_enable_memory_snapshot(self) -> None:
        import modal
        self.assertIn("enable_memory_snapshot", inspect.signature(modal.App.cls).parameters)
        self.assertIn("experimental_options", inspect.signature(modal.App.cls).parameters)
        self.assertIn("snap", inspect.signature(modal.enter).parameters)


# ---------------------------------------------------------------------------
# The app definitions
# ---------------------------------------------------------------------------


@unittest.skipUnless(_installed("modal"), "modal is not installed")
class ModalAppSnapshotTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        import modal._functions
        import modal.experimental  # noqa: F401  (the container runtime imports it too)
        cls._tmp = tempfile.TemporaryDirectory()
        settings = ModalSettings.for_installation("mc-ab12cd", "mc-ab12cd", gpu="l40s", idle_seconds=240)
        env = {**settings.to_env(), "MODAL_CONFIG_PATH": str(Path(cls._tmp.name) / "absent.toml"),
               "MODAL_SERVER_URL": "http://127.0.0.1:9"}
        original = modal._functions._Function._from_local
        cls.options: Dict[str, Dict[str, Any]] = {}

        def recording(info: Any, *args: Any, **kwargs: Any) -> Any:
            cls.options[info.get_tag()] = kwargs
            return original(info, *args, **kwargs)

        sys.modules.pop("deploy.cloud.modal.app", None)
        with patched_environ(env), warnings.catch_warnings(), \
                mock.patch.object(modal._functions._Function, "_from_local", staticmethod(recording)):
            warnings.simplefilter("error")
            cls.module = importlib.import_module("deploy.cloud.modal.app")

    @classmethod
    def tearDownClass(cls) -> None:
        sys.modules.pop("deploy.cloud.modal.app", None)
        cls._tmp.cleanup()

    def test_snapshot_options_per_definition(self) -> None:
        opts = self.options
        self.assertEqual(sorted(opts), ["AnalysisGPU.*", "Worker.*", "gateway", "seed_weights"])
        # Off after the live run rebuilt a GPU snapshot on every cold start (docs/findings.md).
        self.assertFalse(opts["Worker.*"]["enable_memory_snapshot"])
        self.assertEqual(opts["Worker.*"]["experimental_options"], {})
        self.assertTrue(opts["AnalysisGPU.*"]["enable_memory_snapshot"])
        self.assertEqual(opts["AnalysisGPU.*"]["experimental_options"], {})
        for name in ("gateway", "seed_weights"):
            self.assertFalse(opts[name].get("enable_memory_snapshot", False), name)

    def test_concurrency_follows_the_memory_budget(self) -> None:
        # FLUX.2 Klein 4B on an L40S: two pipeline copies, two renders at once, and the
        # host memory for the second copy (common/capacity.py).
        opts = self.options
        self.assertEqual(self.module.RENDER_COPIES, 2)
        self.assertEqual(opts["Worker.*"]["max_concurrent_inputs"], 2)
        self.assertEqual(opts["Worker.*"]["memory"], (12 + 7) * 1024)
        self.assertEqual(opts["AnalysisGPU.*"]["max_concurrent_inputs"], ANALYSIS_MAX_INPUTS)
        self.assertEqual(opts["gateway"]["max_concurrent_inputs"], 32)
        self.assertEqual(opts["gateway"]["timeout"], GATEWAY_TIMEOUT_SECONDS)

    def _user_cls(self, name: str) -> type:
        from modal._utils.async_utils import synchronizer
        return synchronizer._translate_in(getattr(self.module, name))._user_cls

    def _enters(self, name: str) -> Dict[str, List[str]]:
        from modal._partial_function import _find_partial_methods_for_user_cls, _PartialFunctionFlags as F
        user_cls = self._user_cls(name)
        return {phase: sorted(_find_partial_methods_for_user_cls(user_cls, flag))
                for phase, flag in (("snap", F.ENTER_PRE_SNAPSHOT), ("start", F.ENTER_POST_SNAPSHOT))}

    def _enters_in_order(self, name: str) -> List[str]:
        from modal._partial_function import _find_partial_methods_for_user_cls, _PartialFunctionFlags as F
        return list(_find_partial_methods_for_user_cls(self._user_cls(name), F.ENTER_POST_SNAPSHOT))

    def test_enter_split(self) -> None:
        # Snapshot off: both run on every start, load first (class definition order).
        self.assertEqual(self._enters("Worker"), {"snap": [], "start": ["load", "start"]})
        self.assertEqual(list(self._enters_in_order("Worker")), ["load", "start"])
        self.assertEqual(self._enters("AnalysisGPU"), {"snap": ["prepare"], "start": ["start"]})

    # -- lifecycle ------------------------------------------------------------

    def _patched(self, root: str, store: FakeModalDict):
        runtime_cls = self.module.WorkerRuntime

        def runtime(*args: Any, **kwargs: Any) -> WorkerRuntime:
            return runtime_cls(*args, runner_factory=WarmRunner, sleep=lambda s: None, **kwargs)

        return mock.patch.multiple(
            self.module,
            WEIGHTS_MOUNT=root,
            jobs_dict=store,
            weights_volume=SimpleNamespace(reload=lambda: None),
            WorkerRuntime=runtime,
        )

    def _methods(self, instance: Any, flag: Any) -> List[Any]:
        from modal._partial_function import _find_partial_methods_for_user_cls
        return [pf.raw_f for pf in _find_partial_methods_for_user_cls(type(instance), flag).values()]

    def snapshot(self, name: str) -> Any:
        """Run the snap=True enters as the SDK does; the returned instance is the snapshot."""
        from modal._partial_function import _PartialFunctionFlags as F
        user_cls = self._user_cls(name)
        instance = user_cls.__new__(user_cls)
        for method in self._methods(instance, F.ENTER_PRE_SNAPSHOT):
            method(instance)  # an exception here means no snapshot
        return instance

    def restore(self, snapshot: Any) -> Any:
        from modal._partial_function import _PartialFunctionFlags as F
        instance = copy.deepcopy(snapshot)
        for method in self._methods(instance, F.ENTER_POST_SNAPSHOT):
            method(instance)
        return instance

    def cold_start(self, name: str) -> Any:
        """A start with no snapshot: every enter runs, in the SDK's order."""
        return self.restore(self.snapshot(name))

    def test_unseeded_worker_never_starts(self) -> None:
        store = FakeModalDict()
        with tempfile.TemporaryDirectory() as root, self._patched(root, store):
            with self.assertRaises(WorkerNotReady) as caught:
                self.cold_start("Worker")
            self.assertEqual(caught.exception.error_code, "weights_missing")
            self.assertNotIn(heartbeat_key("render"), store.data)
            # Seeding later: the next cold start loads and warms the pipeline.
            write_marker(root)
            live = self.cold_start("Worker")
            self.assertTrue(live.runtime.loaded)
            self.assertEqual(live.runtime.runner.warmups, 1)
            self.assertEqual(store.data[heartbeat_key("render")]["state"], "idle")

    def test_every_cold_start_loads_once_and_gets_its_own_heartbeat(self) -> None:
        store = FakeModalDict()
        with tempfile.TemporaryDirectory() as root, self._patched(root, store):
            write_marker(root)
            started_at = time.time()
            first = self.cold_start("Worker")
            doc = store.data[heartbeat_key("render")]
            self.assertEqual((doc["state"], doc["container"]), ("idle", first.heartbeat.container))
            self.assertGreaterEqual(doc["started_at"], started_at)
            self.assertEqual(first.runtime.runner.loaded, 1, "start does not load a second time")
            second = self.cold_start("Worker")
            self.assertNotEqual(first.heartbeat.container, second.heartbeat.container)
            self.assertEqual(store.data[heartbeat_key("render")]["container"], second.heartbeat.container)
            with mock.patch.object(self.module.modal.experimental, "stop_fetching_inputs") as stop:
                self.assertEqual(self._call(second, "release"), {"ok": True})
            stop.assert_called_once_with()
            self.assertIn(heartbeat_key("render"), store.data)
            second.unload()
            self.assertNotIn(heartbeat_key("render"), store.data)

    def test_a_release_started_cold_start_is_refused_before_loading(self) -> None:
        store = FakeModalDict()
        with tempfile.TemporaryDirectory() as root, self._patched(root, store):
            write_marker(root)
            store.data[release_key("render")] = {"at": time.time() - 1}
            with self.assertRaises(WorkerNotReady) as caught:
                self.cold_start("Worker")
            self.assertEqual(caught.exception.error_code, "released")
            self.assertNotIn(heartbeat_key("render"), store.data)

    def test_analysis_snapshot_holds_no_session_and_no_heartbeat(self) -> None:
        store = FakeModalDict()
        events: List[Any] = []

        class Runtime:
            """OnDemandAnalysis stand-in (the real one needs numpy): records its use."""

            def __init__(self, root: str, selected: Any, reload: Any = None):
                events.append(("runtime", root))

            def analyze(self, metadata: Any, tile: bytes) -> Dict[str, Any]:
                events.append(("session", metadata))
                return {"ok": True}

        fake = SimpleNamespace(OnDemandAnalysis=Runtime, import_runtime_modules=lambda: events.append(("imports",)))
        with tempfile.TemporaryDirectory() as root, self._patched(root, store), \
                mock.patch.dict(sys.modules, {"deploy.cloud.common.analysis_runtime": fake}):
            snap = self.snapshot("AnalysisGPU")
            self.assertEqual(events, [("imports",), ("runtime", root)], "no session before the snapshot")
            self.assertFalse(hasattr(snap, "heartbeat"))
            self.assertEqual(store.data, {})
            restored_at = time.time()
            live = self.restore(snap)
            doc = store.data[heartbeat_key("analysis")]
            self.assertEqual((doc["state"], doc["container"]), ("idle", live.heartbeat.container))
            self.assertGreaterEqual(doc["started_at"], restored_at)
            self.assertEqual(self._call(live, "analyze", {"capability": "x"}, b"png"), {"ok": True})
            self.assertEqual(events[-1], ("session", {"capability": "x"}))
            self.assertEqual(store.data[heartbeat_key("analysis")]["state"], "idle")

    def _call(self, instance: Any, name: str, *args: Any) -> Any:
        from modal._partial_function import _find_partial_methods_for_user_cls, _PartialFunctionFlags as F
        return _find_partial_methods_for_user_cls(type(instance), F.CALLABLE_INTERFACE)[name].raw_f(instance, *args)


if __name__ == "__main__":
    unittest.main()
