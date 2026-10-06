"""Abstract base class definition for sidecar inference backends."""

from __future__ import annotations

import abc
from typing import Any, Dict, List, Optional, Tuple

from ..protocol import Basis, ImageBuffer, ModelMetadata

#: How far the hole reaches past the hint, in crop pixels: the parent writes the
#: answer up to `ISOLATION_RADIUS` (5 px, crates/cleaner-core/src/constants.rs) from
#: the lettering, and 3 px more hand the model the lettering's antialiased edge.
#: deploy/cloud/common/flux.py holds the same number for the cloud worker.
HOLE_GROWTH = 8


def hole_for(hint: Optional[ImageBuffer], width: int, height: int, work_w: int, work_h: int) -> Any:
    """The region a backend may redraw, as a 0/255 PIL "L" image at the working size.

    The hint's lettering grown by :data:`HOLE_GROWTH`, resized without new grey
    levels. No hint is the whole crop: the edit the backend did before the hole.
    """
    from PIL import Image as PILImage
    from PIL import ImageFilter

    if hint is None:
        return PILImage.new("L", (work_w, work_h), 255)
    lettering = hint.to_pil().convert("L")
    if lettering.size != (width, height):
        from ..protocol import ErrorKind, SidecarError

        raise SidecarError(422, ErrorKind.BAD_REQUEST, "Hint geometry differs from the image.")
    hole = lettering.point(lambda v: 255 if v else 0).filter(ImageFilter.MaxFilter(2 * HOLE_GROWTH + 1))
    return hole if hole.size == (work_w, work_h) else hole.resize((work_w, work_h), PILImage.Resampling.NEAREST)


class BackendBase(abc.ABC):
    """Abstract interface that all sidecar backends must implement."""

    @property
    @abc.abstractmethod
    def backend_id(self) -> str:
        """The identifier string of this backend (e.g. 'mflux', 'sdcpp')."""
        raise NotImplementedError

    @abc.abstractmethod
    def get_model_metadata(self) -> ModelMetadata:
        """Return the metadata for the active or configured model."""
        raise NotImplementedError

    @abc.abstractmethod
    def get_weights_bytes(self) -> Optional[int]:
        """Return the size of the model weights on disk in bytes."""
        raise NotImplementedError

    @abc.abstractmethod
    def get_working_set_bytes(self) -> Optional[int]:
        """Return the estimated or measured working set in bytes."""
        raise NotImplementedError

    @abc.abstractmethod
    def get_basis(self) -> Basis:
        """Return whether working set is declared or measured."""
        raise NotImplementedError

    @abc.abstractmethod
    def get_applied_guards(self) -> List[str]:
        """Return the list of memory guards and constraints currently in force."""
        raise NotImplementedError

    @abc.abstractmethod
    def open(
        self,
        model_id: str,
        limits: Dict[str, int],
        require: List[str],
    ) -> Dict[str, Any]:
        """Load model weights and apply memory bounding guards.

        Raises SidecarError if a required guard cannot be satisfied, weights are missing,
        or an allocation failure occurs.
        """
        raise NotImplementedError

    @abc.abstractmethod
    def render(
        self,
        image: ImageBuffer,
        hint: Optional[ImageBuffer],
        prompt: str,
        steps: int,
        seed: int,
        guidance: float,
        deadline_ms: int,
    ) -> Tuple[ImageBuffer, int]:
        """Redraw the hint's lettering (see :func:`hole_for`) and keep the rest of the crop.

        Returns a tuple of (output_image_buffer, elapsed_milliseconds).
        """
        raise NotImplementedError

    @abc.abstractmethod
    def release(self) -> None:
        """Unload model weights, reclaim caches, and return memory to floor."""
        raise NotImplementedError
