"""Provider apps: settings, what gets uploaded, and the real SDK objects they declare.

The settings and staging tests need no SDK. The app import tests run only where the
modal or beam package is installed; they build the decorators and images offline
(lazy SDK objects, a loopback channel) and never contact a provider.
"""

from __future__ import annotations

import hashlib
import importlib
import json
import os
import sys
import tempfile
import unittest
import warnings
from pathlib import Path
from typing import Dict, Iterator
from contextlib import contextmanager

from deploy.cloud.beam import settings as beam_settings
from deploy.cloud.beam.settings import BeamSettings, resolve_weights_root, secret_name_for
from deploy.cloud.beam.stage import IGNORE_FILE_CONTENTS, IGNORE_FILE_NAME, stage_app
from deploy.cloud.common.contract import JobExecutionStatus, JobRequestMetadata
from deploy.cloud.common.deployment import (
    DEFAULT_IDLE_SECONDS,
    ENV_INSTALLATION_ID,
    JOB_TIMEOUT_SECONDS,
    SEED_TIMEOUT_SECONDS,
    parse_gpu,
    parse_idle_seconds,
)
from deploy.cloud.common.handle_mapping import InMemoryHandleRegistry
from deploy.cloud.common.manifest import (
    RECIPE_TEST_SDNQ,
    REVISION_TEST_FLUX,
    ManifestFileRecord,
    ModelManifest,
    verify_model_manifest,
)
from deploy.cloud.common.redact import REDACTED_PRESIGNED_URL, REDACTED_SECRET, redact_dict, redact_text
from deploy.cloud.modal.settings import ModalSettings

REPO_ROOT = Path(__file__).resolve().parents[3]
FIXTURES_DIR = Path(__file__).resolve().parent.parent / "fixtures"


def _installed(name: str) -> bool:
    try:
        importlib.import_module(name)
    except ImportError:
        return False
    return True


@contextmanager
def patched_environ(values: Dict[str, str]) -> Iterator[None]:
    saved = {key: os.environ.get(key) for key in values}
    os.environ.update(values)
    try:
        yield
    finally:
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


class DeploymentKnobsTest(unittest.TestCase):
    def test_idle_seconds_bounds(self) -> None:
        self.assertEqual([parse_idle_seconds(v) for v in (60, "600", 120.0)], [60, 600, 120])
        for bad in (59, 601, "soon", True, 90.5, None):
            with self.assertRaises(ValueError):
                parse_idle_seconds(bad)

    def test_gpu_allowlists(self) -> None:
        self.assertEqual(parse_gpu(" l40s ", ("L4", "A10", "L40S")), "L40S")
        self.assertEqual(parse_gpu("rtx4090", beam_settings.GPU_ALLOWLIST), "RTX4090")
        for bad in ("H100", "", None):
            with self.assertRaises(ValueError):
                parse_gpu(bad, ("L4", "A10", "L40S"))

    def test_job_and_seed_timeouts(self) -> None:
        self.assertEqual((JOB_TIMEOUT_SECONDS, SEED_TIMEOUT_SECONDS), (600, 3600))


