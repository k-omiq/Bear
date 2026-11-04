"""What the helper keeps on its DLL search path on Windows."""

from __future__ import annotations

import os
import sys
import tempfile
import importlib
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))

import manga_cleaner_sidecar
from manga_cleaner_sidecar import path_without_cuda


class PathWithoutCuda(unittest.TestCase):
    def test_folders_with_another_cudnn_or_cuda_toolkit_leave_path(self) -> None:
        # A cuDNN install, a CUDA toolkit's bin, and the rest of the PATH.
        with tempfile.TemporaryDirectory() as scratch:
            cudnn = Path(scratch) / "CUDNN" / "v9.23" / "bin" / "13.0"
            toolkit = Path(scratch) / "CUDA" / "v13.0" / "bin"
            other = Path(scratch) / "Git" / "cmd"
            for folder in (cudnn, toolkit, other):
                folder.mkdir(parents=True)
            (cudnn / "cudnn_engines_tensor_ir64_9.dll").touch()
            (cudnn / "cudnn64_9.dll").touch()
            (toolkit / "cudart64_13.dll").touch()
            (toolkit / "nvcc.exe").touch()
            (other / "git.exe").touch()
            missing = str(Path(scratch) / "gone")
            value = os.pathsep.join([str(cudnn), str(other), str(toolkit), missing, ""])
            self.assertEqual(path_without_cuda(value), os.pathsep.join([str(other), missing, ""]))

    def test_a_folder_with_no_nvidia_library_stays(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            (Path(scratch) / "cudnn_notes.txt").touch()
            self.assertEqual(path_without_cuda(scratch), scratch)



class OnWindows(unittest.TestCase):
    def tearDown(self) -> None:
        importlib.reload(manga_cleaner_sidecar)

    def test_the_shipped_layout_finds_the_app_folder_with_no_variable(self) -> None:
        # The app runs the helper from <app>/sidecar with the C++ runtime in <app>.
        with tempfile.TemporaryDirectory() as scratch:
            app = Path(scratch).resolve() / "Manga Cleaner"
            package = app / "sidecar" / "manga_cleaner_sidecar"
            package.mkdir(parents=True)
            (app / "msvcp140.dll").touch()
            with mock.patch.dict(os.environ), \
                 mock.patch.object(manga_cleaner_sidecar, "__file__", str(package / "__init__.py")):
                os.environ.pop("MC_SIDECAR_DLL_DIR", None)
                self.assertEqual(manga_cleaner_sidecar._app_dll_dirs(), [app])

    def test_importing_the_package_filters_path_and_adds_the_app_folder(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            cudnn = Path(scratch) / "cudnn"
            app = Path(scratch) / "app"
            for folder in (cudnn, app):
                folder.mkdir()
            (cudnn / "cudnn64_9.dll").touch()
            (app / "msvcp140.dll").touch()
            added = []
            with mock.patch.dict(os.environ, {"PATH": os.pathsep.join([str(cudnn), str(app)]),
                                              "MC_SIDECAR_DLL_DIR": str(app)}), \
                 mock.patch.object(sys, "platform", "win32"), \
                 mock.patch.object(os, "add_dll_directory", create=True,
                                   side_effect=lambda folder: added.append(folder)):
                importlib.reload(manga_cleaner_sidecar)
                self.assertEqual(os.environ["PATH"], str(app))
            self.assertEqual(added[0], str(app))

if __name__ == "__main__":
    unittest.main()
