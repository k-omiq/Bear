"""Gaps in FLUX 3 v2 found on the 2026-10-31 bug hunt. Each test states the behaviour wanted and
is skipped with the reason until the code does it. Fake pipeline, no model, no GPU."""

from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

try:
    import numpy as np
    from PIL import Image, ImageFilter
except ImportError:  # pragma: no cover - the cloud tests run without them too
    np = None

from deploy.cloud.common.manifest import ADAPTER_DIR, MODEL_PROD_FLUX3, PRODUCTION_MODELS, production_model
from deploy.cloud.common.weights import check_snapshot_files
from deploy.cloud.tests.test_flux3 import flux3_modules, png


def lettered() -> tuple:
    """A white 300 x 200 crop, dark strokes in the hint's box."""
    page = Image.new("RGB", (300, 200), (255, 255, 255))
    for x in range(40, 260, 6):
        page.paste((20, 20, 20), (x, 60, x + 3, 140))
    hint = Image.new("L", (300, 200), 0)
    hint.paste(255, (40, 60, 260, 140))
    return page, hint


@unittest.skipIf(np is None, "numpy and Pillow are not installed")
class Flux3RetryGapsTest(unittest.TestCase):
    def setUp(self) -> None:
        from deploy.cloud.common.flux3 import Flux3EditRunner, lettering_kept

        self.Runner, self.lettering_kept = Flux3EditRunner, lettering_kept

    def test_the_second_edit_runs_once_however_stubborn_the_model(self) -> None:
        mods = flux3_modules()
        mods.pipeline.echo = True
        page, hint = lettered()
        self.Runner("/w", modules=mods).render_png(png(page), png(hint), 300, 200, seed=1, steps=16, guidance_scaled=100)
        self.assertEqual(len(mods.pipeline.calls), 2)

    @unittest.skip("gap: render_png checks only the first edit, so lettering that survives the second is returned as a success")
    def test_lettering_kept_by_the_second_edit_is_not_a_success(self) -> None:
        from deploy.cloud.common.flux import WorkerRenderError

        mods = flux3_modules()
        page, hint = lettered()
        mods.pipeline.answer = page
        with self.assertRaises(WorkerRenderError):
            self.Runner("/w", modules=mods).render_png(png(page), png(hint), 300, 200, seed=1, steps=16, guidance_scaled=100)

    @unittest.skip("gap: a third of the lettering left in place reads under 0.5, so no retry; real crops: 0.35 at 30 % on real_6_00")
    def test_a_third_of_the_lettering_left_in_place_is_lettering_kept(self) -> None:
        page, hint = lettered()
        clean = Image.new("RGB", page.size, (255, 255, 255))
        clean.paste(page.crop((40, 60, 113, 140)), (40, 60))
        self.assertTrue(self.lettering_kept(np.asarray(page), np.asarray(clean), np.asarray(hint) > 0, np))

    @unittest.skip("gap: the check reads the mean of RGB, so coloured lettering as bright as its ground reads flat and is never retried")
    def test_coloured_lettering_as_bright_as_its_ground_is_lettering_kept(self) -> None:
        page = Image.new("RGB", (300, 200), (0, 0, 200))
        for x in range(40, 260, 6):
            page.paste((200, 0, 0), (x, 60, x + 3, 140))
        hint = Image.new("L", (300, 200), 0)
        hint.paste(255, (40, 60, 260, 140))
        self.assertTrue(self.lettering_kept(np.asarray(page), np.asarray(page), np.asarray(hint) > 0, np))

    @unittest.skip("gap: a hint that covers art beside the lettering pays a second edit for a clean first answer; real crops pass 0.5 with 10 to 40 px of padding")
    def test_a_hint_over_art_with_the_lettering_removed_is_not_edited_again(self) -> None:
        # Art kept where the model should keep it, the lettering gone from the answer.
        art = Image.new("RGB", (300, 200), (255, 255, 255))
        for y in range(30, 170, 8):
            art.paste((60, 60, 60), (20, y, 280, y + 4))
        page = art.copy()
        page.paste((0, 0, 0), (130, 90, 170, 110))
        hint = Image.new("L", (300, 200), 0)
        hint.paste(255, (20, 30, 280, 170))
        mods = flux3_modules()
        mods.pipeline.answer = art
        self.Runner("/w", modules=mods).render_png(png(page), png(hint), 300, 200, seed=1, steps=16, guidance_scaled=100)
        self.assertEqual(len(mods.pipeline.calls), 1)


class SnapshotAdapterGapsTest(unittest.TestCase):
    @unittest.skip("bug: FLUX 3 v2 seeds the LaMa adapter but its file list leaves it out, so the check passes without it")
    def test_every_adapter_is_in_the_file_list_the_seed_checks(self) -> None:
        for model_id, model in PRODUCTION_MODELS.items():
            listed = {path for path, _size in model.files}
            for adapter in model.adapters:
                self.assertIn(f"{ADAPTER_DIR}/{adapter.filename}", listed, model_id)

    @unittest.skip("bug: FLUX 3 v2 passes check_snapshot_files with no LaMa graph, then every render fails model_load_failed")
    def test_a_snapshot_without_the_lama_graph_is_not_complete(self) -> None:
        model = production_model(MODEL_PROD_FLUX3)
        with tempfile.TemporaryDirectory() as root:
            for path, size in model.files:
                target = Path(root) / path
                target.parent.mkdir(parents=True, exist_ok=True)
                with open(target, "wb") as handle:
                    handle.truncate(size)
            self.assertFalse(os.path.exists(Path(root) / ADAPTER_DIR / model.adapters[0].filename))
            self.assertNotEqual(check_snapshot_files(root, MODEL_PROD_FLUX3), [])


if __name__ == "__main__":
    unittest.main()
