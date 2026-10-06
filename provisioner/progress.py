"""IC-2 progress lines on stderr.

The desktop reads the helper's stderr line by line and forwards every line that is
exactly one IC-2 record, so nothing else may ever be written there: the CLI points
the process-level stderr at the null device and hands this module a private copy.

    {"mc_progress":1,"op":"apply","step":"deploy","state":"start","pct":null}
"""

from __future__ import annotations

import json
import threading
from contextlib import contextmanager
from typing import IO, Iterator, Optional

# Fixed ids; the desktop and the UI know each one.
STEPS = (
    "inspect",
    "validate",
    "volume",
    "state",
    "secret",
    "image",
    "deploy",
    "weights",
    "token",
    "endpoint",
    "health",
    "cleanup",
)
STATES = ("start", "done", "fail", "skip")


class StepReporter:
    """Handed to the body of `Progress.step` so long steps can report a percentage."""

    def __init__(self, progress: "Progress", step: str) -> None:
        self._progress = progress
        self._step = step
        self.last_pct: Optional[int] = None

    def pct(self, value: float) -> None:
        pct = max(0, min(100, int(value)))
        if pct != self.last_pct:
            self.last_pct = pct
            self._progress.emit(self._step, "start", pct)


class Progress:
    """Writes IC-2 records for one helper operation. A None stream writes nothing."""

    def __init__(self, op: str, stream: Optional[IO[str]] = None) -> None:
        self.op = op
        self._stream = stream
        self._lock = threading.Lock()

    def emit(self, step: str, state: str, pct: Optional[int] = None) -> None:
        if step not in STEPS or state not in STATES:
            raise ValueError(f"not an IC-2 step or state: {step!r} {state!r}")
        if pct is not None and not (isinstance(pct, int) and 0 <= pct <= 100):
            raise ValueError(f"IC-2 pct must be null or an int 0..100, got {pct!r}")
        if self._stream is None:
            return
        line = json.dumps(
            {"mc_progress": 1, "op": self.op, "step": step, "state": state, "pct": pct},
            separators=(",", ":"),
        )
        with self._lock:
            try:
                self._stream.write(line + "\n")
                self._stream.flush()
            except (OSError, ValueError):
                # The desktop closed the pipe or is gone; progress is best effort and
                # must never turn a working operation into a failed one.
                pass

    def skip(self, step: str) -> None:
        self.emit(step, "skip")

    @contextmanager
    def step(self, step: str) -> Iterator[StepReporter]:
        """start, then done or fail. A step that reported percentages ends done at 100."""
        reporter = StepReporter(self, step)
        self.emit(step, "start")
        try:
            yield reporter
        except BaseException:
            self.emit(step, "fail", reporter.last_pct)
            raise
        self.emit(step, "done", 100 if reporter.last_pct is not None else None)
