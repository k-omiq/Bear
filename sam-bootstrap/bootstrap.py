"""Fetch pinned Koharu sources and export the app's local SAM-TS-L graphs.

Runs under a managed Python environment started by the desktop command. The
checkpoint and converted graphs stay in the user's app data directory.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
import subprocess
import sys
import time
import urllib.request
import zipfile
from pathlib import Path

MODEL_REVISION = "5dd97423e0fbf2404264979136d47e8101144046"
MODEL_SHA256 = "bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45"
MODEL_BYTES = 1355824988
MODEL_URL = f"https://huggingface.co/mayocream/koharu-text-sam-ts-l/resolve/{MODEL_REVISION}/model.safetensors"
DOWNLOAD_ATTEMPTS = 6
HI_SAM_REVISION = "69009434d4dba5541f228d8f5acb0754c333d417"
HI_SAM_ARCHIVE_SHA256 = "f18fb049813f9b9319ac074449f4539fbe5b58f4e3f450ce331578d74bc3e327"


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def progress(step: str, **fields: int) -> None:
    """One JSON line on stdout; the desktop turns these into its progress bar."""
    print(json.dumps({"step": step, **fields}), flush=True)


def checkpoint(root: Path) -> Path:
    """Download the pinned checkpoint, resuming a partial file across retries."""
    target = root / "checkpoint" / "model.safetensors"
    if target.is_file() and target.stat().st_size == MODEL_BYTES and digest(target) == MODEL_SHA256:
        progress("weights", done=MODEL_BYTES, total=MODEL_BYTES)
        return target
    target.parent.mkdir(parents=True, exist_ok=True)
    partial = target.with_name(target.name + ".part")
    for attempt in range(DOWNLOAD_ATTEMPTS):
        have = partial.stat().st_size if partial.is_file() else 0
        if have > MODEL_BYTES:
            partial.unlink()
            have = 0
        if have == MODEL_BYTES:
            break
        request = urllib.request.Request(MODEL_URL, headers={"Range": f"bytes={have}-"} if have else {})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                # A server that ignores the range sends the whole file again.
                if have and response.status != 206:
                    have = 0
                with partial.open("ab" if have else "wb") as output:
                    shown = -1
                    while chunk := response.read(1024 * 1024):
                        output.write(chunk)
                        have += len(chunk)
                        percent = have * 100 // MODEL_BYTES
                        if percent != shown:
                            shown = percent
                            progress("weights", done=have, total=MODEL_BYTES)
        except (OSError, http.client.HTTPException) as error:
            if attempt + 1 == DOWNLOAD_ATTEMPTS:
                raise RuntimeError(f"Koharu checkpoint download failed: {error}") from error
            time.sleep(min(30, 2 ** attempt))
    if not partial.is_file() or partial.stat().st_size != MODEL_BYTES or digest(partial) != MODEL_SHA256:
        partial.unlink(missing_ok=True)
        raise RuntimeError("Koharu checkpoint SHA-256 mismatch")
    os.replace(partial, target)
    return target


def extract_hi_sam_archive(archive: Path, source: Path) -> None:
    """Extract the pinned source snapshot without accepting paths outside it."""
    expected_root = f"Hi-SAM-{HI_SAM_REVISION}"
    with zipfile.ZipFile(archive) as bundle:
        for member in bundle.infolist():
            parts = Path(member.filename).parts
            if parts == (expected_root,) and member.is_dir():
                continue
            if len(parts) < 2 or ".." in parts or parts[0] != expected_root:
                raise RuntimeError("Unexpected Hi-SAM archive entry")
            destination = source.joinpath(*parts[1:])
            if member.is_dir():
                destination.mkdir(parents=True, exist_ok=True)
            else:
                destination.parent.mkdir(parents=True, exist_ok=True)
                with bundle.open(member) as stream, destination.open("wb") as output:
                    while chunk := stream.read(1024 * 1024):
                        output.write(chunk)


def hi_sam(root: Path) -> Path:
    source = root / "Hi-SAM"
    archive = root / "Hi-SAM.zip"
    if not archive.is_file() or digest(archive) != HI_SAM_ARCHIVE_SHA256:
        url = f"https://codeload.github.com/ymy-k/Hi-SAM/zip/{HI_SAM_REVISION}"
        with urllib.request.urlopen(url, timeout=120) as response, archive.open("wb") as output:
            import shutil
            shutil.copyfileobj(response, output)
    if digest(archive) != HI_SAM_ARCHIVE_SHA256:
        raise RuntimeError("Hi-SAM source archive SHA-256 mismatch")
    if source.exists():
        import shutil
        shutil.rmtree(source)
    source.mkdir(parents=True, exist_ok=True)
    extract_hi_sam_archive(archive, source)
    (source / ".pinned-revision").write_text(HI_SAM_REVISION + "\n")
    (source / ".archive-sha256").write_text(HI_SAM_ARCHIVE_SHA256 + "\n")
    return source


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    # The desktop passes the pinned table so every platform exports the same
    # head bytes. The cloud seed omits it and keeps its own Linux pin.
    parser.add_argument("--dense-pe", type=Path)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    weights = checkpoint(root)
    progress("source")
    architecture = hi_sam(root)
    progress("export")
    proof = root / "proof"
    subprocess.run([
        sys.executable, str(arguments.source / "export_mask.py"),
        "--input", str(arguments.source / "synthetic_page.png"),
        "--weights", str(weights),
        "--hi-sam-root", str(architecture),
        "--output-dir", str(proof),
        "--phase", "export",
        *(["--dense-pe", str(arguments.dense_pe)] if arguments.dense_pe else []),
    ], check=True)


if __name__ == "__main__":
    main()
