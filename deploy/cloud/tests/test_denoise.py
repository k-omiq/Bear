"""Page denoise: recipe rules, wire checks, tiling geometry and the gateway route.

Offline: no GPU, no onnxruntime, no torch. The waifu2x tiler runs against fake
sessions that implement the model contract exactly (scale, then crop `offset`
from each side), so its output must equal a plain nearest-neighbour upscale.
"""

from __future__ import annotations

import base64
import io
import json
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

import numpy as np
from PIL import Image

from deploy.cloud.common import denoise as d
from deploy.cloud.common.api import CloudGateway
from deploy.cloud.modal.settings import ModalSettings


def png_of(array: np.ndarray, mode: str) -> bytes:
    out = io.BytesIO()
    image = Image.fromarray(array)
    assert image.mode == mode
    image.save(out, format="PNG")
    return out.getvalue()


def recipe(*steps):
    return {"schema": 1, "steps": list(steps)}


GRAIN = {"op": "denoise", "engine": "waifu2x-art-scan", "level": 1}


class FakeInput:
    name = "x"


class FakeModelSession:
    """The waifu2x ONNX contract: [1,3,T,T] -> [1,3,T*s-2o,T*s-2o], nearest upscale."""

    def __init__(self, scale: int, offset: int) -> None:
        self.scale, self.offset = scale, offset

    def get_inputs(self):
        return [FakeInput()]

    def run(self, _outputs, feeds):
        x = feeds["x"]
        y = x.repeat(self.scale, axis=2).repeat(self.scale, axis=3)
        o = self.offset
        return [y[:, :, o:y.shape[2] - o, o:y.shape[3] - o]]


class FakeFilterSession:
    def run(self, _outputs, feeds):
        size = int(feeds["tile_size"]) * int(feeds["scale"]) - 2 * int(feeds["offset"])
        # Any positive weights: blending equal values must return those values.
        ramp = np.minimum(np.arange(size) + 1, np.arange(size)[::-1] + 1).astype(np.float32)
        return [np.broadcast_to(ramp[:, None] * ramp[None, :], (3, size, size)).copy()]


class FakeSessions:
    def __init__(self, model: d.DenoiseModel) -> None:
        self.model = model

    def get(self, model):
        if model.model_id == d.SEAM_FILTER.model_id:
            return FakeFilterSession()
        return FakeModelSession(model.scale, model.offset)


