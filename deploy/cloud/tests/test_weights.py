"""Weights seeding and the seeded-marker gate, with sparse files instead of 5 GB."""

from __future__ import annotations

import json
from dataclasses import replace
import tempfile
import threading
import unittest
from unittest import mock
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from deploy.cloud.common import weights
from deploy.cloud.common.jobs import MemoryStore
from deploy.cloud.common.manifest import (
    MODEL_PROD_FLUX,
    PROD_SNAPSHOT_FILES,
    PROD_SNAPSHOT_TOTAL_BYTES,
    REVISION_PROD_FLUX,
    MODEL_PROD_FLUX_9B,
    REVISION_PROD_FLUX_9B,
    PROD_SNAPSHOT_9B_FILES,
    PROD_SNAPSHOT_9B_TOTAL_BYTES,
    ADAPTER_DIR,
    FUKIDASHI_FILE,
    FUKIDASHI_REPO,
    FUKIDASHI_REVISION,
    LIGHTNING_FILE,
    LIGHTNING_REPO,
    LIGHTNING_REVISION,
    MODEL_PROD_QWEN,
    PRODUCTION_MODELS,
    PROD_SNAPSHOT_QWEN_REPO_FILES,
    PROD_SNAPSHOT_QWEN_TOTAL_BYTES,
    REVISION_PROD_QWEN,
)
from deploy.cloud.tests.support import silence_deploy_logs

_restore_logs = None


def setUpModule() -> None:
    global _restore_logs
    _restore_logs = silence_deploy_logs()


def tearDownModule() -> None:
    _restore_logs()


def write_sparse(directory: Path, files=PROD_SNAPSHOT_FILES) -> None:
    for relative, size in files:
        target = directory / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        with open(target, "wb") as handle:
            handle.truncate(size)


class FakeDownload:
    """snapshot_download stand-in that writes the pinned file set as sparse files."""

    def __init__(self, files=PROD_SNAPSHOT_FILES, error: Optional[BaseException] = None, failures: int = 0):
        self.files = files
        self.error = error
        self.failures = failures
        self.calls: List[Dict[str, Any]] = []

    def __call__(self, **kwargs: Any) -> str:
        self.calls.append(kwargs)
        if self.error is not None and len(self.calls) <= self.failures:
            raise self.error
        write_sparse(Path(kwargs["local_dir"]), self.files)
        return kwargs["local_dir"]


class SnapshotFilesTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.dir = Path(self._tmp.name)

    def test_complete_set_has_no_problems(self) -> None:
        write_sparse(self.dir)
        self.assertEqual(weights.check_snapshot_files(self.dir), [])

    def test_missing_and_wrong_sizes_are_listed(self) -> None:
        write_sparse(self.dir)
        (self.dir / "vae" / "config.json").unlink()
        with open(self.dir / "README.md", "wb") as handle:
            handle.truncate(1)
        problems = weights.check_snapshot_files(self.dir)
        self.assertIn("missing: vae/config.json", problems)
        self.assertTrue(any(p.startswith("size mismatch: README.md") for p in problems))


class SeedSnapshotTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)

    def test_seed_downloads_the_pin_verifies_marks_and_commits(self) -> None:
        download = FakeDownload()
        commits: List[int] = []
        progress: List[Tuple[int, int]] = []
        result = weights.seed_snapshot(
            self.root, download, lambda: commits.append(1), lambda done, total: progress.append((done, total))
        )
        self.assertEqual(result["status"], "seeded")
        self.assertEqual(
            download.calls,
            [{"repo_id": MODEL_PROD_FLUX, "revision": REVISION_PROD_FLUX, "local_dir": str(weights.snapshot_dir(self.root))}],
        )
        self.assertEqual(commits, [1])
        self.assertTrue(weights.is_seeded(self.root))
        self.assertEqual(progress[0], (0, PROD_SNAPSHOT_TOTAL_BYTES))
        self.assertEqual(progress[-1], (PROD_SNAPSHOT_TOTAL_BYTES, PROD_SNAPSHOT_TOTAL_BYTES))

        again = weights.seed_snapshot(self.root, download, lambda: commits.append(1))
        self.assertEqual(again["status"], "present")
        self.assertEqual(len(download.calls), 1, "a seeded volume never downloads again")
        self.assertEqual(commits, [1])

    def test_9b_seed_uses_its_own_pin_marker_and_size(self) -> None:
        download = FakeDownload(files=PROD_SNAPSHOT_9B_FILES)
        result = weights.seed_snapshot(self.root, download, model_id=MODEL_PROD_FLUX_9B)
        self.assertEqual(result["model_revision"], REVISION_PROD_FLUX_9B)
        self.assertEqual(result["total_bytes"], PROD_SNAPSHOT_9B_TOTAL_BYTES)
        self.assertEqual(download.calls[0]["repo_id"], MODEL_PROD_FLUX_9B)
        self.assertEqual(download.calls[0]["revision"], REVISION_PROD_FLUX_9B)
        self.assertTrue(weights.is_seeded(self.root, MODEL_PROD_FLUX_9B))
        self.assertFalse(weights.is_seeded(self.root, MODEL_PROD_FLUX))

    def test_qwen_seed_downloads_the_pinned_loras_into_the_snapshot(self) -> None:
        loras = {LIGHTNING_REPO: ((LIGHTNING_FILE, 849608296),), FUKIDASHI_REPO: ((FUKIDASHI_FILE, 18753872),)}

        def download(**kwargs: Any) -> str:
            calls.append(kwargs)
            write_sparse(Path(kwargs["local_dir"]), loras.get(kwargs["repo_id"], PROD_SNAPSHOT_QWEN_REPO_FILES))
            return kwargs["local_dir"]

        calls: List[Dict[str, Any]] = []
        result = weights.seed_snapshot(self.root, download, model_id=MODEL_PROD_QWEN)
        self.assertEqual(result["status"], "seeded")
        self.assertEqual(result["total_bytes"], PROD_SNAPSHOT_QWEN_TOTAL_BYTES)
        self.assertEqual([(c["repo_id"], c["revision"]) for c in calls],
                         [(MODEL_PROD_QWEN, REVISION_PROD_QWEN), (LIGHTNING_REPO, LIGHTNING_REVISION),
                          (FUKIDASHI_REPO, FUKIDASHI_REVISION)])
        snapshot = weights.snapshot_dir(self.root, MODEL_PROD_QWEN)
        self.assertEqual([c["local_dir"] for c in calls[1:]], [str(snapshot / ADAPTER_DIR)] * 2)
        self.assertEqual([c["allow_patterns"] for c in calls[1:]], [[LIGHTNING_FILE], [FUKIDASHI_FILE]])
        self.assertTrue(weights.is_seeded(self.root, MODEL_PROD_QWEN))

    def test_qwen_seed_without_the_lora_leaves_no_marker(self) -> None:
        download = FakeDownload(files=PROD_SNAPSHOT_QWEN_REPO_FILES)
        with self.assertRaises(weights.WeightsCorruptError):
            weights.seed_snapshot(self.root, download, model_id=MODEL_PROD_QWEN)
        self.assertFalse(weights.is_seeded(self.root, MODEL_PROD_QWEN))

    def test_qwen_replaced_same_size_adapter_is_downloaded(self) -> None:
        model = PRODUCTION_MODELS[MODEL_PROD_QWEN]
        old_filename = LIGHTNING_FILE.replace("8steps", "4steps")
        old_model = replace(
            model,
            files=tuple((path.replace(LIGHTNING_FILE, old_filename), size) for path, size in model.files),
            adapters=tuple(replace(a, filename=old_filename) if a.filename == LIGHTNING_FILE else a
                           for a in model.adapters),
        )
        with mock.patch.dict(PRODUCTION_MODELS, {MODEL_PROD_QWEN: old_model}):
            old_marker = weights.expected_marker(MODEL_PROD_QWEN)
            write_sparse(weights.snapshot_dir(self.root, MODEL_PROD_QWEN), old_model.files)
        current_marker = weights.expected_marker(MODEL_PROD_QWEN)
        self.assertEqual(old_marker["file_count"], current_marker["file_count"])
        self.assertEqual(old_marker["total_bytes"], current_marker["total_bytes"])
        self.assertFalse(weights.marker_matches(old_marker, MODEL_PROD_QWEN))
        weights.marker_path(self.root, MODEL_PROD_QWEN).write_text(json.dumps(old_marker))

        calls = []
        def download(**kwargs: Any) -> None:
            calls.append(kwargs)
            files = ((a.filename, a.size_bytes) for a in model.adapters if a.repo_id == kwargs["repo_id"])
            write_sparse(Path(kwargs["local_dir"]), tuple(files) or PROD_SNAPSHOT_QWEN_REPO_FILES)

        result = weights.seed_snapshot(self.root, download, model_id=MODEL_PROD_QWEN)
        self.assertEqual(result["status"], "seeded")
        self.assertIn([LIGHTNING_FILE], [c.get("allow_patterns") for c in calls])
        self.assertEqual(weights.read_marker(self.root, MODEL_PROD_QWEN), current_marker)
        self.assertEqual(weights.seed_snapshot(self.root, download, model_id=MODEL_PROD_QWEN)["status"], "present")
        self.assertEqual(len(calls), 3)

    def test_adapter_revision_change_invalidates_marker(self) -> None:
        model = PRODUCTION_MODELS[MODEL_PROD_QWEN]
        current = weights.expected_marker(MODEL_PROD_QWEN)
        revised = replace(model, adapters=(replace(model.adapters[0], revision="different-pin"), *model.adapters[1:]))
        with mock.patch.dict(PRODUCTION_MODELS, {MODEL_PROD_QWEN: revised}):
            self.assertFalse(weights.marker_matches(current, MODEL_PROD_QWEN))

    def test_legacy_marker_requires_verification_again(self) -> None:
        legacy = weights.expected_marker(MODEL_PROD_QWEN)
        legacy.pop("snapshot_fingerprint")
        self.assertFalse(weights.marker_matches(legacy, MODEL_PROD_QWEN))

    def test_incomplete_download_leaves_no_marker(self) -> None:
        download = FakeDownload(files=PROD_SNAPSHOT_FILES[:-1])
        with self.assertRaises(weights.WeightsCorruptError) as caught:
            weights.seed_snapshot(self.root, download)
        self.assertEqual(caught.exception.error_code, "weights_corrupt")
        self.assertFalse(weights.is_seeded(self.root))
        self.assertFalse(weights.marker_path(self.root).exists())

    def test_progress_is_reported_while_the_download_runs(self) -> None:
        seen_partial = threading.Event()
        reports: List[int] = []

        def on_progress(done: int, total: int) -> None:
            reports.append(done)
            if 0 < done < total:
                seen_partial.set()

        def download(**kwargs: Any) -> None:
            target = Path(kwargs["local_dir"])
            write_sparse(target, PROD_SNAPSHOT_FILES[:7])
            self.assertTrue(seen_partial.wait(10), "no progress report during the download")
            write_sparse(target, PROD_SNAPSHOT_FILES)

        weights.seed_snapshot(self.root, download, progress=on_progress, progress_interval=0.01)
        self.assertTrue(all(0 <= done <= PROD_SNAPSHOT_TOTAL_BYTES for done in reports))

    def test_failing_progress_reporter_does_not_fail_the_seed(self) -> None:
        def broken(done: int, total: int) -> None:
            raise RuntimeError("reporter down")

        result = weights.seed_snapshot(self.root, FakeDownload(), progress=broken, progress_interval=0.01)
        self.assertEqual(result["status"], "seeded")

    def test_heartbeat_continues_until_the_snapshot_is_committed(self) -> None:
        committing = threading.Event()
        beat_while_committing = threading.Event()

        def on_progress(done: int, total: int) -> None:
            if committing.is_set():
                beat_while_committing.set()

        def commit() -> None:
            committing.set()
            self.assertTrue(beat_while_committing.wait(10), "the heartbeat stopped before the commit")

        result = weights.seed_snapshot(self.root, FakeDownload(), commit, on_progress, progress_interval=0.01)
        self.assertEqual(result["status"], "seeded")


class RunSeedTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        self.store = MemoryStore()
        self.sleeps: List[float] = []

    def test_success_publishes_done(self) -> None:
        result = weights.run_seed(self.root, self.store, snapshot_download=FakeDownload(), sleep=self.sleeps.append)
        self.assertEqual(result["status"], "seeded")
        state = self.store.get(weights.SEED_STATE_KEY)
        self.assertEqual((state["state"], state["bytes_done"]), ("done", PROD_SNAPSHOT_TOTAL_BYTES))
        self.assertEqual(state["model_revision"], REVISION_PROD_FLUX)

    def test_transient_failure_is_retried(self) -> None:
        download = FakeDownload(error=OSError("connection reset"), failures=1)
        result = weights.run_seed(
            self.root, self.store, snapshot_download=download, retry_delay_seconds=3, sleep=self.sleeps.append
        )
        self.assertEqual(result["status"], "seeded")
        self.assertEqual(len(download.calls), 2)
        self.assertEqual(self.sleeps, [3])

    def test_exhausted_attempts_return_a_typed_failure(self) -> None:
        download = FakeDownload(error=OSError("401 for token=hf_abcdefghijklmnop"), failures=99)
        result = weights.run_seed(
            self.root, self.store, snapshot_download=download, attempts=2, retry_delay_seconds=1, sleep=self.sleeps.append
        )
        self.assertEqual((result["status"], result["error_code"]), ("failed", "weights_download_failed"))
        self.assertNotIn("hf_abcdefghijklmnop", result["message"])
        self.assertEqual(self.sleeps, [1])
        state = self.store.get(weights.SEED_STATE_KEY)
        self.assertEqual((state["state"], state["error_code"]), ("failed", "weights_download_failed"))

    def test_corrupt_snapshot_keeps_its_code(self) -> None:
        download = FakeDownload(files=PROD_SNAPSHOT_FILES[:3])
        result = weights.run_seed(self.root, self.store, snapshot_download=download, attempts=1, sleep=self.sleeps.append)
        self.assertEqual(result["error_code"], "weights_corrupt")

    def test_store_outage_never_raises(self) -> None:
        class DownStore:
            def put(self, key: str, value: Dict[str, Any]) -> None:
                raise ConnectionError("store down")

        result = weights.run_seed(self.root, DownStore(), snapshot_download=FakeDownload(), sleep=self.sleeps.append)
        self.assertEqual(result["status"], "seeded")

    def test_logged_failures_carry_no_secret(self) -> None:
        leak = "GET https://cdn.example/f?X-Amz-Signature=sig0123456789 token=hf_abcdefghijklmnop"

        class LeakyStore:
            def put(self, key: str, value: Dict[str, Any]) -> None:
                raise ConnectionError(leak)

        def leaky_progress(done: int, total: int) -> None:
            raise RuntimeError(leak)

        with self.assertLogs("deploy.cloud.weights", level="WARNING") as logs:
            download = FakeDownload(error=OSError(leak), failures=99)
            weights.run_seed(self.root, LeakyStore(), snapshot_download=download, attempts=1, sleep=self.sleeps.append)
            weights.seed_snapshot(self.root, FakeDownload(), progress=leaky_progress, progress_interval=60)
        text = "\n".join(logs.output)
        for line in ("Seed state write failed", "Seeding attempt 1 failed", "Seed progress report failed"):
            self.assertIn(line, text)
        self.assertNotIn("hf_abcdefghijklmnop", text)
        self.assertNotIn("sig0123456789", text)


class RequireSnapshotTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)

    def test_missing_after_all_attempts(self) -> None:
        reloads: List[int] = []
        sleeps: List[float] = []
        with self.assertRaises(weights.WeightsMissingError) as caught:
            weights.require_snapshot(self.root, reload=lambda: reloads.append(1), attempts=3, delay_seconds=2, sleep=sleeps.append)
        self.assertEqual(caught.exception.error_code, "weights_missing")
        self.assertEqual((len(reloads), sleeps), (2, [2, 2]))

    def test_marker_that_appears_after_a_reload_is_found(self) -> None:
        def reload() -> None:
            weights.seed_snapshot(self.root, FakeDownload())

        path = weights.require_snapshot(self.root, reload=reload, attempts=2, sleep=lambda s: None)
        self.assertEqual(path, weights.snapshot_dir(self.root))

    def test_failed_reload_counts_as_not_visible(self) -> None:
        def reload() -> None:
            raise RuntimeError("reload failed")

        with self.assertRaises(weights.WeightsMissingError):
            weights.require_snapshot(self.root, reload=reload, attempts=2, sleep=lambda s: None)

    def test_marker_for_another_pin_is_ignored(self) -> None:
        weights.marker_path(self.root).write_text('{"schema": 1, "model_revision": "other"}', encoding="utf-8")
        self.assertFalse(weights.is_seeded(self.root))


if __name__ == "__main__":
    unittest.main()
