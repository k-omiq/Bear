"""Denoise models this computer sends to an installation's weights volume.

The seed job downloads the denoise models on the provider's machines. A host that
refuses those machines (bucket.yor.ovh answers 403 to Modal's addresses) leaves its
model out, and the presets that need it are not advertised
(`DenoiseModel.optional`, deploy/cloud/common/denoise.py). The setup helper runs on
the user's computer, which such a host does serve. `fetch_model` downloads the model
here and checks it is the pinned file; the driver then uploads it to the volume.

Nothing here keeps a second public copy of a model: the file goes from its own host
to the user's computer and from there to the user's own volume.
"""
from __future__ import annotations

import hashlib
import lzma  # noqa: F401  (tarfile opens .tar.xz with it; named here so the frozen helper carries it)
import shutil
import tarfile
import urllib.request
import zipfile
from pathlib import Path
from typing import IO, Any

from provisioner.endpoint import tls_context

# Per socket operation, not for the whole download: the driver bounds that.
SOCKET_TIMEOUT_SECONDS = 60.0
# The archives are tens of megabytes. More than this is not one of them.
MAX_DOWNLOAD_BYTES = 1 << 30
USER_AGENT = "manga-cleaner-setup"
_CHUNK = 1 << 20


class LocalModelError(Exception):
    """The model could not be fetched, or what came back is not the pinned file."""


def _copy(source: IO[bytes], target: Path, limit: int) -> None:
    """Copy at most `limit` bytes; more than that fails instead of filling the disk."""
    written = 0
    with target.open("wb") as out:
        for chunk in iter(lambda: source.read(_CHUNK), b""):
            written += len(chunk)
            if written > limit:
                raise LocalModelError(f"more than {limit} bytes")
            out.write(chunk)


def _download(url: str, target: Path) -> None:
    if not url.startswith("https://"):
        raise LocalModelError("only https downloads are allowed")
    opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=tls_context()))
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with opener.open(request, timeout=SOCKET_TIMEOUT_SECONDS) as response:
        _copy(response, target, MAX_DOWNLOAD_BYTES)


def _extract(archive: Path, member: str, target: Path, limit: int) -> None:
    if archive.name.endswith((".tar.xz", ".tar.gz", ".tar")):
        with tarfile.open(archive) as tar:
            source = tar.extractfile(member)
            if source is None:
                raise LocalModelError(f"{member} is not a file in the archive")
            with source:
                _copy(source, target, limit)
    else:
        with zipfile.ZipFile(archive) as bundle, bundle.open(member) as source:
            _copy(source, target, limit)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(_CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch_model(model: Any, directory: str) -> str:
    """Download `model` (a DenoiseModel) into `directory`; the path of the verified file.

    Raises LocalModelError when the host does not serve it or the file is not the
    pinned one. Whatever was downloaded is removed in that case.
    """
    folder = Path(directory)
    target = folder / model.file_name
    # The archive keeps its extension: it tells tar from zip.
    download = folder / ("download-" + model.url.rsplit("/", 1)[-1]) if model.archive_member else target
    try:
        _download(model.url, download)
        if model.archive_member:
            # One byte over the pinned size is enough to know it is another file.
            _extract(download, model.archive_member, target, model.bytes + 1)
        size, digest = target.stat().st_size, _sha256(target)
        if size != model.bytes or digest != model.sha256:
            raise LocalModelError(f"{model.model_id} is not the pinned file (sha256 {digest}, {size} bytes)")
    except LocalModelError:
        target.unlink(missing_ok=True)
        raise
    except Exception as exc:
        target.unlink(missing_ok=True)
        raise LocalModelError(f"{model.model_id}: {type(exc).__name__}: {exc}"[:300]) from None
    finally:
        if download != target:
            download.unlink(missing_ok=True)
    return str(target)
