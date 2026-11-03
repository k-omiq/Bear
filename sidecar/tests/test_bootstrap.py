"""Offline checks for the desktop's model installation sequence."""

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from sidecar import bootstrap


class BootstrapTest(unittest.TestCase):
    def test_a_failed_step_names_itself_and_says_why(self):
        script = "import sys; sys.stderr.write('x' * 5000 + ' 404 for this repo'); sys.exit(3)"
        with self.assertRaises(RuntimeError) as caught:
            bootstrap.run([sys.executable, "-c", script], label="model download")
        message = str(caught.exception)
        self.assertTrue(message.startswith("model download failed (exit 3): "), message)
        self.assertTrue(message.endswith("404 for this repo"), message)
        self.assertLess(len(message), 500)

    def test_a_module_step_is_named_after_the_module(self):
        with self.assertRaises(RuntimeError) as caught:
            bootstrap.run([sys.executable, "-m", "no_such_module_here"])
        self.assertIn("no_such_module_here failed (exit 1): ", str(caught.exception))
        self.assertIn("No module named", str(caught.exception))

    def test_sdnq_install_creates_discoverable_environment_and_pinned_weights(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch) / "install"
            source = Path(scratch) / "source"
            source.mkdir()
            (source / "pyproject.toml").write_text("[project]\nname='test'\n")
            calls = []

            def fake_run(command, **_kwargs):
                calls.append(command)
                if command[1:3] == ["-m", "venv"]:
                    pending = Path(command[-1])
                    python = bootstrap.python_in(pending)
                    python.parent.mkdir(parents=True)
                    (pending / "pyvenv.cfg").write_text("home=/tmp\n")
                    python.touch()
                elif "snapshot_download" in " ".join(command):
                    weights = Path(command[-1])
                    (weights / "model_index.json").write_text("{}")

            with patch.object(bootstrap, "run", side_effect=fake_run), patch.object(bootstrap.sys, "version_info", (3, 11, 0)):
                result = bootstrap.install(root, source, "sdnq", "cuda")
            self.assertEqual(result["model"], "flux2-klein-4b")
            self.assertTrue((root / ".venv/pyvenv.cfg").is_file())
            marker = root / "weights/flux2-klein-4b-sdnq/.manga-cleaner-ready.json"
            self.assertTrue(marker.is_file())
            self.assertTrue(any("torch==2.13.0+cu129" in call and "torchvision==0.28.0+cu129" in call for call in calls))
            self.assertTrue(any("--no-deps" in call for call in calls))
            self.assertTrue(any("endswith('+cu129')" in " ".join(call) for call in calls))

    def test_a_python_without_the_pinned_torchvision_is_refused(self):
        with tempfile.TemporaryDirectory() as scratch, patch.object(bootstrap.sys, "version_info", (3, 10, 0)):
            with self.assertRaisesRegex(ValueError, "Python 3.11 or 3.12"):
                bootstrap.install(Path(scratch), Path(scratch), "sdnq", "cuda")

    def test_a_cuda_build_that_is_replaced_fails_the_import_check(self):
        check = "import types; torch = types.SimpleNamespace(__version__='2.15.0'); assert torch.__version__.endswith('+cu130'), 'not the cu130 build'"
        with self.assertRaisesRegex(RuntimeError, "not the cu130 build"):
            bootstrap.run([sys.executable, "-c", check], label="import check")

    def test_a_pip_step_may_outlast_a_slow_torch_download(self):
        with patch.object(bootstrap.subprocess, "run") as run:
            run.return_value.returncode = 0
            bootstrap.run([sys.executable, "-m", "pip", "install", "torch"])
        self.assertEqual(run.call_args.kwargs["timeout"], 7200)

    def _install(self, root, source, *args, **kwargs):
        def fake_run(command, **_kwargs):
            if command[1:3] == ["-m", "venv"]:
                pending = Path(command[-1])
                python = bootstrap.python_in(pending)
                python.parent.mkdir(parents=True)
                (pending / "pyvenv.cfg").write_text("home=/tmp\n")
                python.touch()
            elif "snapshot_download" in " ".join(command):
                self.assertTrue(Path(command[-1]).name.startswith(".installing-"))
                (Path(command[-1]) / "model_index.json").write_text("{}")
            elif "hf_hub_download" in " ".join(command):
                self.fetched.append(command[-2])
        self.fetched = []
        with patch.object(bootstrap, "run", side_effect=fake_run), patch.object(bootstrap.sys, "version_info", (3, 11, 0)):
            return bootstrap.install(root, source, *args, **kwargs)

    def test_a_checkpoint_lands_with_its_adapters_and_shows_only_when_complete(self):
        with tempfile.TemporaryDirectory() as scratch:
            root, source = Path(scratch) / "install", Path(scratch) / "source"
            source.mkdir()
            (source / "pyproject.toml").write_text("[project]\nname='test'\n")
            checkpoint = {"repo": "a/b", "revision": "c", "dir": "qwen-image-edit-2511-sdnq", "expected": "model_index.json",
                          "bytes": 1, "adapters": [{"repo": "d/e", "revision": "f", "file": "lora.safetensors"}]}
            result = self._install(root, source, "sdnq", "mps", "qwen-image-edit-2511", checkpoint)
            self.assertEqual(result["model"], "qwen-image-edit-2511")
            self.assertEqual(self.fetched, ["lora.safetensors"])
            weights = root / "weights"
            self.assertTrue((weights / "qwen-image-edit-2511-sdnq/.manga-cleaner-ready.json").is_file())
            self.assertFalse((weights / ".installing-qwen-image-edit-2511-sdnq").exists())

    def test_a_second_model_uses_the_environment_already_there(self):
        with tempfile.TemporaryDirectory() as scratch:
            root, source = Path(scratch) / "install", Path(scratch) / "source"
            source.mkdir()
            (source / "pyproject.toml").write_text("[project]\nname='test'\n")
            root.mkdir()
            (root / "pyvenv.cfg").write_text("home=/tmp\n")
            python = bootstrap.python_in(root)
            python.parent.mkdir(parents=True)
            python.touch()
            self._install(root, source, "sdnq", "mps")
            self.assertFalse((root / ".venv").exists(), "the root is the environment")

    def test_the_import_check_covers_what_starting_the_helper_imports(self):
        with tempfile.TemporaryDirectory() as scratch:
            root, source = Path(scratch) / "install", Path(scratch) / "source"
            source.mkdir()
            (source / "pyproject.toml").write_text("[project]\nname='test'\n")
            checks = []

            def fake_run(command, **kwargs):
                if kwargs.get("label") == "import check":
                    checks.append(command[-1])
                if command[1:3] == ["-m", "venv"]:
                    pending = Path(command[-1])
                    bootstrap.python_in(pending).parent.mkdir(parents=True)
                    (pending / "pyvenv.cfg").write_text("home=/tmp\n")
                    bootstrap.python_in(pending).touch()
                elif "snapshot_download" in " ".join(command):
                    (Path(command[-1]) / "model_index.json").write_text("{}")

            with patch.object(bootstrap, "run", side_effect=fake_run), patch.object(bootstrap.sys, "version_info", (3, 11, 0)):
                bootstrap.install(root, source, "sdnq", "mps")
            self.assertIn("manga_cleaner_sidecar.__main__", checks[0])

    def test_a_download_that_cannot_fit_is_refused_before_it_starts(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            pending = root / ".installing-model"
            checkpoint = {"bytes": 12_000_000_000}
            free = bootstrap.shutil.disk_usage(root)._replace(free=5_000_000_000)
            with patch.object(bootstrap.shutil, "disk_usage", return_value=free):
                with self.assertRaisesRegex(RuntimeError, r"needs 14\.0 GB free, and 5\.0 GB is free"):
                    bootstrap.check_disk_room(root, pending, checkpoint)
                # What a retry has already fetched does not have to fit again.
                pending.mkdir()
                with (pending / "part").open("wb") as held:
                    held.truncate(11_000_000_000)
                bootstrap.check_disk_room(root, pending, checkpoint)

    def test_a_checkpoint_cannot_write_outside_the_weights_folder(self):
        for name in ("../evil", "/etc/evil", ".hidden", ""):
            with self.assertRaises(ValueError):
                bootstrap.valid_checkpoint({"dir": name, "adapters": []})
        with self.assertRaises(ValueError):
            bootstrap.valid_checkpoint({"dir": "ok", "adapters": [{"file": "../evil"}]})

    def test_invalid_backend_and_accelerator_are_rejected_before_install(self):
        with self.assertRaises(ValueError):
            bootstrap.resolve_backend("bad", "auto")
        with self.assertRaises(ValueError):
            bootstrap.resolve_backend("sdnq", "bogus")

    def test_a_repair_under_automatic_keeps_the_gpu_torch_was_built_for(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".venv").mkdir()
            (root / ".venv" / "pyvenv.cfg").write_text("")
            self.assertIsNone(bootstrap.installed_accelerator(root))
            site = root / ".venv" / "Lib" / "site-packages"
            site.mkdir(parents=True)
            (site / "torch-2.13.0.dist-info").mkdir()
            self.assertIsNone(bootstrap.installed_accelerator(root), "a CPU-only torch names no GPU")
            (site / "torch-2.13.0.dist-info").rmdir()
            (site / "torch-2.13.0+xpu.dist-info").mkdir()
            self.assertEqual(bootstrap.installed_accelerator(root), "xpu")
        with patch.object(bootstrap.sys, "platform", "win32"), patch.object(bootstrap.shutil, "which", return_value=None):
            self.assertEqual(bootstrap.resolve_backend("sdnq", "auto", "xpu"), ("sdnq", "xpu"))
            self.assertEqual(bootstrap.resolve_backend("sdnq", "cuda", "xpu"), ("sdnq", "cuda"), "a choice made wins")

    def test_windows_cuda_uses_published_torch_wheel(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch) / "install"
            source = Path(scratch) / "source"
            source.mkdir()
            (source / "pyproject.toml").write_text("[project]\nname='test'\n")
            calls = []

            def fake_run(command, **_kwargs):
                calls.append(command)
                if command[1:3] == ["-m", "venv"]:
                    pending = Path(command[-1])
                    python = bootstrap.python_in(pending)
                    python.parent.mkdir(parents=True)
                    (pending / "pyvenv.cfg").write_text("home=C:\\\\Python\n")
                    python.touch()
                elif "snapshot_download" in " ".join(command):
                    (Path(command[-1]) / "model_index.json").write_text("{}")

            with patch.object(bootstrap, "run", side_effect=fake_run), \
                 patch.object(bootstrap.sys, "version_info", (3, 11, 0)), \
                 patch.object(bootstrap.sys, "platform", "win32"):
                bootstrap.install(root, source, "sdnq", "cuda")
            torch_step = next(call for call in calls if "torch==2.13.0+cu130" in call)
            self.assertIn("torchvision==0.28.0+cu130", torch_step)
            self.assertIn("https://download.pytorch.org/whl/cu130", torch_step)


if __name__ == "__main__":
    unittest.main()
