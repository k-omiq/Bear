"""The download half of the models this computer sends to a volume. No network."""
from __future__ import annotations

import hashlib
import io
import shutil
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest import mock

from provisioner import local_models
from provisioner.local_models import LocalModelError, fetch_model

WEIGHTS = b"pinned model bytes " * 1000


def model(**changes: Any) -> SimpleNamespace:
    values = dict(model_id="test.model", file_name="test--model.pth", url="https://models.invalid/model.tar.xz",
                  sha256=hashlib.sha256(WEIGHTS).hexdigest(), bytes=len(WEIGHTS), archive_member="models/model.pth")
    values.update(changes)
    return SimpleNamespace(**values)


def tar_xz(member: str, data: bytes) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:xz") as tar:
        info = tarfile.TarInfo(member)
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


def zipped(member: str, data: bytes) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as bundle:
        bundle.writestr(member, data)
    return buffer.getvalue()


class FetchModelTest(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.mkdtemp(prefix="mc-local-models-test-")
        self.addCleanup(shutil.rmtree, self.directory, ignore_errors=True)
        self.served = b""
        self.urls: list = []

        def download(url: str, target: Path) -> None:
            self.urls.append(url)
            target.write_bytes(self.served)

        patcher = mock.patch.object(local_models, "_download", download)
        patcher.start()
        self.addCleanup(patcher.stop)

    def left_behind(self) -> list:
        return sorted(path.name for path in Path(self.directory).iterdir())

    def test_a_tar_xz_member_is_extracted_and_verified(self) -> None:
        self.served = tar_xz("models/model.pth", WEIGHTS)
        path = fetch_model(model(), self.directory)
        self.assertEqual(Path(path).read_bytes(), WEIGHTS)
        self.assertEqual(self.urls, ["https://models.invalid/model.tar.xz"])
        self.assertEqual(self.left_behind(), ["test--model.pth"], "the archive is removed")

    def test_a_zip_member_and_a_plain_file_work_too(self) -> None:
        self.served = zipped("model.pth", WEIGHTS)
        path = fetch_model(model(url="https://models.invalid/models.zip", archive_member="model.pth"), self.directory)
        self.assertEqual(Path(path).read_bytes(), WEIGHTS)
        self.served = WEIGHTS
        path = fetch_model(model(url="https://models.invalid/model.pth", archive_member="", file_name="plain.pth"),
                           self.directory)
        self.assertEqual(Path(path).read_bytes(), WEIGHTS)

    def test_another_file_than_the_pinned_one_is_refused_and_removed(self) -> None:
        self.served = tar_xz("models/model.pth", WEIGHTS[:-1] + b"!")
        with self.assertRaisesRegex(LocalModelError, "not the pinned file"):
            fetch_model(model(), self.directory)
        self.assertEqual(self.left_behind(), [])

    def test_a_member_larger_than_the_pinned_size_stops_early(self) -> None:
        self.served = tar_xz("models/model.pth", WEIGHTS * 3)
        with self.assertRaisesRegex(LocalModelError, "more than"):
            fetch_model(model(), self.directory)
        self.assertEqual(self.left_behind(), [])

    def test_a_missing_member_or_a_page_that_is_no_archive_is_refused(self) -> None:
        self.served = tar_xz("other/file.pth", WEIGHTS)
        with self.assertRaises(LocalModelError):
            fetch_model(model(), self.directory)
        self.served = b"<html>Checking your browser</html>"
        with self.assertRaises(LocalModelError):
            fetch_model(model(), self.directory)
        self.assertEqual(self.left_behind(), [])


class DownloadTest(unittest.TestCase):
    def test_only_https_is_allowed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            for url in ("http://models.invalid/model.pth", "file:///etc/hosts", "ftp://models.invalid/model.pth"):
                with self.assertRaisesRegex(LocalModelError, "https"):
                    local_models._download(url, Path(directory) / "out")

    def test_a_download_over_the_limit_stops(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(LocalModelError, "more than 10 bytes"):
                local_models._copy(io.BytesIO(b"x" * 11), Path(directory) / "out", 10)

    def test_the_real_optional_models_are_https_archives_this_module_reads(self) -> None:
        from deploy.cloud.common.denoise import preset_models
        optional = [m for m in preset_models() if m.optional]
        self.assertTrue(optional, "the helper sends at least the MoESR descreen model")
        for m in optional:
            self.assertTrue(m.url.startswith("https://"), m.url)
            if m.archive_member:
                self.assertTrue(m.url.endswith((".tar.xz", ".tar.gz", ".tar", ".zip")), m.url)


if __name__ == "__main__":
    unittest.main()
