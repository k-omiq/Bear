"""Fetch pinned Koharu sources and export the app's local SAM-TS-L graphs.

Runs under a managed Python environment started by the desktop command. The
checkpoint and converted graphs stay in the user's app data directory.
"""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

MODEL_REVISION = "5dd97423e0fbf2404264979136d47e8101144046"
MODEL_SHA256 = "bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45"
HI_SAM_REVISION = "69009434d4dba5541f228d8f5acb0754c333d417"
HI_SAM_ARCHIVE_SHA256 = "f18fb049813f9b9319ac074449f4539fbe5b58f4e3f450ce331578d74bc3e327"


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


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
    from huggingface_hub import hf_hub_download
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    checkpoint = Path(hf_hub_download(
        repo_id="mayocream/koharu-text-sam-ts-l",
        filename="model.safetensors",
        revision=MODEL_REVISION,
        local_dir=root / "checkpoint",
    ))
    if digest(checkpoint) != MODEL_SHA256:
        raise RuntimeError("Koharu checkpoint SHA-256 mismatch")
    architecture = hi_sam(root)
    proof = root / "proof"
    subprocess.run([
        sys.executable, str(arguments.source / "export_mask.py"),
        "--input", str(arguments.source / "synthetic_page.png"),
        "--weights", str(checkpoint),
        "--hi-sam-root", str(architecture),
        "--output-dir", str(proof),
        "--phase", "export",
    ], check=True)


if __name__ == "__main__":
    main()