class RecipeTests(unittest.TestCase):
    def test_steps_resolve_to_pinned_models(self):
        steps = d.resolve_recipe(recipe(
            {"op": "jpeg", "engine": "mangajpeg-hq"},
            {"op": "sharpen", "engine": "waifu2x-art-scan", "scale": 4, "level": 1, "strength": 0.5},
        ))
        self.assertEqual([s.model_id for s in steps],
                         ["omdb.1x-MangaJPEGHQ", "waifu2x.swin_unet.art_scan.noise1_scale4x"])
        self.assertEqual(steps[1].strength, 0.5)
        ids = [m.model_id for m in d.models_for(steps)]
        self.assertIn(d.SEAM_FILTER.model_id, ids)

    def test_sharpen_without_level_uses_the_plain_scale_model(self):
        (step,) = d.resolve_recipe(recipe({"op": "sharpen", "engine": "waifu2x-cunet-art"}))
        self.assertEqual(step.model_id, "waifu2x.cunet.art.scale2x")

    def test_mangajanai_picks_the_nearest_trained_height(self):
        (step,) = d.resolve_recipe(recipe({"op": "sharpen", "engine": "mangajanai", "scale": 4}))
        self.assertEqual(step.model_id, "mangajanai.4x.auto")
        self.assertEqual(d.mangajanai_for(2, 1809).height, 1920)
        self.assertEqual(d.mangajanai_for(2, 1200).height, 1200)
        self.assertEqual(d.mangajanai_for(4, 3000).height, 2048)
        self.assertEqual(len([m for m in d.models_for([step]) if m.model_id.startswith("mangajanai.")]), 7)
        self.assertTrue(all(m.archive_member for m in d.models_for([step]) if m.family == "spandrel"))

    def test_realcugan_levels_and_native_3x(self):
        cases = {(3, None): "realcugan.up3x-latest-conservative", (2, 1): "realcugan.up2x-latest-denoise1x",
                 (4, 0): "realcugan.up4x-latest-no-denoise"}
        for (scale, level), model_id in cases.items():
            raw = {"op": "sharpen", "engine": "realcugan", "scale": scale, **({} if level is None else {"level": level})}
            with self.subTest(raw=raw):
                self.assertEqual(d.resolve_step(raw).model_id, model_id)
        with self.assertRaises(d.DenoiseError):
            d.resolve_step({"op": "sharpen", "engine": "realcugan", "scale": 3, "level": 1})

    def test_invalid_recipes_are_refused(self):
        bad = [
            {"schema": 2, "steps": [GRAIN]},
            recipe(),
            recipe(GRAIN, GRAIN, GRAIN, GRAIN),
            recipe({"op": "denoise", "engine": "waifu2x-art-scan", "level": 4}),
            recipe({"op": "denoise", "engine": "waifu2x-art-scan", "level": True}),
            recipe({"op": "denoise", "engine": "waifu2x-art-scan"}),
            recipe({"op": "sharpen", "engine": "waifu2x-cunet-art", "scale": 4}),
            recipe({"op": "jpeg", "engine": "mangajpeg-hq", "level": 1}),
            recipe({"op": "jpeg", "engine": "apisr"}),
            recipe({**GRAIN, "strength": 1.5}),
            recipe({**GRAIN, "extra": 1}),
        ]
        for raw in bad:
            with self.subTest(raw=raw), self.assertRaises(d.DenoiseError):
                d.resolve_recipe(raw)

    def test_output_scale_lands_on_the_last_upscaling_step(self):
        sharpen4 = {"op": "sharpen", "engine": "waifu2x-art-scan", "scale": 4}
        sharpen2 = {"op": "sharpen", "engine": "realcugan", "scale": 2}
        self.assertEqual([s.out for s in d.resolve_recipe(recipe(sharpen4))], [1])
        for out in (2, 3, 4):
            steps = d.resolve_recipe({**recipe(sharpen4, GRAIN), "output_scale": out})
            self.assertEqual([s.out for s in steps], [out, 1])
        # The last step that upscales keeps it; an earlier one still returns to size.
        steps = d.resolve_recipe({**recipe(sharpen4, sharpen2), "output_scale": 2})
        self.assertEqual([s.out for s in steps], [1, 2])
        fixed = d.resolve_recipe({**recipe({"op": "sharpen", "engine": "digimanga-bw-4x"}), "output_scale": 4})
        self.assertEqual([s.out for s in fixed], [4])
        bad = [
            {**recipe(sharpen4), "output_scale": 1},
            {**recipe(sharpen4), "output_scale": 5},
            {**recipe(sharpen4), "output_scale": 2.0},
            {**recipe(sharpen4), "output_scale": True},
            {**recipe(sharpen4), "output_scale": None},
            {**recipe(sharpen2), "output_scale": 3},
            {**recipe(sharpen4, sharpen2), "output_scale": 4},
            {**recipe(GRAIN), "output_scale": 2},
            {**recipe({"op": "jpeg", "engine": "mangajpeg-hq"}), "output_scale": 2},
            {**recipe(sharpen4), "scale": 2},
        ]
        for raw in bad:
            with self.subTest(raw=raw), self.assertRaises(d.DenoiseError):
                d.resolve_recipe(raw)

    def test_every_model_is_pinned(self):
        for model in d.MODELS.values():
            with self.subTest(model=model.model_id):
                self.assertRegex(model.sha256, r"^[0-9a-f]{64}$")
                self.assertGreater(model.bytes, 0)
                self.assertTrue(model.url.startswith("https://"))
                if model.family == "waifu2x":
                    self.assertIn(d.W2X_REVISION, model.url)


