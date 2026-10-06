"""The cloud recipe must be the local recipe.

The local engine owns every recipe value: flux.rs holds the prompt and sampling
knobs, sdnq.py holds the working-size rule and the pipeline call, and the sidecar's
backend/base.py holds the hole's reach. These tests read those files as text, so a
change on either side fails here instead of letting cloud and local renders drift
apart.
"""

from __future__ import annotations

import ast
import re
import textwrap
import unittest
from pathlib import Path
from typing import Any, Dict, Tuple

from deploy.cloud.common import contract, flux, manifest

REPO_ROOT = Path(__file__).resolve().parents[3]
FLUX_RS = REPO_ROOT / "crates" / "cleaner-core" / "src" / "engines" / "flux.rs"
RENDER_RS = REPO_ROOT / "crates" / "cleaner-core" / "src" / "engines" / "render.rs"
SDNQ_PY = REPO_ROOT / "sidecar" / "manga_cleaner_sidecar" / "backend" / "sdnq.py"
BASE_PY = REPO_ROOT / "sidecar" / "manga_cleaner_sidecar" / "backend" / "base.py"
FLUX_PY = REPO_ROOT / "deploy" / "cloud" / "common" / "flux.py"


def _rust_const(source: str, name: str, rust_type: str, value_pattern: str) -> str:
    match = re.search(rf"pub const {name}: {re.escape(rust_type)} = {value_pattern};", source)
    if match is None:
        raise AssertionError(f"pub const {name}: {rust_type} not found")
    return match.group(1)


def _python_const(tree: ast.Module, name: str) -> Any:
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == name for t in node.targets):
            return ast.literal_eval(node.value)
    raise AssertionError(f"module constant {name} not found")


def _call_dict(tree: ast.AST, callee: str) -> ast.Dict:
    """The dict literal passed to `callee(...)` (method or function) in the tree."""
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        name = func.attr if isinstance(func, ast.Attribute) else getattr(func, "id", None)
        if name != callee:
            continue
        for arg in node.args:
            if isinstance(arg, ast.Dict):
                return arg
    raise AssertionError(f"call to {callee} with a dict literal not found")


def _update_keywords(tree: ast.AST) -> Dict[str, str]:
    """The keywords of the one `kwargs.update(...)` call: what is passed unfiltered."""
    for node in ast.walk(tree):
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "update"
                and isinstance(node.func.value, ast.Name) and node.func.value.id == "kwargs"):
            return {k.arg: ast.unparse(k.value) for k in node.keywords}
    raise AssertionError("kwargs.update(...) not found")


def _dict_shape(node: ast.Dict, source: str) -> Dict[str, str]:
    shape = {}
    for key, value in zip(node.keys, node.values):
        shape[ast.literal_eval(key)] = ast.get_source_segment(source, value)
    return shape


class RecipeParityTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        for path in (FLUX_RS, RENDER_RS, SDNQ_PY, FLUX_PY, BASE_PY):
            if not path.is_file():
                raise AssertionError(f"recipe source missing: {path}")
        cls.flux_rs = FLUX_RS.read_text(encoding="utf-8")
        cls.render_rs = RENDER_RS.read_text(encoding="utf-8")
        cls.sdnq_source = SDNQ_PY.read_text(encoding="utf-8")
        cls.sdnq_tree = ast.parse(cls.sdnq_source)
        cls.flux_source = FLUX_PY.read_text(encoding="utf-8")
        cls.flux_tree = ast.parse(cls.flux_source)
        cls.base_tree = ast.parse(BASE_PY.read_text(encoding="utf-8"))

    def test_prompt_matches_flux_rs(self) -> None:
        prompt = _rust_const(self.flux_rs, "PROMPT", "&str", r'"((?:[^"\\]|\\.)*)"')
        self.assertEqual(manifest.RECIPE_PROMPT, prompt)

    def test_hole_matches_the_sidecar(self) -> None:
        self.assertEqual(flux.HOLE_GROWTH, _python_const(self.base_tree, "HOLE_GROWTH"))
        local = _update_keywords(self.sdnq_tree)
        cloud = _update_keywords(self.flux_tree)
        self.assertEqual(local, cloud)
        self.assertEqual(sorted(cloud), ["image_reference", "mask_image", "strength"])
        self.assertEqual(cloud["strength"], "1.0")
        self.assertEqual(cloud["image_reference"], "work_crop")

    def test_sampling_matches_flux_rs(self) -> None:
        steps = int(_rust_const(self.flux_rs, "STEPS", "u32", r"(\d+)"))
        seed = int(_rust_const(self.flux_rs, "SEED", "u64", r"(\d+)"))
        guidance = float(_rust_const(self.flux_rs, "GUIDANCE", "f32", r"([0-9]+(?:\.[0-9]+)?)"))
        pinned = manifest.PINNED_SAMPLING[manifest.RECIPE_PROD_SDNQ]
        self.assertEqual(pinned.steps, steps)
        self.assertEqual(pinned.seed, seed)
        # flux.rs wire_sampling(): (GUIDANCE * 100.0).round()
        self.assertEqual(pinned.guidance_scaled, round(guidance * 100))
        self.assertEqual(flux.guidance_from_scaled(pinned.guidance_scaled), guidance)

    def test_interface_values(self) -> None:
        # IC-6 fixes these on the wire; the parse above ties them to the local engine.
        pinned = manifest.PINNED_SAMPLING[manifest.RECIPE_PROD_SDNQ]
        self.assertEqual((pinned.seed, pinned.steps, pinned.guidance_scaled), (1, 4, 100))
        self.assertEqual(manifest.RECIPE_PROD_SDNQ, "mc-flux2-klein-inpaint-v1")
        self.assertEqual(manifest.RECIPE_PROD_SDNQ_9B, "mc-flux2-klein-9b-inpaint-v1")
        recipe = manifest.PINNED_RECIPES[manifest.RECIPE_PROD_SDNQ]
        self.assertEqual(recipe.preprocessing_version, "2.0.0")
        self.assertFalse(recipe.native_mask_conditioning)

    def test_preprocessing_version_matches_render_rs(self) -> None:
        # The client prepares every crop; the gateway only advertises the version.
        rust = _rust_const(self.render_rs, "PREPROCESSING_VERSION", "&str", r'"([^"]*)"')
        self.assertEqual(manifest.PROD_PREPROCESSING_VERSION, rust)
        for recipe_id in (manifest.RECIPE_PROD_SDNQ, manifest.RECIPE_PROD_SDNQ_9B):
            self.assertEqual(manifest.PINNED_RECIPES[recipe_id].preprocessing_version, rust)

    def test_crop_limits_match_render_rs(self) -> None:
        # render.rs sizes every crop's context to fit these; a gateway that
        # advertised less would refuse crops the estimate and consent promised.
        side = int(_rust_const(self.render_rs, "MAX_CROP_SIDE", "u32", r"([0-9_]+)").replace("_", ""))
        pixels = int(_rust_const(self.render_rs, "MAX_CROP_PIXELS", "u64", r"([0-9_]+)").replace("_", ""))
        for limits in (contract.provisional_fixture_limits(), manifest.production_limits()):
            self.assertEqual((limits.max_width, limits.max_height, limits.max_pixels), (side, side, pixels))

    def test_working_size_constants_match_sdnq_py(self) -> None:
        self.assertEqual(manifest.WORK_LONG_SIDE, _python_const(self.sdnq_tree, "WORK_LONG_SIDE"))
        self.assertEqual(contract.LATENT_STRIDE, _python_const(self.sdnq_tree, "LATENT_STRIDE"))
        self.assertEqual(contract.LATENT_STRIDE, int(_rust_const(self.render_rs, "LATENT_STRIDE", "u32", r"(\d+)")))

    def _local_working_size(self):
        namespace: Dict[str, Any] = {
            "WORK_LONG_SIDE": _python_const(self.sdnq_tree, "WORK_LONG_SIDE"),
            "LATENT_STRIDE": _python_const(self.sdnq_tree, "LATENT_STRIDE"),
            "Tuple": Tuple,
        }
        wanted = {"_snap_stride", "_working_size"}
        for node in ast.walk(self.sdnq_tree):
            if isinstance(node, ast.FunctionDef) and node.name in wanted:
                code = textwrap.dedent(ast.get_source_segment(self.sdnq_source, node, padded=True))
                exec(compile(code, str(SDNQ_PY), "exec"), namespace)
                wanted.discard(node.name)
        self.assertEqual(wanted, set(), "sdnq.py working-size helpers not found")
        return namespace["_working_size"]

    def test_working_size_behaves_like_sdnq_py(self) -> None:
        local = self._local_working_size()
        sizes = list(range(1, 64)) + list(range(64, 2049, 16)) + [767, 768, 769, 1000, 1536]
        for width in sizes[::3]:
            for height in sizes[::5]:
                self.assertEqual(flux.working_size(width, height), local(width, height), (width, height))

    def test_pipeline_call_matches_sdnq_py(self) -> None:
        local = _dict_shape(_call_dict(self.sdnq_tree, "_call_kwargs"), self.sdnq_source)
        cloud = _dict_shape(_call_dict(self.flux_tree, "filter_call_kwargs"), self.flux_source)
        self.assertEqual(sorted(local), sorted(cloud))
        # Values that must be literally identical; the rest differ only in where the
        # number comes from (the recipe instead of a request argument).
        for key in ("image", "width", "height", "generator", "output_type", "max_area"):
            self.assertEqual(local[key], cloud[key], key)
        self.assertEqual(cloud["prompt"], "RECIPE_PROMPT")
        self.assertEqual(cloud["num_inference_steps"], "int(steps)")
        self.assertIn("guidance_from_scaled(guidance_scaled)", cloud["guidance_scale"])

    def test_generator_is_seeded_on_the_host(self) -> None:
        needle = 'torch.Generator(device="cpu").manual_seed(int(seed))'
        self.assertIn(needle, self.sdnq_source)
        self.assertIn(needle, self.flux_source)


if __name__ == "__main__":
    unittest.main()
