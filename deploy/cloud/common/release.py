"""Which cloud code a deployment runs, as one digest.

The Modal image ships the package inits plus the `common/` and `modal/` sources
(`shipped`). `code_digest` hashes exactly those files, so the gateway can say
which code it runs (`GET /mc/v1/capabilities`, `code_digest`) and the desktop can
compare it with the code its own helper would deploy (`build.rs` hashes the same
files the same way). A difference means Update would change what runs.

Standard library only: the gateway image and the helper both import it.
"""

from __future__ import annotations

import hashlib
from pathlib import Path
from typing import List

_SHIPPED_DIRS = {("cloud", "common"), ("cloud", "modal")}
_SHIPPED_INITS = {("__init__.py",), ("cloud", "__init__.py")}


def shipped(relative: Path) -> bool:
    """Whether a path under `deploy/` is part of the Modal image's code."""
    parts = relative.parts
    if relative.suffix != ".py":
        return False
    if parts in _SHIPPED_INITS:
        return True
    return len(parts) == 3 and parts[:2] in _SHIPPED_DIRS


def shipped_files(root: Path) -> List[Path]:
    """The shipped files under `root` (the `deploy/` directory), relative, sorted by POSIX path."""
    candidates = [root / "__init__.py", root / "cloud" / "__init__.py"]
    for directory in ("common", "modal"):
        folder = root / "cloud" / directory
        if folder.is_dir():
            candidates.extend(sorted(folder.iterdir()))
    files = [path.relative_to(root) for path in candidates if path.is_file()]
    return sorted((path for path in files if shipped(path)), key=lambda path: path.as_posix())


def code_digest(root: Path) -> str:
    """SHA-256 over each shipped file's POSIX path, a NUL, its SHA-256, and a newline."""
    outer = hashlib.sha256()
    for relative in shipped_files(root):
        inner = hashlib.sha256((root / relative).read_bytes()).hexdigest()
        outer.update(f"{relative.as_posix()}\0{inner}\n".encode("utf-8"))
    return outer.hexdigest()