class PresetTests(unittest.TestCase):
    def test_interface_presets_match_the_cloud_presets(self):
        path = Path(__file__).resolve().parents[3] / "src" / "lib" / "model" / "denoise-presets.json"
        table = json.loads(path.read_text())
        shipped = {p["id"]: p for p in table["presets"]}
        self.assertEqual(list(shipped), list(d.PRESETS))
        for name, recipe_doc in d.PRESETS.items():
            with self.subTest(preset=name):
                self.assertEqual(shipped[name]["recipe"], recipe_doc)
                routes = d.resolve_routes(recipe_doc)
                # The desktop has one model, the ONNX file this preset runs.
                expected = ["cloud", "local"] if name == "waifu2x-scan-4x-n2" else ["cloud"]
                self.assertEqual(shipped[name]["targets"], expected)
                self.assertIn(shipped[name]["kind"], ("bw", "color", "any"))
                # Each preset offers the page's size or twice it (the user's call, 2026-11-14),
                # and every route of its recipe can give that.
                largest = shipped[name]["maxScale"]
                self.assertEqual(largest, 2)
                both = d.resolve_routes({**recipe_doc, "output_scale": largest})
                self.assertEqual([s.out for s in both.all() if d.step_scale(s) > 1][-1:], [2])
                self.assertEqual(len(both.all()), len(routes.all()))
        self.assertEqual({e["id"]: e["step"] for e in table["chainSteps"]}, d.CHAIN_STEPS)
        self.assertEqual([e["id"] for e in table["chainSteps"]], list(d.CHAIN_STEPS))
        for entry in table["chainSteps"]:
            with self.subTest(step=entry["id"]):
                d.resolve_step(entry["step"])
                self.assertEqual(set(entry["credit"]), {"name", "author", "license"})

    def test_the_presets_are_the_users_picks(self):
        bw = {name: [s.model_id for s in d.resolve_routes(r).gray] for name, r in d.PRESETS.items()}
        self.assertEqual(bw["realcugan-2x-auto"], ["realcugan.up2x-latest-conservative"])
        self.assertEqual(bw["moesr-descreen-auto"], ["umzi.ds-moesr-v2.4x"])
        self.assertEqual(bw["ijanai-v3-denoise-4x"], ["ijanai.v3denoise.4x"])
        self.assertEqual(bw["waifu2x-scan-4x-n2"], ["waifu2x.swin_unet.art_scan.noise2_scale4x"])
        for name in ("realcugan-2x-auto", "moesr-descreen-auto"):
            self.assertEqual([s.model_id for s in d.resolve_routes(d.PRESETS[name]).color], ["ijanai.v3denoise.4x"])
        # No shipped preset runs MangaJaNai, the models that turned colour pages grey.
        self.assertFalse(any(m.model_id.startswith("mangajanai.") for m in d.preset_models()))

    def test_only_preset_and_chain_models_are_seeded(self):
        ids = {m.model_id for m in d.preset_models()}
        for kept in ("waifu2x.swin_unet.art_scan.noise2_scale4x", d.SEAM_FILTER.model_id, "omdb.1x-MangaJPEGHQ",
                     "ijanai.v3denoise.4x", "ijanai.v3denoise.2x", "umzi.ds-moesr-v2.4x",
                     "realcugan.up2x-latest-denoise3x", "waifu2x.swin_unet.art_scan.noise1"):
            self.assertIn(kept, ids)
        for dropped in ("omdb.1x-MangaJPEGMQ", "waifu2x.cunet.art.noise1", "mangajanai.2x.1200p",
                        "realcugan.up3x-latest-conservative"):
            self.assertNotIn(dropped, ids)
        self.assertEqual([m.model_id for m in d.preset_models() if m.optional], [d.MOESR.model_id])

    def test_the_worker_image_pins_spandrel_and_its_extra_arches(self):
        app = (Path(__file__).resolve().parents[1] / "modal" / "app.py").read_text()
        self.assertIn('DENOISE_REQUIREMENTS = ("spandrel==0.4.2", "spandrel_extra_arches==0.2.0")', app)


class WireTests(unittest.TestCase):
    def setUp(self):
        self.page = png_of(np.full((8, 8), 128, dtype=np.uint8), "L")
        self.meta = {"protocol_version": d.DENOISE_VERSION, "recipe": recipe(GRAIN),
                     "request_digest": d.request_digest(recipe(GRAIN), self.page)}

    def test_valid_request(self):
        self.assertEqual(len(d.validate_denoise_request(self.meta, self.page).gray), 1)

    def test_digest_binds_recipe_and_page(self):
        other = png_of(np.full((8, 8), 127, dtype=np.uint8), "L")
        with self.assertRaises(d.DenoiseError):
            d.validate_denoise_request(self.meta, other)
        changed = {**self.meta, "recipe": recipe({**GRAIN, "level": 2})}
        with self.assertRaises(d.DenoiseError):
            d.validate_denoise_request(changed, self.page)

    def test_non_png_and_extra_fields_refused(self):
        with self.assertRaises(d.DenoiseError):
            d.validate_denoise_request(self.meta, b"GIF89a" + self.page)
        with self.assertRaises(d.DenoiseError):
            d.validate_denoise_request({**self.meta, "x": 1}, self.page)


