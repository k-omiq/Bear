"""The sidecar must keep running with its parent and end when the parent ends."""

from __future__ import annotations

import ctypes
import os
import subprocess
import sys
import threading
import time
import unittest
from unittest import mock

SIDECAR_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, SIDECAR_ROOT)

from manga_cleaner_sidecar import watchdog

# Bounded, so that a watchdog that fails this test leaves no process behind.
CHILD = """
import sys, time
sys.path.insert(0, sys.argv[1])
from manga_cleaner_sidecar.watchdog import start_parent_watchdog
start_parent_watchdog(int(sys.argv[2]))
for _ in range(600):
    print("alive", flush=True)
    time.sleep(0.1)
"""

PARENT = """
import os, subprocess, sys, time
subprocess.Popen([sys.executable, "-c", sys.argv[1], sys.argv[2], str(os.getpid())], stdout=sys.stdout)
time.sleep(60)
"""


class ParentWatchdogTest(unittest.TestCase):
    def test_child_runs_with_its_parent_and_ends_with_it(self) -> None:
        parent = subprocess.Popen(
            [sys.executable, "-c", PARENT, CHILD, SIDECAR_ROOT],
            stdout=subprocess.PIPE,
            text=True,
        )
        self.addCleanup(parent.stdout.close)
        self.addCleanup(parent.wait)
        self.addCleanup(parent.kill)

        last_line = [time.monotonic()]
        closed = threading.Event()

        def read() -> None:
            for _ in parent.stdout:
                last_line[0] = time.monotonic()
            closed.set()

        threading.Thread(target=read, daemon=True).start()

        # Longer than the POSIX poll, and than any exit a wrong answer would cause.
        time.sleep(4)
        self.assertLess(time.monotonic() - last_line[0], 2, "the child stopped with its parent alive")

        parent.kill()
        self.assertTrue(closed.wait(15), "the child outlived its parent")


class WindowsWaitTest(unittest.TestCase):
    """The branch only Windows takes, with the system calls faked."""

    def wait(self, handle: int, wait_answer: int = 0, last_error: int = 0) -> mock.Mock:
        kernel32 = mock.MagicMock()
        kernel32.OpenProcess.return_value = handle
        kernel32.WaitForSingleObject.return_value = wait_answer
        with mock.patch.object(ctypes, "WinDLL", return_value=kernel32, create=True), mock.patch.object(
            ctypes, "get_last_error", return_value=last_error, create=True
        ), mock.patch.object(os, "_exit") as exit_now:
            watchdog._wait_for_parent_windows(4242)
        kernel32.OpenProcess.assert_called_once_with(watchdog._SYNCHRONIZE, False, 4242)
        return exit_now

    def test_exits_when_the_parent_ends(self) -> None:
        self.wait(handle=7).assert_called_once_with(0)

    def test_a_failed_wait_is_not_an_ended_parent(self) -> None:
        self.wait(handle=7, wait_answer=0xFFFFFFFF).assert_not_called()

    def test_exits_when_the_parent_is_already_gone(self) -> None:
        self.wait(handle=0, last_error=87).assert_called_once_with(0)

    def test_a_refused_handle_is_not_a_gone_parent(self) -> None:
        self.wait(handle=0, last_error=5).assert_not_called()


if __name__ == "__main__":
    unittest.main()