class ModalSettingsTest(unittest.TestCase):
    def test_selected_analysis_models_round_trip_without_gpu_start(self) -> None:
        settings = ModalSettings.for_installation("mc-a", "mc-a", analysis_models=("text_regions_rt@1", "text_mask_sam_ts@1"))
        self.assertEqual(settings.analysis_models, ("text_mask_sam_ts@1", "text_regions_rt@1"))
        self.assertEqual(ModalSettings.from_env(settings.to_env()), settings)

    def test_names_and_env_round_trip(self) -> None:
        settings = ModalSettings.for_installation("mc-ab12cd", "mc-ab12cd", gpu="a10", idle_seconds=300)
        self.assertEqual(
            (settings.app_name, settings.volume_name, settings.dict_name, settings.gpu),
            ("mc-ab12cd", "mc-ab12cd-weights", "mc-ab12cd-jobs", "A10"),
        )
        self.assertIsNone(settings.environment)
        self.assertEqual(ModalSettings.from_env(settings.to_env()), settings)

    def test_defaults(self) -> None:
        settings = ModalSettings.from_env({ENV_INSTALLATION_ID: "mc-x1"})
        self.assertEqual((settings.gpu, settings.idle_seconds, settings.app_name), ("L4", DEFAULT_IDLE_SECONDS, "mc-x1"))

    def test_bad_values_fail_the_import(self) -> None:
        with self.assertRaises(ValueError):
            ModalSettings.from_env({"MC_MODAL_GPU": "H100"})
        with self.assertRaises(ValueError):
            ModalSettings.from_env({"MC_MODAL_IDLE_SECONDS": "5"})
        with self.assertRaises(ValueError):
            ModalSettings.from_env({"MC_MODAL_ROUTING_REGION": "mars"})

    def test_routing_region_names_the_gateway_function(self) -> None:
        # The default keeps the name every installation was deployed with.
        default = ModalSettings.for_installation("mc-ab12cd", "mc-ab12cd")
        self.assertEqual((default.routing_region, default.gateway_function), ("us-east", "gateway"))
        self.assertEqual(ModalSettings.from_env({ENV_INSTALLATION_ID: "mc-x1"}).routing_region, "us-east")
        # Any other region is another Function: Modal refuses to change one in place.
        south = ModalSettings.for_installation("mc-ab12cd", "mc-ab12cd", routing_region="ap-south")
        self.assertEqual(south.gateway_function, "gateway_ap_south")
        self.assertEqual(south.to_env()["MC_MODAL_ROUTING_REGION"], "ap-south")
        self.assertEqual(ModalSettings.from_env(south.to_env()), south)