class ImageTests(unittest.TestCase):
    def test_waifu2x_tiling_matches_a_plain_upscale(self):
        rng = np.random.default_rng(7)
        rgb = rng.random((3, 203, 517), dtype=np.float32)
        for model_id in ("waifu2x.swin_unet.art_scan.noise1", "waifu2x.swin_unet.art_scan.scale2x",
                         "waifu2x.swin_unet.art_scan.scale4x", "waifu2x.cunet.art.noise2",
                         "waifu2x.cunet.art.scale2x"):
            model = d.MODELS[model_id]
            with self.subTest(model=model_id):
                out = d.Waifu2x(FakeSessions(model), model, tile=64).run(rgb)
                expected = rgb.repeat(model.scale, axis=1).repeat(model.scale, axis=2)
                self.assertEqual(out.shape, expected.shape)
                np.testing.assert_allclose(out, expected, atol=1e-5)

    def test_downscale_keeps_flat_pages_flat(self):
        flat = np.full((3, 40, 60), 0.3, dtype=np.float32)
        out = d.downscale_linear_lanczos(flat, 30, 20)
        self.assertEqual(out.shape, (3, 20, 30))
        np.testing.assert_allclose(out, 0.3, atol=1e-4)

    def test_gray_page_stays_gray_and_same_size(self):
        png = png_of(np.arange(35 * 17, dtype=np.uint8).reshape(17, 35), "L")
        page = d.decode_page(png)
        self.assertTrue(page.gray)
        image = Image.open(io.BytesIO(d.encode_page(page, page.rgb)))
        self.assertEqual((image.mode, image.size), ("L", (35, 17)))

    def test_output_scale_enlarges_the_answer(self):
        rng = np.random.default_rng(3)
        gray = (rng.random((23, 31)) * 255).astype(np.uint8)
        png = png_of(gray, "L")
        sharpen = {"op": "sharpen", "engine": "waifu2x-art-scan", "scale": 4}
        sessions = FakeSessions(d.MODELS["waifu2x.swin_unet.art_scan.scale4x"])
        # No output_scale: the page's own size, as before.
        out, info = d.denoise_page(sessions, d.resolve_recipe(recipe(sharpen)), png)
        self.assertEqual((Image.open(io.BytesIO(out)).size, info["width"], info["height"]), ((31, 23), 31, 23))
        # The model's own scale: nothing is scaled down, so the fake's nearest upscale comes back.
        out, info = d.denoise_page(sessions, d.resolve_recipe({**recipe(sharpen), "output_scale": 4}), png)
        image = Image.open(io.BytesIO(out))
        self.assertEqual((image.mode, image.size, info["width"], info["height"]), ("L", (124, 92), 124, 92))
        np.testing.assert_array_equal(np.asarray(image), gray.repeat(4, axis=0).repeat(4, axis=1))
        # Between the two: scaled down to that multiple, and a later 1x step runs there.
        for out_scale in (2, 3):
            steps = d.resolve_recipe({**recipe({**sharpen, "strength": 0.5}, GRAIN), "output_scale": out_scale})
            out, info = d.denoise_page(sessions, steps, png)
            with self.subTest(out_scale=out_scale):
                self.assertEqual(Image.open(io.BytesIO(out)).size, (31 * out_scale, 23 * out_scale))
                self.assertEqual((info["width"], info["height"], len(info["steps"])), (31 * out_scale, 23 * out_scale, 2))

    def test_an_answer_past_the_size_limit_is_page_too_large(self):
        png = png_of(np.zeros((8, 8), dtype=np.uint8), "L")
        steps = d.resolve_recipe({**recipe({"op": "sharpen", "engine": "waifu2x-art-scan", "scale": 4}), "output_scale": 4})
        limit, d.DENOISE_MAX_RESULT_PNG_BYTES = d.DENOISE_MAX_RESULT_PNG_BYTES, 16
        try:
            with self.assertRaises(d.DenoiseError) as caught:
                d.denoise_page(FakeSessions(d.MODELS["waifu2x.swin_unet.art_scan.scale4x"]), steps, png)
        finally:
            d.DENOISE_MAX_RESULT_PNG_BYTES = limit
        self.assertEqual(caught.exception.error_code, "page_too_large")
        # The answer's limit is its own, above the upload's.
        self.assertEqual((d.DENOISE_MAX_PNG_BYTES, d.DENOISE_MAX_RESULT_PNG_BYTES), (16 << 20, 64 << 20))

    def test_alpha_is_enlarged_with_the_page(self):
        rgba = np.zeros((6, 9, 4), dtype=np.uint8)
        rgba[..., 0] = 200
        rgba[..., 3] = 77
        page = d.decode_page(png_of(rgba, "RGBA"))
        image = Image.open(io.BytesIO(d.encode_page(page, page.rgb.repeat(2, axis=1).repeat(2, axis=2))))
        self.assertEqual((image.mode, image.size), ("RGBA", (18, 12)))
        self.assertTrue((np.asarray(image)[..., 3] == 77).all())

    def test_alpha_is_carried_unchanged(self):
        rgba = np.zeros((6, 9, 4), dtype=np.uint8)
        rgba[..., 0] = 200
        rgba[..., 3] = np.arange(54, dtype=np.uint8).reshape(6, 9)
        page = d.decode_page(png_of(rgba, "RGBA"))
        image = Image.open(io.BytesIO(d.encode_page(page, page.rgb)))
        np.testing.assert_array_equal(np.asarray(image)[..., 3], rgba[..., 3])

    def test_indexed_and_bitonal_pages_are_declined(self):
        for mode in ("P", "1"):
            out = io.BytesIO()
            Image.new(mode, (4, 4)).save(out, format="PNG")
            with self.subTest(mode=mode), self.assertRaises(d.DenoiseError) as caught:
                d.decode_page(out.getvalue())
            self.assertEqual(caught.exception.error_code, "unsupported_mode")


