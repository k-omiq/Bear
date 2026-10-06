"""Offline checks for the desktop's FLUX installation sequence."""

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
