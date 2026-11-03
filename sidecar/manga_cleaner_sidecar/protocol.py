"""Wire protocol definitions and serialization for Manga Cleaner sidecar.

Implements the exact JSON and raw binary pixel schema expected by the Rust
client in cleaner-core::sidecar::wire.
"""

from __future__ import annotations

import base64
import dataclasses
from dataclasses import dataclass
from enum import Enum
from typing import Any, Dict, List, Optional
from PIL import Image as PILImage


PROTOCOL_VERSION: int = 1


class State(str, Enum):
    """Lifecycle state of the sidecar engine."""
    IDLE = "idle"
    LOADING = "loading"
    READY = "ready"
    BUSY = "busy"


class Basis(str, Enum):
    """Basis for working set memory estimation."""
    DECLARED = "declared"
    MEASURED = "measured"


class ErrorKind(str, Enum):
    """Taxonomy of sidecar error kinds matching cleaner-core wire definitions."""
    UNAUTHORIZED = "unauthorized"
    BUSY = "busy"
    NOT_OPEN = "not_open"
    BAD_REQUEST = "bad_request"
    UNBOUNDED_BACKEND = "unbounded_backend"
    WEIGHTS_MISSING = "weights_missing"
    OUT_OF_MEMORY = "out_of_memory"
    TORCH_WITHOUT_GPU = "torch_without_gpu"
    GPU_UNREACHABLE = "gpu_unreachable"
    INTERNAL = "internal"


class SidecarError(Exception):
    """Structured exception mapped directly to wire error codes and taxonomy."""

    def __init__(
        self,
        status_code: int,
        kind: ErrorKind | str,
        detail: Optional[str] = None,
    ) -> None:
        super().__init__(detail or str(kind))
        self.status_code = status_code
        self.kind = str(kind.value if isinstance(kind, ErrorKind) else kind)
        self.detail = detail

    def to_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {"kind": self.kind}
        if self.detail:
            result["detail"] = self.detail
        return {"error": result}


@dataclass
class ImageBuffer:
    """A rectangle of interleaved 8-bit samples, base64-encoded over the wire.

    The application transfers raw samples rather than compressed PNG bytes to
    prevent silent colour space conversions, profile shifts, or palette corruptions
    from occurring across the process seam.
    """
    width: int
    height: int
    channels: int
    encoding: str
    data: str

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> ImageBuffer:
        if not isinstance(data, dict):
            raise SidecarError(422, ErrorKind.BAD_REQUEST, "Image payload must be a JSON object")
        try:
            width = int(data["width"])
            height = int(data["height"])
            channels = int(data["channels"])
            encoding = str(data["encoding"]).lower()
            raw_b64 = str(data["data"])
        except (KeyError, ValueError, TypeError) as exc:
            raise SidecarError(422, ErrorKind.BAD_REQUEST, f"Malformed image metadata: {exc}") from exc

        if width <= 0 or height <= 0:
            raise SidecarError(422, ErrorKind.BAD_REQUEST, f"Invalid image dimensions: {width}x{height}")

        expected_channels = 3 if encoding == "rgb8" else 1 if encoding == "gray8" else None
        if expected_channels is None:
            raise SidecarError(422, ErrorKind.BAD_REQUEST, f"Unsupported encoding: {encoding}")
        if channels != expected_channels:
            raise SidecarError(
                422,
                ErrorKind.BAD_REQUEST,
                f"Channel count {channels} does not match encoding '{encoding}' (expected {expected_channels})",
            )

        return cls(width=width, height=height, channels=channels, encoding=encoding, data=raw_b64)

    def to_dict(self) -> Dict[str, Any]:
        return {
            "width": self.width,
            "height": self.height,
            "channels": self.channels,
            "encoding": self.encoding,
            "data": self.data,
        }

    def to_pil(self) -> PILImage.Image:
        """Decode base64 raw samples into a PIL Image."""
        try:
            raw_bytes = base64.b64decode(self.data, validate=True)
        except Exception as exc:
            raise SidecarError(422, ErrorKind.BAD_REQUEST, f"Invalid base64 image data: {exc}") from exc

        expected_len = self.width * self.height * self.channels
        if len(raw_bytes) != expected_len:
            raise SidecarError(
                422,
                ErrorKind.BAD_REQUEST,
                f"Decoded buffer size ({len(raw_bytes)} bytes) does not match declared "
                f"geometry {self.width}x{self.height}x{self.channels} ({expected_len} bytes)",
            )

        if self.encoding == "rgb8":
            return PILImage.frombytes("RGB", (self.width, self.height), raw_bytes)
        elif self.encoding == "gray8":
            # For diffusion inpainting we convert grayscale inputs into RGB mode
            return PILImage.frombytes("L", (self.width, self.height), raw_bytes).convert("RGB")
        else:
            raise SidecarError(422, ErrorKind.BAD_REQUEST, f"Unsupported image encoding: {self.encoding}")

    @classmethod
    def from_pil(
        cls,
        image: PILImage.Image,
        target_width: int,
        target_height: int,
        encoding: str = "rgb8",
    ) -> ImageBuffer:
        """Convert a PIL Image back to an ImageBuffer with exact geometry."""
        # Enforce exact response geometry matching the original request
        if image.size != (target_width, target_height):
            image = image.resize((target_width, target_height), PILImage.Resampling.LANCZOS)

        encoding_lower = encoding.lower()
        if encoding_lower == "rgb8":
            rgb_img = image.convert("RGB")
            raw_bytes = rgb_img.tobytes()
            channels = 3
        elif encoding_lower == "gray8":
            gray_img = image.convert("L")
            raw_bytes = gray_img.tobytes()
            channels = 1
        else:
            raise SidecarError(422, ErrorKind.BAD_REQUEST, f"Unknown target encoding: {encoding}")

        b64_str = base64.b64encode(raw_bytes).decode("ascii")
        return cls(
            width=target_width,
            height=target_height,
            channels=channels,
            encoding=encoding_lower,
            data=b64_str,
        )