def synthetic_pages():
    """Small pages with known colour: grey, grey with a small pink logo, colour art,
    and colour art on a mostly white page."""
    rng = np.random.default_rng(11)
    h, w = 120, 90
    yy, xx = np.mgrid[0:h, 0:w]
    grey = (128 + 70 * np.sin(xx / 5.0) * np.cos(yy / 7.0) + rng.normal(0, 4, (h, w))).clip(0, 255).astype(np.uint8)
    logo = np.stack([grey] * 3, axis=-1).copy()
    logo[4:12, 6:30] = (240, 120, 170)  # 192 of 10800 pixels, under 2 %
    colour = np.stack([(120 + 90 * np.sin(xx / 6.0)), (90 + 60 * np.cos(yy / 5.0)),
                       (160 + 60 * np.sin((xx + yy) / 9.0))], axis=-1).clip(0, 255).astype(np.uint8)
    sparse = np.full((h, w, 3), 250, dtype=np.uint8)
    sparse[30:80, 20:60] = colour[30:80, 20:60]  # a colour panel on a white page, 18 %
    return {"grey": grey, "logo": logo, "colour": colour, "sparse": sparse}


class RouteTests(unittest.TestCase):
    def page(self, name):
        array = synthetic_pages()[name]
        return d.decode_page(png_of(array, "L" if array.ndim == 2 else "RGB"))

    def test_colour_pages_are_told_from_grey_pages_by_the_whole_page(self):
        self.assertFalse(d.is_colour_page(self.page("grey")))
        self.assertFalse(d.is_colour_page(self.page("logo")), "a small logo on a grey page stays on the grey route")
        self.assertTrue(d.is_colour_page(self.page("colour")))
        self.assertTrue(d.is_colour_page(self.page("sparse")), "white paper around colour art is still a colour page")
        # Scan tint: every pixel a little warm, none past the chroma line.
        tinted = np.stack([synthetic_pages()["grey"]] * 3, axis=-1).astype(np.int16)
        tinted[..., 0] += 6
        tinted[..., 2] -= 6
        self.assertFalse(d.is_colour_page(d.decode_page(png_of(tinted.clip(0, 255).astype(np.uint8), "RGB"))))

    def test_one_run_sends_each_page_to_its_route(self):
        routed = {**recipe(GRAIN), "color_steps": [{"op": "sharpen", "engine": "waifu2x-art-scan", "scale": 2}]}
        routes = d.resolve_routes(routed)
        self.assertEqual([s.model_id for s in routes.all()],
                         ["waifu2x.swin_unet.art_scan.noise1", "waifu2x.swin_unet.art_scan.scale2x"])
        sessions = FakeSessions(None)
        ran = {}
        for name, array in synthetic_pages().items():
            png = png_of(array, "L" if array.ndim == 2 else "RGB")
            out, info = d.denoise_page(sessions, routes, png)
            ran[name] = [step["model_id"] for step in info["steps"]]
            self.assertEqual(Image.open(io.BytesIO(out)).size, (90, 120), "every route keeps the page size")
        self.assertEqual(ran["grey"], ["waifu2x.swin_unet.art_scan.noise1"])
        self.assertEqual(ran["logo"], ["waifu2x.swin_unet.art_scan.noise1"])
        self.assertEqual(ran["colour"], ["waifu2x.swin_unet.art_scan.scale2x"])
        self.assertEqual(ran["sparse"], ["waifu2x.swin_unet.art_scan.scale2x"])
        # A plain list of steps is one route for every page, as before.
        _, info = d.denoise_page(sessions, d.resolve_recipe(routed), png_of(synthetic_pages()["colour"], "RGB"))
        self.assertEqual([s["model_id"] for s in info["steps"]], ["waifu2x.swin_unet.art_scan.noise1"])

    def test_output_scale_lands_on_both_routes(self):
        routes = d.resolve_routes({**d.PRESETS["realcugan-2x-auto"], "output_scale": 2})
        self.assertEqual(([s.out for s in routes.gray], [s.out for s in routes.color]), ([2], [2]))
        with self.assertRaises(d.DenoiseError):
            # Real-CUGAN 2x cannot give 4x, whatever the colour route can.
            d.resolve_routes({**d.PRESETS["realcugan-2x-auto"], "output_scale": 4})
        with self.assertRaises(d.DenoiseError):
            d.resolve_routes({**recipe({"op": "sharpen", "engine": "realcugan", "scale": 2}),
                              "color_steps": [GRAIN], "output_scale": 2})


