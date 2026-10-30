"""Offline checks for the desktop's model installation sequence."""

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from sidecar import bootstrap


class BootstrapTest(unittest.TestCase):
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
            self.assertTrue(any("torch==2.13.0" in call for call in calls))
            self.assertTrue(any("--no-deps" in call for call in calls))

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
            self.assertTrue(any("torch==2.9.0" in call for call in calls))


if __name__ == "__main__":
    unittest.main()
