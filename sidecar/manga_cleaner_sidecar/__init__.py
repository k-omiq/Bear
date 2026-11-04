"""Manga Cleaner Sidecar package.

Out-of-process diffusion inpainting engine (Rung 3a) serving FLUX.2 Klein 4B
over loopback HTTP/1.1 for Manga Cleaner.
"""

import os
import sys
from pathlib import Path

__version__ = "0.1.0"

__all__ = ["__version__"]

#: File names of NVIDIA's own CUDA and cuDNN libraries on Windows.
_CUDA_LIBRARY_PREFIXES = ("cudnn", "cublas", "cudart")


def _app_dll_dirs() -> list:
    """Where the app keeps the Microsoft C++ runtime it ships: torch needs it, and a
    Windows without Visual C++ installed has none. Named by the installer's import
    check, or the app's folder, which holds this package's bundled copy."""
    named = os.environ.get("MC_SIDECAR_DLL_DIR")
    candidates = [Path(named)] if named else []
    candidates.append(Path(__file__).resolve().parent.parent.parent)
    return [folder for folder in candidates if (folder / "msvcp140.dll").is_file()]


def _holds_cuda_libraries(folder: str) -> bool:
    try:
        names = os.listdir(folder)
    except OSError:
        return False
    return any(name.lower().startswith(_CUDA_LIBRARY_PREFIXES) and name.lower().endswith(".dll")
               for name in names)


def path_without_cuda(value: str) -> str:
    """`PATH` less every folder holding a CUDA toolkit's or cuDNN's own libraries.

    torch's Windows wheel carries its own cuDNN 9 but not every part a newer cuDNN
    has (no `cudnn_engines_tensor_ir64_9.dll`), and cuDNN opens its parts by name on
    the first convolution. A cuDNN of another version on `PATH`, the user's own or
    the one the app puts there for a CUDA build of ONNX Runtime (`cuda.rs`), then
    lends that part, and every render fails with
    `CUDNN_STATUS_SUBLIBRARY_VERSION_MISMATCH`. torch needs nothing from those folders.
    """
    return os.pathsep.join(entry for entry in value.split(os.pathsep)
                           if not (entry and _holds_cuda_libraries(entry)))


if sys.platform == "win32":
    os.environ["PATH"] = path_without_cuda(os.environ.get("PATH", ""))

#: Kept open for the life of the process: closing one takes its folder back off the
#: search path.
_DLL_DIRECTORIES = (
    [os.add_dll_directory(str(folder)) for folder in _app_dll_dirs()]
    if sys.platform == "win32" else []
)