class ChainTests(unittest.TestCase):
    def test_chain_limits(self):
        cugan = {"op": "sharpen", "engine": "realcugan", "scale": 2}
        self.assertEqual(len(d.resolve_recipe(recipe(GRAIN, GRAIN, cugan))), d.MAX_STEPS)
        bad = [
            recipe(GRAIN, GRAIN, GRAIN, cugan),
            {**recipe(GRAIN), "color_steps": []},
            {**recipe(GRAIN), "color_steps": [GRAIN] * 4},
            {**recipe(GRAIN), "color_steps": GRAIN},
            {**recipe(GRAIN), "colour_steps": [GRAIN]},
            recipe({"op": "sharpen", "engine": "ijanai-v3-denoise", "scale": 3}),
            recipe({"op": "sharpen", "engine": "ijanai-v3-denoise", "scale": True}),
            recipe({"op": "sharpen", "engine": "moesr-descreen", "scale": 2}),
            recipe({"op": "sharpen", "engine": "moesr-descreen", "level": 1}),
            recipe({"op": "denoise", "engine": "moesr-descreen", "level": 1}),
        ]
        for raw in bad:
            with self.subTest(raw=raw), self.assertRaises(d.DenoiseError):
                d.resolve_routes(raw)

    def test_scaled_engines_pick_their_model(self):
        step = lambda **kw: d.resolve_step({"op": "sharpen", **kw}).model_id  # noqa: E731
        self.assertEqual(step(engine="ijanai-v3-denoise"), "ijanai.v3denoise.4x")
        self.assertEqual(step(engine="ijanai-v3-denoise", scale=2), "ijanai.v3denoise.2x")
        self.assertEqual(step(engine="moesr-descreen"), "umzi.ds-moesr-v2.4x")
        # A chain the app builds: up to three steps from the list, at twice the page.
        chain = {**recipe(*(d.CHAIN_STEPS[k] for k in ("mangajpeg-hq", "moesr-descreen-4x", "waifu2x-scan-n1"))),
                 "output_scale": 2}
        self.assertEqual([s.out for s in d.resolve_recipe(chain)], [1, 2, 1])


class FakeNet:
    input_channels = 3

    def __init__(self, scale=1):
        self.scale = scale


class FakeNetSessions:
    def __init__(self, scale=1):
        self.scale = scale

    def get(self, model):
        return FakeNet(self.scale)


