#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
uv venv --python 3.12.13 "$here/artifacts/venv"
uv pip sync --python "$here/artifacts/venv/bin/python" "$here/environment.txt"
"$here/artifacts/venv/bin/python" - <<'PY'
import platform
import torch
import onnx
import onnxruntime
print(platform.platform(), platform.machine())
print("torch", torch.__version__, "onnx", onnx.__version__, "onnxruntime", onnxruntime.__version__)
print("providers", onnxruntime.get_available_providers())
PY
