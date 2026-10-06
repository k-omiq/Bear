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


if __name__ == "__main__":
    unittest.main()