class BeamSettingsTest(unittest.TestCase):
    def test_selected_analysis_worker_has_its_own_deployment_and_url(self) -> None:
        settings = BeamSettings.for_installation("mc-a", "mc-a", analysis_models=("text_regions_rt@1",))
        self.assertEqual(settings.deployment_names["analysis"], "mc-a-analysis")
        linked = settings.with_analysis_url("https://example.invalid/analysis")
        self.assertEqual(BeamSettings.from_env(linked.to_env()), linked)

    def test_names_and_env_round_trip(self) -> None:
        settings = BeamSettings.for_installation("mc-ab12cd", "mc-ab12cd", gpu="a10g", idle_seconds=90)
        self.assertEqual(
            settings.deployment_names,
            {"seed": "mc-ab12cd-seed", "worker": "mc-ab12cd-worker", "gateway": "mc-ab12cd-gateway"},
        )
        self.assertEqual((settings.volume_name, settings.map_name), ("mc-ab12cd-weights", "mc-ab12cd-jobs"))
        self.assertEqual((settings.secret_name, settings.gpu), ("MC_MC_AB12CD_TOKEN", "A10G"))
        env = settings.to_env()
        self.assertNotIn(beam_settings.ENV_WORKER_URL, env, "Beam rejects empty values")
        self.assertEqual(BeamSettings.from_env(env), settings)
        with_url = settings.with_worker_url("https://mc-ab12cd-worker-abc.app.beam.cloud")
        self.assertEqual(BeamSettings.from_env(with_url.to_env()), with_url)

    def test_values_beam_cannot_carry_are_refused(self) -> None:
        settings = BeamSettings.for_installation("mc-ab12cd", "mc-ab12cd")
        with self.assertRaises(ValueError):
            settings.with_worker_url("https://x?a=b").to_env()
        with self.assertRaises(ValueError):
            settings.with_worker_url("has space").to_env()

    def test_secret_names_are_env_names(self) -> None:
        self.assertEqual(secret_name_for("mc-my-books-1a2b3c4d"), "MC_MC_MY_BOOKS_1A2B3C4D_TOKEN")
        self.assertRegex(secret_name_for("mc--x.y"), r"^[A-Z_][A-Z0-9_]*$")

    def test_weights_root_prefers_the_mount(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            self.assertEqual(resolve_weights_root("vol", cwd=base), base / "weights")
            (base / "weights").mkdir()
            self.assertEqual(resolve_weights_root("vol", cwd=base), base / "weights")


class BeamStagingTest(unittest.TestCase):
    def test_stage_holds_only_what_the_containers_import(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "stage"
            module = stage_app(target)
            self.assertEqual(module, target / "mc_beam_app.py")
            self.assertEqual(module.read_bytes(), (REPO_ROOT / "deploy" / "cloud" / "beam" / "app.py").read_bytes())
            files = sorted(str(p.relative_to(target)) for p in target.rglob("*") if p.is_file())
            common = sorted(
                f"deploy/cloud/common/{p.name}" for p in (REPO_ROOT / "deploy" / "cloud" / "common").glob("*.py")
            )
            expected = sorted(
                [
                    IGNORE_FILE_NAME,
                    "mc_beam_app.py",
                    "deploy/__init__.py",
                    "deploy/cloud/__init__.py",
                    "deploy/cloud/beam/__init__.py",
                    "deploy/cloud/beam/backend.py",
                    "deploy/cloud/beam/routes.py",
                    "deploy/cloud/beam/settings.py",
                    "deploy/cloud/beam/analysis_backend.py",
                    "analysis-source/bootstrap.py",
                    "analysis-source/export_mask.py",
                    "analysis-source/synthetic_page.png",
                ]
                + common
            )
            self.assertEqual(files, expected)
            self.assertEqual((target / IGNORE_FILE_NAME).read_text(encoding="utf-8"), IGNORE_FILE_CONTENTS)
            for path in files:
                self.assertNotIn("tests", path)
                self.assertNotIn("modal", path)


class SharedUtilitiesTest(unittest.TestCase):
    def test_presigned_urls_are_redacted(self) -> None:
        gcs = (
            "https://storage.googleapis.com/bucket/out.png?X-Goog-Algorithm=GOOG4-RSA-SHA256&"
            "X-Goog-Credential=sa%40proj.iam.gserviceaccount.com&X-Goog-Signature=abcdef1234567890secretsig"
        )
        s3 = "https://storage.beam.cloud/v2/outputs/crop.png?X-Amz-Signature=secret999&X-Amz-Credential=AKIAEXAMPLE"
        legacy = "https://storage.googleapis.com/b/o.png?GoogleAccessId=a@p.iam.gserviceaccount.com&Signature=legacy123"
        for url, secret in ((gcs, "secretsig"), (s3, "secret999"), (legacy, "legacy123")):
            redacted = redact_text(f"Fetched {url} ok")
            self.assertNotIn(secret, redacted)
            self.assertIn(REDACTED_PRESIGNED_URL, redacted)
        nested = redact_dict({"gcs_signed_url": gcs, "nested": {"note": f"link {gcs}"}})
        self.assertEqual(nested["gcs_signed_url"], REDACTED_SECRET)
        self.assertNotIn("secretsig", json.dumps(nested))

    def test_manifest_verification(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            model = b"weights-bytes"
            (root / "model.safetensors").write_bytes(model)
            files = [
                ManifestFileRecord("model.safetensors", hashlib.sha256(model).hexdigest(), len(model)),
                ManifestFileRecord("config.json", hashlib.sha256(b"X" * 100).hexdigest(), 100),
            ]
            manifest = ModelManifest("test-model", REVISION_TEST_FLUX, RECIPE_TEST_SDNQ, "1.0.0", files, len(model) + 100)
            report = verify_model_manifest(root, manifest)
            self.assertFalse(report["valid"])
            self.assertEqual((report["missing_files"], report["verified_files"]), (["config.json"], ["model.safetensors"]))
            (root / "config.json").write_bytes(b"Y" * 100)
            self.assertEqual(verify_model_manifest(root, manifest)["corrupted_files"], ["config.json"])
            (root / "config.json").write_bytes(b"X" * 100)
            self.assertTrue(verify_model_manifest(root, manifest)["valid"])

    def test_handle_registry_records(self) -> None:
        meta = JobRequestMetadata.from_dict(json.loads((FIXTURES_DIR / "job_request_metadata.json").read_text()))
        image = (FIXTURES_DIR / "tiny_image.png").read_bytes()
        registry = InMemoryHandleRegistry()
        record = registry.register_job(job_meta=meta, native_handle="native-1", app_handle="handle-1", provider="modal")
        self.assertIs(registry.get_by_native_handle("native-1"), record)
        self.assertIs(registry.get_by_job_attempt(meta.job_id, meta.attempt_id), record)
        registry.update_status("handle-1", JobExecutionStatus.RUNNING)
        self.assertIsNotNone(record.started_at)
        registry.store_result(app_handle="handle-1", result_data=image, result_digest=meta.image_sha256)
        self.assertEqual((record.status, record.result_bytes), (JobExecutionStatus.COMPLETED, len(image)))


@unittest.skipUnless(_installed("modal"), "modal is not installed")
class ModalAppImportTest(unittest.TestCase):
    """deploy.cloud.modal.app declares the app offline; nothing here talks to Modal."""

    @classmethod
    def setUpClass(cls) -> None:
        cls._tmp = tempfile.TemporaryDirectory()
        settings = ModalSettings.for_installation("mc-ab12cd", "mc-ab12cd", gpu="l40s", idle_seconds=240)
        env = {
            **settings.to_env(),
            "MODAL_CONFIG_PATH": str(Path(cls._tmp.name) / "absent.toml"),
            "MODAL_SERVER_URL": "http://127.0.0.1:9",
        }
        sys.modules.pop("deploy.cloud.modal.app", None)
        with patched_environ(env), warnings.catch_warnings():
            warnings.simplefilter("error")
            cls.module = importlib.import_module("deploy.cloud.modal.app")
        cls.settings = settings

    @classmethod
    def tearDownClass(cls) -> None:
        sys.modules.pop("deploy.cloud.modal.app", None)
        cls._tmp.cleanup()

    def test_app_layout(self) -> None:
        app = self.module.app
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            functions = sorted(app.registered_functions)
            classes = sorted(app.registered_classes)
        self.assertEqual(app.name, "mc-ab12cd")
        self.assertEqual(functions, ["AnalysisGPU.*", "Worker.*", "gateway", "seed_weights"])
        self.assertEqual(classes, ["AnalysisGPU", "Worker"])
        self.assertEqual(self.module.SETTINGS, self.settings)

    def test_selected_analysis_capability_is_baked_into_modal_app(self) -> None:
        settings = ModalSettings.for_installation("mc-rt", "mc-rt", analysis_models=("text_regions_rt@1", "text_mask_sam_ts@1"))
        env = {**settings.to_env(), "MODAL_CONFIG_PATH": str(Path(self._tmp.name) / "absent.toml"),
               "MODAL_SERVER_URL": "http://127.0.0.1:9"}
        sys.modules.pop("deploy.cloud.modal.app", None)
        with patched_environ(env), warnings.catch_warnings():
            warnings.simplefilter("error")
            selected = importlib.import_module("deploy.cloud.modal.app")
        self.assertEqual(selected.SETTINGS.analysis_models, ("text_mask_sam_ts@1", "text_regions_rt@1"))
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            self.assertIn("AnalysisGPU", selected.app.registered_classes)
        self.assertEqual(selected.AnalysisGPU.__name__, "AnalysisGPU")

    def test_only_the_deploy_sources_are_uploaded(self) -> None:
        shipped = self.module._not_shipped
        for kept in ("__init__.py", "cloud/__init__.py", "cloud/common/api.py", "cloud/modal/backend.py"):
            self.assertFalse(shipped(Path(kept)), kept)
        for dropped in (
            "cloud/tests/test_gateway.py",
            "cloud/fixtures/tiny_image.png",
            "cloud/beam/app.py",
            "cloud/common/__pycache__/api.cpython-312.pyc",
            "cloud/modal/notes.md",
        ):
            self.assertTrue(shipped(Path(dropped)), dropped)


@unittest.skipUnless(_installed("beam"), "beam-client is not installed")
class BeamAppImportTest(unittest.TestCase):
    """The staged Beam module builds its decorators offline against a loopback channel."""

    @classmethod
    def setUpClass(cls) -> None:
        from beta9.abstractions.base import set_channel
        from beta9.channel import Channel
        from beta9.config import ConfigContext

        cls._tmp = tempfile.TemporaryDirectory()
        settings = BeamSettings.for_installation("mc-ab12cd", "mc-ab12cd", gpu="rtx5090", idle_seconds=180)
        env = {**settings.to_env(), "CONFIG_PATH": str(Path(cls._tmp.name) / "absent.ini")}
        stage = Path(cls._tmp.name) / "stage"
        stage_app(stage)
        previous_cwd = os.getcwd()
        with patched_environ(env), warnings.catch_warnings():
            warnings.simplefilter("error")
            # Same channel shape the provisioner builds: no reconnect subscription, and a
            # config so the decorators never look for a config file. Nothing calls out.
            channel = Channel(addr="127.0.0.1:9", token="offline-test", retry=(lambda state: None, False))
            channel.config = ConfigContext(token="offline-test", gateway_host="127.0.0.1", gateway_port=9)
            set_channel(channel=channel)
            os.chdir(stage)
            # os.getcwd() is the resolved path, which is what Beam maps handlers against.
            sys.path.insert(0, os.getcwd())
            try:
                cls.module = importlib.import_module("mc_beam_app")
            finally:
                sys.path.pop(0)
                os.chdir(previous_cwd)
        cls.settings = settings

    @classmethod
    def tearDownClass(cls) -> None:
        from beta9.abstractions.base import unset_channel

        unset_channel()
        sys.modules.pop("mc_beam_app", None)
        cls._tmp.cleanup()

    def test_handlers(self) -> None:
        seed, render, analyze, gateway = (getattr(self.module, name).parent for name in ("seed", "render", "analyze", "gateway"))
        self.assertEqual((seed.name, render.name, gateway.name), ("mc-ab12cd-seed", "mc-ab12cd-worker", "mc-ab12cd-gateway"))
        self.assertEqual(analyze.name, "mc-ab12cd-analysis")
        self.assertEqual(render.gpu, "RTX5090")
        self.assertEqual(analyze.gpu, "RTX5090")
        self.assertFalse(seed.gpu)
        self.assertFalse(gateway.gpu)
        self.assertEqual((render.keep_warm_seconds, render.task_policy.max_retries), (180, 0))
        self.assertEqual((render.task_policy.timeout, seed.task_policy.timeout), (600, 3600))
        self.assertEqual((analyze.task_policy.timeout, analyze.task_policy.max_retries), (180, 0))
        self.assertEqual(analyze.on_start, "mc_beam_app:load_analysis_worker")
        self.assertTrue(all(h.authorized for h in (seed, render, analyze, gateway)))
        self.assertEqual(render.on_start, "mc_beam_app:load_worker")
        self.assertEqual([s.name for s in gateway.secrets], ["MC_MC_AB12CD_TOKEN"])
        for handler in (seed, render, analyze, gateway):
            self.assertEqual([(v.name, v.mount_path) for v in handler.volumes], [("mc-ab12cd-weights", "./weights")])
        self.assertIn("MC_BEAM_GPU=RTX5090", gateway.env)
        # FLUX.2 Klein 4B on an RTX5090: two worker processes, each with its own
        # pipeline, and a whole worker's memory for each (common/capacity.py).
        self.assertEqual((render.workers, analyze.workers, seed.workers, gateway.workers), (2, 2, 1, 1))
        self.assertEqual(self.module.RENDER_WORKERS, 2)

    def test_gpu_image_pins(self) -> None:
        steps = [(step.type, step.command) for step in self.module.gpu_image.build_steps]
        self.assertIn(("shell", self.module.GPU_TORCH_INSTALL), steps)
        self.assertIn("torch==2.13.0 torchvision==0.28.0", self.module.GPU_TORCH_INSTALL)
        self.assertIn("https://download.pytorch.org/whl/cu129", self.module.GPU_TORCH_INSTALL)
        for requirement in self.module.GPU_REQUIREMENTS:
            self.assertIn(("pip", requirement), steps)


if __name__ == "__main__":
    unittest.main()
