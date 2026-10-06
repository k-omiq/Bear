"""The clean directory Beam uploads for one deploy.

Beam uploads the whole working directory and names each container handler after the
module path relative to it, so the provisioner deploys from a fresh directory that
holds only the app module at its root, the part of the deploy package the containers
import, and an ignore file. Nothing from the user's machine besides these files is
uploaded. This module does not import beam.
"""

from __future__ import annotations

import shutil
from pathlib import Path
from typing import Iterator, Tuple

from deploy.cloud.beam.settings import APP_MODULE

# deploy/cloud/beam/stage.py -> deploy/
DEPLOY_ROOT = Path(__file__).resolve().parents[2]
REPO_ROOT = DEPLOY_ROOT.parent
APP_SOURCE = Path(__file__).resolve().with_name("app.py")

_SHIPPED_FILES: Tuple[str, ...] = (
    "__init__.py",
    "cloud/__init__.py",
    "cloud/beam/__init__.py",
    "cloud/beam/backend.py",
    "cloud/beam/routes.py",
    "cloud/beam/settings.py",
    "cloud/beam/analysis_backend.py",
)
_SHIPPED_PACKAGES: Tuple[str, ...] = ("cloud/common",)

# Written by us so the SDK does not generate its own; patterns use gitwildmatch.
IGNORE_FILE_NAME = ".beamignore"
IGNORE_FILE_CONTENTS = ".beamignore\n__pycache__\n**/__pycache__/\n*.pyc\n"


def shipped_files() -> Iterator[str]:
    """Paths relative to the deploy package root that the containers need."""
    yield from _SHIPPED_FILES
    for package in _SHIPPED_PACKAGES:
        for source in sorted((DEPLOY_ROOT / package).glob("*.py")):
            yield f"{package}/{source.name}"


def stage_app(target: Path) -> Path:
    """Fill `target` (normally a new empty temp directory) and return the module path."""
    target.mkdir(parents=True, exist_ok=True)
    module_path = target / f"{APP_MODULE}.py"
    shutil.copyfile(APP_SOURCE, module_path)
    for relative in shipped_files():
        destination = target / "deploy" / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(DEPLOY_ROOT / relative, destination)
    # The analysis seed executes the exact desktop SAM exporter. Only source and
    # one synthetic parity input travel with the deployment, never graph bytes.
    sources = {
        "bootstrap.py": REPO_ROOT / "sam-bootstrap/bootstrap.py",
        "export_mask.py": REPO_ROOT / "spikes/sam-ts-l/export_mask.py",
        "synthetic_page.png": REPO_ROOT / "spikes/sam-ts-l/fixtures/synthetic_page.png",
    }
    for name, source in sources.items():
        destination = target / "analysis-source" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
    (target / IGNORE_FILE_NAME).write_text(IGNORE_FILE_CONTENTS, encoding="utf-8")
    return module_path
