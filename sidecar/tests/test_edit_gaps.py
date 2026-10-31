"""Gaps in the local FLUX 3 and the model table found on the 2026-10-31 bug hunt. A stand-in for
torch and a fake pipeline: no model, no GPU. Each test states the behaviour wanted and is skipped
with the reason until the code does it. Needs numpy and Pillow, which `edit.py` needs too."""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from contextlib import contextmanager
from types import SimpleNamespace
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

try:
    import numpy as np
    from PIL import Image
except ImportError:  # pragma: no cover
    np = None

from manga_cleaner_sidecar.backend.sdnq import SdnqBackend  # noqa: E402
from manga_cleaner_sidecar.protocol import ImageBuffer  # noqa: E402


class _Tensor:
    """What `render_flux3` does to its noise, over a numpy array."""

    def __init__(self, array):
        self.array = array

    def to(self, _target):
        return self

    @property
    def T(self):
        return _Tensor(self.array.T)

    def reshape(self, *shape):
        return _Tensor(self.array.reshape(*shape))


def fake_torch():
    @contextmanager
    def inference_mode():
        yield

    generator = SimpleNamespace(manual_seed=lambda seed: seed)
    return SimpleNamespace(
        randn=lambda shape, generator: _Tensor(np.zeros(shape, dtype=np.float32)),
        Generator=lambda: generator,
        bfloat16="bfloat16",
        inference_mode=inference_mode,
    )


class EchoPipeline:
    """A model that changed nothing, as FLUX 3 did on 5 of 19 real crops."""

    transformer = SimpleNamespace(config=SimpleNamespace(in_channels=4))

    def __init__(self):
        self.calls = 0

    def __call__(self, **kwargs):
        self.calls += 1
        return [kwargs["image"].copy()]


@unittest.skipIf(np is None, "numpy and Pillow are not installed")
class LocalFlux3GapsTest(unittest.TestCase):
    def test_lettering_the_edit_left_in_place_does_not_come_back_as_a_clean(self) -> None:
        page = Image.new("RGB", (768, 512), (255, 255, 255))
        for x in range(200, 560, 12):
            page.paste((10, 10, 10), (x, 200, x + 6, 300))
        hint = Image.new("L", page.size, 0)
        hint.paste(255, (200, 200, 560, 300))
        backend = SdnqBackend()
        backend.family = "flux3"
        backend.pipeline = EchoPipeline()
        backend.device = "cpu"
        image = ImageBuffer.from_pil(page, 768, 512, "rgb8")
        with mock.patch.dict(sys.modules, {"torch": fake_torch()}), \
                mock.patch.object(backend, "_prompt_embeds", return_value="embeds"), \
                mock.patch.object(backend, "_answer", side_effect=lambda image, result, start: (result, 0)):
            result, _ = backend._render_edit(image, page, ImageBuffer.from_pil(hint, 768, 512, "gray8"), "words", 16, 1, 1.0)
        changed = np.abs(np.asarray(result, dtype=np.int16) - np.asarray(page, dtype=np.int16)).sum()
        self.assertTrue(backend.pipeline.calls > 1 or changed > 0, "the same crop came back after one edit")


class WorkingSetGapsTest(unittest.TestCase):
    def test_a_declared_peak_is_never_below_the_weights_on_disk(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            folder = os.path.join(root, "flux3-image-klein")
            os.mkdir(folder)
            open(os.path.join(folder, "modular_model_index.json"), "w").close()
            with open(os.path.join(folder, "transformer.safetensors"), "wb") as handle:
                handle.truncate(20_320_984_775)  # The bf16 weights the cloud also serves.
            backend = SdnqBackend(weights_dir=root)
            backend.active_model_id = "flux3-image-klein"
            from manga_cleaner_sidecar.backend.sdnq import find_weights_dir

            backend.weights_dir = find_weights_dir("flux3-image-klein", root)
            self.assertGreaterEqual(backend.get_working_set_bytes(), backend.get_weights_bytes())


if __name__ == "__main__":
    unittest.main()
