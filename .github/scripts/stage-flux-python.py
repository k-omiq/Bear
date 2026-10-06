"""Bundle uv so FLUX setup can download Python on machines without Python.

Run after installing the pinned uv wheel, before Tauri builds. The desktop uses
uv only to acquire a managed interpreter into its own app-data directory.
"""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import sys

ROOT = Path(__file__).resolve().parents[2]


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: stage-flux-python.py <target-triple>")
    target = sys.argv[1]
    binary = shutil.which("uv")
    if binary is None:
        raise SystemExit("uv is missing; install uv==0.12.19 before staging")
    destination = ROOT / "src-tauri" / "binaries" / f"manga-cleaner-uv-{target}"
    if sys.platform == "win32":
        destination = destination.with_suffix(".exe")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(binary, destination)
    destination.chmod(0o755)
    config = ROOT / "src-tauri" / "tauri.conf.json"
    data = json.loads(config.read_text(encoding="utf-8"))
    bundled = data["bundle"].setdefault("externalBin", [])
    if "binaries/manga-cleaner-uv" not in bundled:
        bundled.append("binaries/manga-cleaner-uv")
    config.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    print(f"Managed Python installer ready: {destination}")


if __name__ == "__main__":
    main()