class ChromaTests(unittest.TestCase):
    """A grey-only model never takes a page's colour (the MangaJaNai bug of 2026-11-14)."""

    def run_model(self, model, rgb, gray, grey_answer=True):
        net = d.Spandrel(FakeNetSessions(model.scale), model)
        seen = []

        def infer(planes):
            seen.append(planes.shape[0])
            out = planes.repeat(model.scale, axis=1).repeat(model.scale, axis=2)
            # A grey-only RGB network answers grey whatever it is given.
            return np.repeat(out.mean(axis=0, keepdims=True), 3, axis=0) if grey_answer else out
        net._infer = infer
        return net.run(rgb, gray), seen

    def test_a_grey_only_model_keeps_the_logo_colour(self):
        logo = synthetic_pages()["logo"]
        page = d.decode_page(png_of(logo, "RGB"))
        for model in (d.MOESR, d.mangajanai_for(2, 1200)):
            with self.subTest(model=model.model_id):
                self.assertTrue(model.gray_only)
                out, seen = self.run_model(replace(model, scale=1), page.rgb, page.gray)
                self.assertEqual(seen, [3], "an RGB network gets the luma on all three planes")
                np.testing.assert_allclose(out, page.rgb, atol=2e-3)
                # At a scale the source chroma is enlarged under the model's luma.
                out, _ = self.run_model(replace(model, scale=2), page.rgb, page.gray)
                self.assertEqual(out.shape, (3, 240, 180))
                pink = (out[:, 10:22, 14:58] * 255).mean(axis=(1, 2))
                self.assertGreater(pink[0] - pink[1], 80, "the logo is still pink")

    def test_an_rgb_model_gets_the_page_as_it_is(self):
        colour = d.decode_page(png_of(synthetic_pages()["colour"], "RGB"))
        model = d.MODELS["ijanai.v3denoise.4x"]
        self.assertFalse(model.gray_only)
        out, seen = self.run_model(replace(model, scale=1), colour.rgb, colour.gray, grey_answer=False)
        np.testing.assert_array_equal(out, colour.rgb)

    def test_with_chroma_of_is_exact_at_the_same_size(self):
        rgb = d.decode_page(png_of(synthetic_pages()["sparse"], "RGB")).rgb
        np.testing.assert_allclose(d.with_chroma_of(rgb, d.luma_of(rgb)), rgb, atol=1e-5)


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.saved = d._extra_arches_installed
        d._extra_arches_installed = False

    def tearDown(self):
        d._extra_arches_installed = self.saved

    def test_extra_arches_install_once_per_process(self):
        class DuplicateArchitectureError(Exception):
            pass

        class Module:
            calls = 0

            def install(self):
                Module.calls += 1
                if Module.calls > 1:
                    raise DuplicateArchitectureError("MoESR")
        module = Module()
        self.assertTrue(d.install_extra_arches(module))
        self.assertFalse(d.install_extra_arches(module))
        self.assertEqual(Module.calls, 1)
        # Registered by someone else first: still the state wanted, and said once.
        d._extra_arches_installed = False
        self.assertTrue(d.install_extra_arches(module))
        self.assertEqual(Module.calls, 2)

    def test_another_install_failure_is_not_swallowed(self):
        class Module:
            def install(self):
                raise ImportError("no torch")
        with self.assertRaises(ImportError):
            d.install_extra_arches(Module())
        self.assertFalse(d._extra_arches_installed)


class WeightsTests(unittest.TestCase):
    def setUp(self):
        self.saved = d._download

    def tearDown(self):
        d._download = self.saved

    def test_a_refused_optional_model_is_skipped_and_a_required_one_fails(self):
        import urllib.error

        def refuse(url, directory):
            raise urllib.error.HTTPError(url, 403, "Forbidden", None, None)
        d._download = refuse
        with tempfile.TemporaryDirectory() as root:
            self.assertEqual(d.ensure_models(root, [d.MOESR]), {})
            with self.assertRaises(OSError):
                d.ensure_models(root, [d.SEAM_FILTER])

    def test_a_tar_member_is_extracted_and_verified(self):
        import hashlib
        import tarfile
        payload = b"moesr weights" * 100
        with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as src:
            archive = Path(src) / "m.tar.xz"
            with tarfile.open(archive, "w:xz") as tar:
                info = tarfile.TarInfo("models/m.pth")
                info.size = len(payload)
                tar.addfile(info, io.BytesIO(payload))

            def serve(url, directory):
                target = Path(directory) / "part"
                target.write_bytes(archive.read_bytes())
                return target
            d._download = serve
            model = replace(d.MOESR, url="https://example.invalid/m.tar.xz", archive_member="models/m.pth",
                            sha256=hashlib.sha256(payload).hexdigest(), bytes=len(payload))
            paths = d.ensure_models(root, [model])
            self.assertEqual(paths[model.model_id].read_bytes(), payload)
            # Already seeded: verified, not fetched again, even with the host refusing now.
            d._download = lambda url, directory: (_ for _ in ()).throw(AssertionError("fetched again"))
            self.assertEqual(d.ensure_models(root, [model]), paths)


