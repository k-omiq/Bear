"""What the helper keeps on its DLL search path on Windows."""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))

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


if __name__ == "__main__":
    unittest.main()
