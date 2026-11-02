"""Parent process watchdog for Manga Cleaner sidecar.

The sidecar process holds multi-gigabyte neural network weights and GPU allocations
in unified memory. If the parent application crashes or is terminated abruptly,
the operating system does not automatically terminate child background processes
that are listening on network sockets.

This watchdog thread watches the parent process and forces an immediate exit
(`os._exit(0)`) the moment the parent process disappears. POSIX polls the parent
PID at regular intervals. Windows waits on the parent's process handle.
"""

from __future__ import annotations

import os
import sys
import threading
import time
from typing import Optional

_SYNCHRONIZE = 0x00100000
_INFINITE = 0xFFFFFFFF
_WAIT_OBJECT_0 = 0
_ERROR_INVALID_PARAMETER = 87


def _wait_for_parent_windows(parent_pid: int) -> None:
    """Exit when the parent ends, by waiting on its process handle.

    `os.kill(pid, 0)` does not test a PID on Windows: Python reads signal 0 as
    CTRL_C_EVENT and sends it to a console process group, so its answer says
    nothing about whether the process is alive.
    """
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
    kernel32.WaitForSingleObject.restype = wintypes.DWORD

    handle = kernel32.OpenProcess(_SYNCHRONIZE, False, parent_pid)
    if not handle:
        # Only "no such process" says the parent is gone. Any other refusal
        # leaves a parent that may be alive, whose sidecar must keep working.
        if ctypes.get_last_error() == _ERROR_INVALID_PARAMETER:
            os._exit(0)
        return
    if kernel32.WaitForSingleObject(handle, _INFINITE) == _WAIT_OBJECT_0:
        os._exit(0)


def _poll_parent_posix(parent_pid: int, poll_interval_sec: float) -> None:
    while True:
        time.sleep(poll_interval_sec)

        # Check if parent PID is still alive by sending signal 0
        try:
            os.kill(parent_pid, 0)
        except OSError:
            # The parent process no longer exists. Terminate immediately without
            # running standard Python exit handlers to prevent hanging on locks.
            os._exit(0)

        # If the parent process dies, the child process is immediately adopted
        # by init or launchd (PID 1). If getppid() no longer equals the original
        # parent_pid, the original parent has exited.
        try:
            if os.getppid() != parent_pid:
                os._exit(0)
        except Exception:
            pass


def start_parent_watchdog(parent_pid: Optional[int], poll_interval_sec: float = 1.0) -> None:
    """Start a background daemon thread that monitors the parent PID."""
    if parent_pid is None or parent_pid <= 1:
        return

    def _watchdog_loop() -> None:
        if sys.platform == "win32":
            _wait_for_parent_windows(parent_pid)
        else:
            _poll_parent_posix(parent_pid, poll_interval_sec)

    thread = threading.Thread(
        target=_watchdog_loop,
        name="manga-cleaner-parent-watchdog",
        daemon=True,
    )
    thread.start()
