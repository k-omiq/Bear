"""Pinned source extraction contracts; no model download or PyTorch required."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch


MODULE = importlib.util.spec_from_file_location("sam_bootstrap", Path(__file__).with_name("bootstrap.py"))
bootstrap = importlib.util.module_from_spec(MODULE)
MODULE.loader.exec_module(bootstrap)


class SourceArchiveTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.archive = self.root / "Hi-SAM.zip"
        self.prefix = f"Hi-SAM-{bootstrap.HI_SAM_REVISION}"

    def archive_with(self, entries: dict[str, bytes]) -> None:
        with zipfile.ZipFile(self.archive, "w") as bundle:
            for name, contents in entries.items():
                bundle.writestr(name, contents)

    def test_root_directory_and_files_extract_and_pin(self) -> None:
        self.archive_with({f"{self.prefix}/": b"", f"{self.prefix}/hi_sam/modeling/build.py": b"pinned\n"})
        pinned_hash = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        with patch.object(bootstrap, "HI_SAM_ARCHIVE_SHA256", pinned_hash):
            source = bootstrap.hi_sam(self.root)
        self.assertEqual((source / "hi_sam/modeling/build.py").read_bytes(), b"pinned\n")
        self.assertEqual((source / ".pinned-revision").read_text().strip(), bootstrap.HI_SAM_REVISION)
        self.assertEqual((source / ".archive-sha256").read_text().strip(), pinned_hash)

    def test_wrong_root_and_traversal_are_rejected(self) -> None:
        for name in ("other/build.py", f"{self.prefix}/../escape.py", "/absolute.py"):
            self.archive_with({name: b"bad"})
            with self.subTest(name=name), self.assertRaisesRegex(RuntimeError, "Unexpected"):
                bootstrap.extract_hi_sam_archive(self.archive, self.root / "source")
        self.assertFalse((self.root / "escape.py").exists())

    def test_archive_hash_mismatch_is_rejected_before_extraction(self) -> None:
        self.archive_with({f"{self.prefix}/hi_sam/modeling/build.py": b"bad"})
        bad_archive = self.archive.read_bytes()
        with patch.object(bootstrap.urllib.request, "urlopen", return_value=io.BytesIO(bad_archive)), self.assertRaisesRegex(RuntimeError, "SHA-256 mismatch"):
            bootstrap.hi_sam(self.root)
        self.assertFalse((self.root / "Hi-SAM").exists())


class Response(io.BytesIO):
    def __init__(self, body: bytes, status: int) -> None:
        super().__init__(body)
        self.status = status


class CheckpointDownloadTest(unittest.TestCase):
    BODY = bytes(range(256)) * 40

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        pins = {"MODEL_BYTES": len(self.BODY), "MODEL_SHA256": hashlib.sha256(self.BODY).hexdigest()}
        for name, value in pins.items():
            patcher = patch.object(bootstrap, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.requests: list[str | None] = []

    def serve(self, *, honour_range: bool, fail_first_after: int | None = None):
        def urlopen(request, timeout):
            ranged = request.get_header("Range")
            self.requests.append(ranged)
            if fail_first_after is not None and len(self.requests) == 1:
                class Broken(Response):
                    def read(inner, size=-1):
                        if inner.tell() >= fail_first_after:
                            raise ConnectionResetError("dropped")
                        return super().read(min(size, fail_first_after - inner.tell()))
                return Broken(self.BODY, 200)
            if ranged and honour_range:
                start = int(ranged.removeprefix("bytes=").removesuffix("-"))
                return Response(self.BODY[start:], 206)
            return Response(self.BODY, 200)
        return patch.object(bootstrap.urllib.request, "urlopen", side_effect=urlopen)

    def test_a_dropped_download_resumes_from_its_partial_file(self) -> None:
        with self.serve(honour_range=True, fail_first_after=4096), patch.object(bootstrap.time, "sleep"):
            target = bootstrap.checkpoint(self.root)
        self.assertEqual(target.read_bytes(), self.BODY)
        self.assertEqual(self.requests, [None, "bytes=4096-"])
        self.assertFalse(target.with_name("model.safetensors.part").exists())

    def test_a_server_that_ignores_the_range_restarts_the_file(self) -> None:
        part = self.root / "checkpoint" / "model.safetensors.part"
        part.parent.mkdir(parents=True)
        part.write_bytes(self.BODY[:100])
        with self.serve(honour_range=False):
            target = bootstrap.checkpoint(self.root)
        self.assertEqual(target.read_bytes(), self.BODY)

    def test_a_verified_checkpoint_is_not_downloaded_again(self) -> None:
        target = self.root / "checkpoint" / "model.safetensors"
        target.parent.mkdir(parents=True)
        target.write_bytes(self.BODY)
        with self.serve(honour_range=True):
            bootstrap.checkpoint(self.root)
        self.assertEqual(self.requests, [])

    def test_wrong_bytes_are_refused_and_not_kept(self) -> None:
        with patch.object(bootstrap, "MODEL_SHA256", "0" * 64), self.serve(honour_range=True), \
                self.assertRaisesRegex(RuntimeError, "SHA-256 mismatch"):
            bootstrap.checkpoint(self.root)
        self.assertFalse((self.root / "checkpoint" / "model.safetensors.part").exists())
        self.assertFalse((self.root / "checkpoint" / "model.safetensors").exists())


if __name__ == "__main__":
    unittest.main()