class SeedTests(unittest.TestCase):
    def test_capabilities_follow_seeded_files(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root) / d.DENOISE_DIR_NAME
            directory.mkdir()
            self.assertFalse(d.denoise_capabilities(root)["available"])
            for model_id in (d.SEAM_FILTER.model_id, "waifu2x.swin_unet.art_scan.noise1",
                             "waifu2x.swin_unet.art_scan.scale2x"):
                model = d.MODELS[model_id]
                with (directory / model.file_name).open("wb") as handle:
                    handle.truncate(model.bytes)
            caps = d.denoise_capabilities(root)
            self.assertTrue(caps["available"])
            self.assertEqual(caps["features"], ["output_scale", "color_route"])
            self.assertEqual(caps["engines"]["denoise"], ["waifu2x-art-scan"])
            self.assertEqual(caps["engines"]["jpeg"], [])

    def test_unseeded_models_are_not_downloaded_on_a_worker(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(d.DenoiseError) as caught:
                d.ensure_models(root, [d.SEAM_FILTER], download=False)
            self.assertEqual(caught.exception.error_code, "weights_missing")

    def test_settings_carry_the_denoise_flag(self):
        on = ModalSettings.for_installation("mc-test", "mc-test", denoise=True)
        self.assertEqual(on.to_env()["MC_MODAL_DENOISE"], "1")
        self.assertTrue(ModalSettings.from_env(on.to_env()).denoise)
        self.assertFalse(ModalSettings.from_env(ModalSettings.for_installation("mc-test", "mc-test").to_env()).denoise)


class FakeDenoiseWorker:
    def __init__(self) -> None:
        self.calls = 0

    def capabilities(self):
        return {"protocol_version": d.DENOISE_VERSION, "available": True}

    def denoise(self, metadata, page_png):
        self.calls += 1
        return {"protocol_version": d.DENOISE_VERSION, "request_digest": metadata["request_digest"],
                "page_png": page_png, "info": {"steps": []}}


class GatewayTests(unittest.TestCase):
    def setUp(self):
        self.worker = FakeDenoiseWorker()
        self.gateway = CloudGateway("modal", denoise_worker=self.worker, trust_edge_auth=True)
        self.page = png_of(np.full((8, 8), 90, dtype=np.uint8), "L")

    def post(self, gateway, metadata, page=None):
        body = json.dumps({"metadata": metadata,
                           "page_png_b64": base64.b64encode(page or self.page).decode()}).encode()
        status, _, raw = gateway.handle_http_request(
            "POST", "/mc/denoise/v1/page", {"Content-Type": "application/json"}, body)
        return status, json.loads(raw)

    def meta(self, raw_recipe=None):
        raw_recipe = raw_recipe or recipe(GRAIN)
        return {"protocol_version": d.DENOISE_VERSION, "recipe": raw_recipe,
                "request_digest": d.request_digest(raw_recipe, self.page)}

    def test_page_round_trip(self):
        status, body = self.post(self.gateway, self.meta())
        self.assertEqual(status, 200)
        self.assertEqual(base64.b64decode(body["page_png_b64"]), self.page)
        self.assertEqual(self.worker.calls, 1)

    def test_bad_digest_never_reaches_the_gpu(self):
        meta = {**self.meta(), "request_digest": "0" * 64}
        status, body = self.post(self.gateway, meta)
        self.assertEqual((status, body["error_code"]), (400, "invalid_request"))
        self.assertEqual(self.worker.calls, 0)

    def test_unconfigured_deployment_answers_unavailable(self):
        gateway = CloudGateway("modal", trust_edge_auth=True)
        status, body = self.post(gateway, self.meta())
        self.assertEqual((status, body["error_code"]), (503, "capability_unavailable"))
        status, _, _ = gateway.handle_http_request("GET", "/mc/denoise/v1/capabilities", {}, b"")
        self.assertEqual(status, 503)

    def test_a_worker_answer_past_the_size_limit_is_413(self):
        def too_large(metadata, page_png):
            raise d.DenoiseError("the denoised page is past the answer's size limit", "page_too_large")
        self.worker.denoise = too_large
        status, body = self.post(self.gateway, self.meta())
        self.assertEqual((status, body["error_code"]), (413, "page_too_large"))

    def test_an_answer_one_body_cannot_carry_is_413(self):
        # Inside the answer's limit, past what the synchronous route's JSON body holds.
        def large(metadata, page_png):
            return {"protocol_version": d.DENOISE_VERSION, "request_digest": metadata["request_digest"],
                    "page_png": bytes(d.DENOISE_MAX_RESPONSE_BYTES), "info": {"steps": []}}
        self.worker.denoise = large
        status, body = self.post(self.gateway, self.meta())
        self.assertEqual((status, body["error_code"]), (413, "page_too_large"))

    def test_capabilities(self):
        status, _, raw = self.gateway.handle_http_request("GET", "/mc/denoise/v1/capabilities", {}, b"")
        self.assertEqual((status, json.loads(raw)["available"]), (200, True))


if __name__ == "__main__":
    unittest.main()
