"""The sidecar must start and report its memory where `resource` does not exist."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import types
import unittest
from unittest import mock

SIDECAR_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Python on Windows has no `resource`. This imports what `python -m
# manga_cleaner_sidecar` imports, as that Python does.
WITHOUT_RESOURCE = """
import json, sys
sys.path.insert(0, sys.argv[1])
sys.modules["resource"] = None
import manga_cleaner_sidecar.__main__
from manga_cleaner_sidecar.memory import build_memory_report
print(json.dumps(build_memory_report().to_dict()))
"""


class MemoryReportTest(unittest.TestCase):
    def test_the_entry_point_imports_and_reports_without_resource(self) -> None:
        done = subprocess.run(
            [sys.executable, "-c", WITHOUT_RESOURCE, SIDECAR_ROOT],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        report = json.loads(done.stdout.strip().splitlines()[-1])
        self.assertIsInstance(report["peak_rss_bytes"], int)

    def test_the_gpu_figure_is_torch_on_a_discrete_gpu_and_none_otherwise(self) -> None:
        sys.path.insert(0, SIDECAR_ROOT)
        from manga_cleaner_sidecar import memory

        cuda = types.SimpleNamespace(is_initialized=lambda: True, memory_reserved=lambda: 8_000_000_000)
        with mock.patch.dict(sys.modules, {"torch": types.SimpleNamespace(cuda=cuda)}):
            self.assertEqual(memory.get_gpu_bytes(), 8_000_000_000)
            self.assertEqual(memory.build_memory_report().to_dict()["gpu_bytes"], 8_000_000_000)
        unused = types.SimpleNamespace(is_initialized=lambda: False)
        with mock.patch.dict(sys.modules, {"torch": types.SimpleNamespace(cuda=unused, xpu=unused)}):
            self.assertIsNone(memory.get_gpu_bytes())

    @unittest.skipUnless(sys.platform == "win32", "the working set is a Windows reading")
    def test_windows_reports_a_real_working_set(self) -> None:
        sys.path.insert(0, SIDECAR_ROOT)
        from manga_cleaner_sidecar import memory

        self.assertGreater(memory.get_peak_rss_bytes(), 0)
        self.assertGreater(memory.get_current_rss_bytes(), 0)
        self.assertEqual(memory.build_memory_report().instrument, "ru_maxrss")


if __name__ == "__main__":
    unittest.main()
