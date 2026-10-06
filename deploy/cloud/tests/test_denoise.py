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
        shipped = {p["id"]: p for p in json.loads(path.read_text())["presets"]}
        self.assertEqual(set(shipped), set(d.PRESETS))
        for name, recipe_doc in d.PRESETS.items():
            with self.subTest(preset=name):
                self.assertEqual(shipped[name]["recipe"], recipe_doc)
                d.resolve_recipe(recipe_doc)
                # Only waifu2x ships as ONNX, so only it can run on the user's computer.
                expected = ["cloud", "local"] if name.startswith("waifu2x") else ["cloud"]
                self.assertEqual(shipped[name]["targets"], expected)

    def test_only_preset_models_are_seeded(self):
        ids = {m.model_id for m in d.preset_models()}
        self.assertIn("waifu2x.swin_unet.art_scan.noise2_scale4x", ids)
        self.assertIn(d.SEAM_FILTER.model_id, ids)
        self.assertNotIn("omdb.1x-MangaJPEGHQ", ids)


class WireTests(unittest.TestCase):
    def setUp(self):
        self.page = png_of(np.full((8, 8), 128, dtype=np.uint8), "L")
        self.meta = {"protocol_version": d.DENOISE_VERSION, "recipe": recipe(GRAIN),
                     "request_digest": d.request_digest(recipe(GRAIN), self.page)}

    def test_valid_request(self):
        self.assertEqual(len(d.validate_denoise_request(self.meta, self.page)), 1)

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

    def test_capabilities(self):
        status, _, raw = self.gateway.handle_http_request("GET", "/mc/denoise/v1/capabilities", {}, b"")
        self.assertEqual((status, json.loads(raw)["available"]), (200, True))


if __name__ == "__main__":
    unittest.main()
