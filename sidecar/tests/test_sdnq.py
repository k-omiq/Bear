"""Unit tests for the `sdnq` backend, runnable with no model and no torch.

    .sidecar-venv/bin/python -m unittest discover -s sidecar/tests -t sidecar

`unittest` rather than pytest, because the sidecar's own requirements install
neither and a test that needs a fourth dependency is a test nobody runs.

**What is covered without a GPU or 5 GB of weights**: the weights-directory
resolution (three real layouts, including the HuggingFace cache whose files are
all symlinks), the on-disk size that layout would otherwise report as zero, the
working resolution, the guard vocabulary, and the registry. Everything that
needs torch is guarded by :func:`importable` and skips rather than failing, so
this file is green on a machine holding only the MLX backend's dependencies.
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))

from manga_cleaner_sidecar.backend import BACKENDS, get_backend, importable_backends
from manga_cleaner_sidecar.backend.sdnq import (
    CPU_OFFLOAD,
    LATENT_STRIDE,
    MEMORY_FRACTION,
    WORK_LONG_SIDE,
    SdnqBackend,
    find_weights_dir,
    importable,
    resolve_pipeline_dir,
    weights_bytes_of,
)
from manga_cleaner_sidecar.protocol import Basis, ErrorKind, SidecarError

MODEL_INDEX = {
    "_class_name": "Flux2KleinPipeline",
    "transformer": ["diffusers", "Flux2Transformer2DModel"],
}


def _snapshot(path: str, payload: bytes = b"weights") -> str:
    """A minimal diffusers snapshot at ``path``: a model_index and one file."""
    os.makedirs(os.path.join(path, "transformer"), exist_ok=True)
    with open(os.path.join(path, "model_index.json"), "w", encoding="utf-8") as handle:
        json.dump(MODEL_INDEX, handle)
    with open(
        os.path.join(path, "transformer", "diffusion_pytorch_model.safetensors"), "wb"
    ) as handle:
        handle.write(payload)
    return path


def _hf_cache_entry(root: str, repo: str, revision: str = "abc123") -> str:
    """A HuggingFace cache entry, **with symlinks**, as the real one has.

    `models--Org--Repo/snapshots/<sha>/*` are symlinks into `blobs/`, and that is
    exactly the layout `memory.calculate_dir_size_bytes` answers zero for.
    """
    entry = os.path.join(root, "models--" + repo.replace("/", "--"))
    blobs = os.path.join(entry, "blobs")
    snapshot = os.path.join(entry, "snapshots", revision)
    os.makedirs(blobs, exist_ok=True)
    os.makedirs(os.path.join(snapshot, "transformer"), exist_ok=True)

    index_blob = os.path.join(blobs, "index")
    with open(index_blob, "w", encoding="utf-8") as handle:
        json.dump(MODEL_INDEX, handle)
    weights_blob = os.path.join(blobs, "weights")
    with open(weights_blob, "wb") as handle:
        handle.write(b"x" * 4096)

    os.symlink(index_blob, os.path.join(snapshot, "model_index.json"))
    os.symlink(
        weights_blob,
        os.path.join(snapshot, "transformer", "diffusion_pytorch_model.safetensors"),
    )
    return snapshot


class WeightsResolution(unittest.TestCase):
    def setUp(self) -> None:
        self.root = tempfile.mkdtemp(prefix="mc-sdnq-")

    def tearDown(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)

    def test_a_plain_snapshot_is_found_as_itself(self) -> None:
        direct = _snapshot(os.path.join(self.root, "flux2-klein-4b-sdnq"))
        self.assertEqual(resolve_pipeline_dir(direct), direct)

    def test_a_huggingface_cache_entry_resolves_to_its_newest_snapshot(self) -> None:
        older = _hf_cache_entry(self.root, "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic", "old")
        newer = _hf_cache_entry(self.root, "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic", "new")
        os.utime(newer, (2_000_000_000, 2_000_000_000))
        os.utime(older, (1_000_000_000, 1_000_000_000))
        entry = os.path.dirname(os.path.dirname(newer))
        self.assertEqual(resolve_pipeline_dir(entry), newer)

    def test_a_weights_root_holding_one_cache_entry_resolves_through_it(self) -> None:
        snapshot = _hf_cache_entry(self.root, "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic")
        self.assertEqual(resolve_pipeline_dir(self.root), snapshot)

    def test_a_directory_with_no_model_index_anywhere_is_not_a_snapshot(self) -> None:
        os.makedirs(os.path.join(self.root, "empty"))
        self.assertIsNone(resolve_pipeline_dir(os.path.join(self.root, "empty")))
        self.assertIsNone(resolve_pipeline_dir(os.path.join(self.root, "nothing-here")))

    def test_the_model_id_selects_a_directory_by_name_and_by_repository(self) -> None:
        _snapshot(os.path.join(self.root, "flux2-klein-4b-sdnq"))
        _hf_cache_entry(self.root, "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic")
        # The picker's own id, spelled as the directory is.
        found = find_weights_dir("flux2-klein-4b-sdnq", self.root)
        self.assertIsNotNone(found)
        self.assertIn("flux2-klein-4b-sdnq", found or "")
        # And the repository name a cache entry was created from.
        found = find_weights_dir("Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic", self.root)
        self.assertIsNotNone(found)
        self.assertIn("snapshots", found or "")

    def test_the_model_id_matches_hf_cache_entry_with_dots(self) -> None:
        # Multiple entries present so fallback to root does not mask mismatch
        os.makedirs(os.path.join(self.root, "unrelated-dir"))
        snapshot = _hf_cache_entry(self.root, "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic")
        found = find_weights_dir("flux2-klein-4b", self.root)
        self.assertEqual(found, snapshot)

    def test_an_override_naming_the_snapshot_itself_is_honoured(self) -> None:
        direct = _snapshot(os.path.join(self.root, "somewhere-else"))
        self.assertEqual(find_weights_dir("anything-at-all", direct), direct)

    def test_a_lone_folder_of_another_model_is_not_this_model(self) -> None:
        _snapshot(os.path.join(self.root, "flux3-image-klein-sdnq"))
        self.assertIsNone(find_weights_dir("flux2-klein-4b", self.root))
        self.assertIsNotNone(find_weights_dir("flux3-image-klein", self.root))

    def test_a_flux3_snapshot_has_a_modular_index_and_a_download_in_progress_is_skipped(self) -> None:
        flux3 = os.path.join(self.root, "flux3-image-klein-sdnq")
        os.makedirs(flux3)
        with open(os.path.join(flux3, "modular_model_index.json"), "w") as out:
            out.write("{}")
        _snapshot(os.path.join(self.root, ".installing-qwen-image-edit-2511-sdnq"))
        self.assertEqual(find_weights_dir("flux3-image-klein", self.root), flux3)
        self.assertIsNone(find_weights_dir("qwen-image-edit-2511", self.root))

    def test_the_largest_component_is_one_folder_and_not_the_whole_snapshot(self) -> None:
        from manga_cleaner_sidecar.backend.sdnq import largest_component_bytes

        for folder, size in (("transformer", 300), ("text_encoder", 200), ("adapters", 900)):
            os.makedirs(os.path.join(self.root, folder))
            with open(os.path.join(self.root, folder, "weights"), "wb") as out:
                out.write(b"0" * size)
        self.assertEqual(largest_component_bytes(self.root), 300)

    def test_a_snapshot_of_symlinks_reports_its_real_size_once(self) -> None:
        """The reason :func:`weights_bytes_of` exists at all.

        `memory.calculate_dir_size_bytes` skips symlinks - right for a directory
        of real files, and **zero** for the ordinary HuggingFace layout. Weights
        are the floor the hardware gate refuses below, so a zero there is a gate
        that never fires.
        """
        from manga_cleaner_sidecar.memory import calculate_dir_size_bytes

        snapshot = _hf_cache_entry(self.root, "Disty0/FLUX.2-klein-4B-SDNQ-4bit-dynamic")
        self.assertIsNone(calculate_dir_size_bytes(snapshot))
        measured = weights_bytes_of(snapshot)
        self.assertIsNotNone(measured)
        self.assertGreaterEqual(measured or 0, 4096)

        # The blobs are counted once even when a whole cache entry is walked and
        # every blob is reachable twice.
        entry = os.path.dirname(os.path.dirname(snapshot))
        self.assertLess(weights_bytes_of(entry) or 0, 4096 * 2 + 4096)


class EditModels(unittest.TestCase):
    def test_a_model_id_names_its_pipeline(self) -> None:
        from manga_cleaner_sidecar.backend.edit import family_of

        self.assertEqual(family_of("qwen-image-edit-2511"), "qwen")
        self.assertEqual(family_of("flux3-image-klein"), "flux3")
        self.assertEqual(family_of("flux2-klein-4b"), "klein")
        self.assertEqual(family_of("flux2-klein-9b"), "klein")
        self.assertEqual(family_of("somebody-elses-model"), "klein")

    def test_flux3_scales_a_small_crop_up_and_only_snaps_a_large_one(self) -> None:
        from manga_cleaner_sidecar.backend.edit import flux3_work_size

        self.assertEqual(flux3_work_size(336, 416), (608, 768))
        self.assertEqual(flux3_work_size(1024, 800), (1024, 800))


class Contract(unittest.TestCase):
    def test_the_working_resolution_upscales_small_crops_and_never_shrinks_one(self) -> None:
        # A line of dialogue: upscaled to the working long side, snapped down.
        w, h = SdnqBackend._working_size(88, 56)
        self.assertEqual(max(w, h), WORK_LONG_SIDE)
        self.assertEqual(w % LATENT_STRIDE, 0)
        self.assertEqual(h % LATENT_STRIDE, 0)
        # A crop already at or past it is edited at its own size - shrinking it
        # would throw away page the parent decoded on purpose.
        self.assertEqual(SdnqBackend._working_size(1024, 512), (1024, 512))
        self.assertEqual(SdnqBackend._working_size(WORK_LONG_SIDE, 100), (WORK_LONG_SIDE, 100))

    def test_the_guard_vocabulary_is_the_two_the_rust_contract_requires(self) -> None:
        backend = SdnqBackend()
        self.assertEqual(backend.backend_id, "sdnq")
        self.assertEqual(backend.CANONICAL_GUARDS, [CPU_OFFLOAD, MEMORY_FRACTION])
        # The concrete calls a caller might name instead all map onto the two.
        for spelling in (
            "model_cpu_offload",
            "sequential_cpu_offload",
            "torch.mps.set_per_process_memory_fraction",
            "torch.cuda.set_per_process_memory_fraction",
            "PYTORCH_MPS_HIGH_WATERMARK_RATIO",
        ):
            self.assertIn(backend.GUARD_ALIASES[spelling], backend.CANONICAL_GUARDS)
        # Nothing is echoed before an open applied it.
        self.assertEqual(backend.get_applied_guards(), [])
        self.assertEqual(backend.get_basis(), Basis.DECLARED)
        # And the guards belonging to the other backends are refused rather than
        # quietly accepted - a guard this backend cannot apply must not be
        # promised.
        for foreign in ("mx.set_memory_limit", "vae_tiling", "text_encoder_eviction"):
            self.assertIsNone(backend.GUARD_ALIASES.get(foreign))

    def test_sdnq_is_told_not_to_compile_kernels_before_it_can_be_imported(self) -> None:
        self.assertEqual(os.environ.get("SDNQ_USE_TORCH_COMPILE"), "0")

    def test_a_render_before_an_open_is_not_open_rather_than_a_crash(self) -> None:
        backend = SdnqBackend()
        with self.assertRaises(SidecarError) as caught:
            backend.render(None, None, "Remove all text.", 4, 1, 1.0, 1000)  # type: ignore[arg-type]
        self.assertEqual(caught.exception.status_code, 412)
        self.assertEqual(caught.exception.kind, ErrorKind.NOT_OPEN.value)

    def test_an_open_with_no_weights_on_disk_declines_rather_than_faults(self) -> None:
        backend = SdnqBackend(weights_dir="/nonexistent/manga-cleaner-weights")
        with self.assertRaises(SidecarError) as caught:
            backend.open("flux2-klein-4b", {"total_bytes": 1 << 33}, [])
        self.assertEqual(caught.exception.status_code, 503)
        self.assertEqual(caught.exception.kind, ErrorKind.WEIGHTS_MISSING.value)

    def test_a_declared_working_set_is_never_below_the_weights_it_sits_on(self) -> None:
        backend = SdnqBackend()
        working = backend.get_working_set_bytes()
        weights = backend.get_weights_bytes()
        self.assertIsNotNone(working)
        if weights is not None:
            self.assertGreater(working or 0, weights)

    def test_release_is_safe_before_an_open(self) -> None:
        SdnqBackend().release()


class Registry(unittest.TestCase):
    def test_the_registry_knows_three_ids_and_refuses_a_fourth(self) -> None:
        self.assertEqual(set(BACKENDS), {"mflux", "sdnq", "sdcpp"})
        self.assertEqual(get_backend("sdnq").backend_id, "sdnq")
        self.assertEqual(get_backend(" SDNQ ").backend_id, "sdnq")
        with self.assertRaises(SidecarError) as caught:
            get_backend("diffusers")
        self.assertEqual(caught.exception.status_code, 422)

    def test_the_importable_report_names_only_backends_that_could_render(self) -> None:
        """`sdcpp` is never listed: it has no import and accepts no open."""
        reported = importable_backends()
        self.assertNotIn("sdcpp", reported)
        self.assertEqual(("sdnq" in reported), importable())
        for name in reported:
            self.assertIn(name, BACKENDS)


@unittest.skipUnless(importable(), "torch, diffusers and sdnq are not installed here")
class WithTorch(unittest.TestCase):
    """The parts that need the runtime. Skipped where it is absent."""

    def test_the_device_table_prefers_an_accelerator_and_names_its_dtype(self) -> None:
        import torch

        device, dtype = SdnqBackend.pick_device()
        self.assertIn(device, ("cuda", "xpu", "mps", "cpu"))
        expected = {
            "cuda": torch.bfloat16,
            "xpu": torch.bfloat16,
            "mps": torch.float16,
            "cpu": torch.float32,
        }[device]
        self.assertIs(dtype, expected, "Metal has no bfloat16 and the CPU arm is float32")

    def test_the_fraction_setter_exists_on_whatever_device_this_is(self) -> None:
        """The required guard is only honest if the mechanism is really there."""
        import torch

        device, _ = SdnqBackend.pick_device()
        if device == "cpu":
            self.skipTest("no accelerator: this machine is refused at open, by design")
        if device == "mps":
            backend = SdnqBackend()
            applied = backend._apply_memory_fraction("mps", 1024 * 1024 * 1024)
            self.assertEqual(applied, "PYTORCH_MPS_HIGH_WATERMARK_RATIO")
            self.assertIn("PYTORCH_MPS_HIGH_WATERMARK_RATIO", os.environ)
        else:
            namespace = {"cuda": torch.cuda, "xpu": getattr(torch, "xpu", None)}[device]
            self.assertTrue(hasattr(namespace, "set_per_process_memory_fraction"))
        self.assertIsNotNone(SdnqBackend._accelerator_total_bytes(device))


if __name__ == "__main__":
    unittest.main()