@dataclass
class MemoryReport:
    """Memory consumption report matching cleaner-core wire specifications.

    The two primary fields keep their historical names and no longer carry a
    resident set size. They carry the process's **physical footprint**, which
    is the quantity the operating system, and therefore the user, calls memory
    used; `instrument` says so explicitly so no reader has to infer it from a
    field name. See `memory.py` for why the two differ by more than a factor of
    two here.
    """
    peak_rss_bytes: int
    rss_bytes: Optional[int] = None
    cache_bytes: Optional[int] = None
    #: Names the quantity `peak_rss_bytes` and `rss_bytes` hold: either
    #: "phys_footprint" or, where no Mach ledger could be read, "ru_maxrss".
    instrument: Optional[str] = None
    #: The footprint readings under their own names, so the primary fields can
    #: be audited rather than trusted.
    footprint_bytes: Optional[int] = None
    peak_footprint_bytes: Optional[int] = None
    #: The resident readings, kept so the disagreement between the two
    #: instruments stays visible on the wire instead of being averaged away.
    resident_bytes: Optional[int] = None
    peak_resident_bytes: Optional[int] = None
    #: The inference backend's own high-water mark, where it can say. A third
    #: instrument that agrees with neither of the others.
    backend_peak_bytes: Optional[int] = None
    #: Held on a discrete GPU now, which is in neither instrument above. Not in the
    #: primary fields: the parent checks those against this machine's RAM.
    gpu_bytes: Optional[int] = None

    def to_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {"peak_rss_bytes": self.peak_rss_bytes}
        for name in (
            "rss_bytes",
            "cache_bytes",
            "instrument",
            "footprint_bytes",
            "peak_footprint_bytes",
            "resident_bytes",
            "peak_resident_bytes",
            "backend_peak_bytes",
            "gpu_bytes",
        ):
            value = getattr(self, name)
            if value is not None:
                result[name] = value
        return result


@dataclass
class ModelMetadata:
    """Model identification metadata."""
    id: str
    quantization: Optional[str] = "int4"
    source: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {"id": self.id}
        if self.quantization is not None:
            result["quantization"] = self.quantization
        if self.source is not None:
            result["source"] = self.source
        return result
