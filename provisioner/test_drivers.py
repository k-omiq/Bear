"""The provisioner end to end against fake Modal and Beam SDKs.

The real controller, journal and drivers run; only the SDK modules, the provider
edge (HTTP) and the clock are fakes (see fake_sdks.py). Each case runs for both
providers. `Killed` stands in for the desktop killing the helper mid-run.
"""

import io
import json
import pickle
import re
import shutil
import tempfile
import threading
import time
import unittest
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple
from unittest import mock

from provisioner.beam_driver import BeamDriver, load_plain
from provisioner.controller import ProvisioningController
from provisioner.driver_base import SEED_INTENT_KEY, SEED_STALE_SECONDS, SEED_START_GRACE_SECONDS, Deadline, read_seed_state
from provisioner import driver_base
from provisioner.endpoint import check_endpoint
from provisioner.fake_sdks import (
    FakeBeamCloud,
    FakeModalCloud,
    Killed,
    beam_app_loader,
    modal_app_loader,
)
from provisioner.journal import InstallationJournal
from provisioner.modal_driver import TOKEN_CREATE_INTENT_KEY, ModalDriver
from provisioner.progress import STATES, STEPS
from provisioner.protocol import HELPER_PROTOCOL_VERSION, ProtocolError

from deploy.cloud.common.contract import PROTOCOL_VERSION, HealthResponse
from deploy.cloud.common.manifest import (
    MODEL_PROD_FLUX,
    MODEL_PROD_FLUX_9B,
    MODEL_PROD_QWEN,
    PROD_SNAPSHOT_9B_TOTAL_BYTES,
    PROD_SNAPSHOT_QWEN_TOTAL_BYTES,
    PROD_SNAPSHOT_TOTAL_BYTES,
    get_production_model_info,
)
from deploy.cloud.common.weights import seed_state


class SeedStateModelTest(unittest.TestCase):
    def test_same_size_adapter_replacement_and_legacy_seed_status_are_unknown(self):
        from dataclasses import replace
        from unittest import mock
        from deploy.cloud.common.manifest import LIGHTNING_FILE, PRODUCTION_MODELS
        model = PRODUCTION_MODELS[MODEL_PROD_QWEN]
        old_filename = LIGHTNING_FILE.replace("8steps", "4steps")
        old_model = replace(model, adapters=tuple(
            replace(a, filename=old_filename) if a.filename == LIGHTNING_FILE else a
            for a in model.adapters
        ))
        with mock.patch.dict(PRODUCTION_MODELS, {MODEL_PROD_QWEN: old_model}):
            old_done = seed_state("done", model.total_bytes, model_id=MODEL_PROD_QWEN)
        self.assertEqual(read_seed_state(old_done, MODEL_PROD_QWEN)[0], "unknown")
        old_done.pop("snapshot_fingerprint")
        self.assertEqual(read_seed_state(old_done, MODEL_PROD_QWEN)[0], "unknown")

    def test_9b_seed_status_uses_its_own_revision_and_size(self):
        running = seed_state("running", PROD_SNAPSHOT_9B_TOTAL_BYTES // 2,
                             now=5.0, model_id=MODEL_PROD_FLUX_9B)
        self.assertEqual(read_seed_state(running, MODEL_PROD_FLUX_9B), ("running", 50, ""))
        self.assertEqual(read_seed_state(running)[0], "unknown")
        done = seed_state("done", PROD_SNAPSHOT_9B_TOTAL_BYTES,
                          now=6.0, model_id=MODEL_PROD_FLUX_9B)
        self.assertEqual(read_seed_state(done, MODEL_PROD_FLUX_9B), ("done", 100, ""))

    def test_a_done_seed_of_an_older_file_set_is_not_done(self):
        # Qwen recipe v2 added FukidashiErase to the same base revision. A seed that
        # finished before it covers fewer bytes and must not skip the new download.
        from deploy.cloud.common.manifest import FUKIDASHI_BYTES, MODEL_PROD_QWEN, PROD_SNAPSHOT_QWEN_TOTAL_BYTES
        done = seed_state("done", PROD_SNAPSHOT_QWEN_TOTAL_BYTES, now=6.0, model_id=MODEL_PROD_QWEN)
        self.assertEqual(read_seed_state(done, MODEL_PROD_QWEN), ("done", 100, ""))
        older = dict(done, bytes_total=PROD_SNAPSHOT_QWEN_TOTAL_BYTES - FUKIDASHI_BYTES)
        self.assertEqual(read_seed_state(older, MODEL_PROD_QWEN)[0], "unknown")


class EndpointModelCheckTest(unittest.TestCase):
    """/model-info must name the model the installation chose; a probe accepts any production one."""

    URL = "https://ws--mc-ab12cd-gateway.modal.run/mc/v1"
    CREDENTIAL = {"kind": "modal_proxy", "token_id": "wk-test", "token_secret": "ws-test"}

    def check(self, served: str, model_id: Optional[str]) -> Dict[str, Any]:
        def get(url: str, headers: Dict[str, str], timeout: float) -> Tuple[Optional[int], bytes]:
            if url.endswith("/health"):
                body = HealthResponse(status="ok", provider="modal", protocol_version=PROTOCOL_VERSION).to_dict()
            else:
                body = get_production_model_info("modal", served).to_dict()
            return 200, json.dumps(body).encode("utf-8")

        return check_endpoint(self.URL, self.CREDENTIAL, provider="modal", deadline=Deadline(30), http=get, model_id=model_id)

    def test_the_chosen_model_passes(self) -> None:
        for model in (MODEL_PROD_FLUX, MODEL_PROD_FLUX_9B):
            self.assertEqual(self.check(model, model)["model"]["model_id"], model)

    def test_another_model_than_the_chosen_one_fails(self) -> None:
        with self.assertRaises(ProtocolError) as caught:
            self.check(MODEL_PROD_FLUX_9B, MODEL_PROD_FLUX)
        self.assertIn("different model", caught.exception.message)

    def test_a_probe_accepts_any_production_model(self) -> None:
        for model in (MODEL_PROD_FLUX, MODEL_PROD_FLUX_9B):
            self.assertTrue(self.check(model, None)["ok"])


IC1_KEYS = {
    "denoise",
    "installation_id",
    "provider",
    "stage",
    "endpoint_url",
    "runtime_credential",
    "gpu",
    "idle_seconds",
    "model",
    "analysis_models",
    "compatibility_status",
    "resources_created",
    "setup_credential_forgotten",
}
IC2_LINE = re.compile(
    r'^\{"mc_progress":1,"op":"(?P<op>[a-z_]+)","step":"(?P<step>[a-z]+)","state":"(?P<state>[a-z]+)","pct":(?P<pct>null|\d{1,3})\}$'
)
PIPELINE = ["volume", "state", "secret", "image", "deploy", "weights", "token", "endpoint", "health"]


_TRIPPED: List[str] = []


def _trip() -> None:
    _TRIPPED.append("unpickled")


class _Tripwire:
    def __reduce__(self):
        return (_trip, ())


class FakeClock:
    def __init__(self) -> None:
        self.now = 1000.0

    def __call__(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds


def _unavailable() -> Any:
    raise ImportError("not in this test")


class Harness:
    """One desktop: a journal directory, one fake provider account, fresh helper runs."""

    def __init__(self, provider: str) -> None:
        self.provider = provider
        self.clock = FakeClock()
        self.root = Path(tempfile.mkdtemp(prefix="mc-journal-test-"))
        if provider == "modal":
            self.cloud: Any = FakeModalCloud()
            self.credentials = {"token_id": "ak-good-token-id", "token_secret": "as-good-token-secret"}
        else:
            self.cloud = FakeBeamCloud()
            self.credentials = {"token": "b9-good-api-key"}
        self.sdk_loader = self.cloud.sdk
        self.progress_lines: List[str] = []
        self.sign_in_seconds = 40.0

    def driver(self) -> Any:
        if self.provider == "modal":
            return ModalDriver(
                sdk_loader=self.sdk_loader,
                app_loader=modal_app_loader(self.cloud),
                http=self.cloud.http(),
                sleep=self.clock.sleep,
                clock=self.clock,
                sign_in_seconds=self.sign_in_seconds,
                wall_clock=self.clock,
            )
        return BeamDriver(
            sdk_loader=self.sdk_loader,
            app_loader=beam_app_loader(self.cloud),
            http=self.cloud.http(),
            post=self.cloud.post,
            sleep=self.clock.sleep,
            clock=self.clock,
            wall_clock=self.clock,
        )

    def request(self, op: str, params: Dict[str, Any]) -> Tuple[Dict[str, Any], List[str]]:
        """One helper process: a new controller, one request, the response and its IC-2 lines."""
        stream = io.StringIO()
        other = ModalDriver(sdk_loader=_unavailable) if self.provider == "beam" else BeamDriver(sdk_loader=_unavailable)
        drivers = {"modal_driver": self.driver(), "beam_driver": other}
        if self.provider == "beam":
            drivers = {"modal_driver": other, "beam_driver": self.driver()}
        controller = ProvisioningController(self.root, progress_stream=stream, clock=self.clock, **drivers)
        envelope = {
            "protocol_version": HELPER_PROTOCOL_VERSION,
            "request_id": f"req-{op}",
            "op": op,
            "provider": self.provider,
            "params": params,
        }
        try:
            raw = controller.handle_request(json.dumps(envelope))
        finally:
            self.progress_lines = stream.getvalue().splitlines()
        self.raw = raw
        return json.loads(raw), self.progress_lines

    def plan(self, installation_id: str = "mc-ab12cd", options: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
        response, _ = self.request(
            "plan", {"credentials": self.credentials, "installation_id": installation_id, "options": options or {}}
        )
        assert response["success"], response
        return response["data"]

    def apply(self, installation_id: str = "mc-ab12cd", options: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
        plan = self.plan(installation_id, options)
        response, _ = self.request(
            "apply",
            {
                "credentials": self.credentials,
                "installation_id": installation_id,
                "approved_plan_hash": plan["plan_hash"],
                "options": options or {},
                "forget_setup_credential": True,
            },
        )
        return response

    def resume(self, installation_id: str = "mc-ab12cd", credentials: Optional[Dict[str, Any]] = None, options=None,
               approved_plan_hash: Optional[str] = None):
        params = {
            "credentials": credentials or self.credentials,
            "installation_id": installation_id,
            "options": options or {},
            "forget_setup_credential": True,
        }
        if approved_plan_hash is not None:
            params["approved_plan_hash"] = approved_plan_hash
        response, _ = self.request("resume", params)
        return response

    def journal(self, installation_id: str = "mc-ab12cd") -> Any:
        return InstallationJournal(self.root, installation_id, self.provider).load()

    def journal_text(self, installation_id: str = "mc-ab12cd") -> str:
        return (self.root / "installations" / installation_id / "journal.json").read_text(encoding="utf-8")

    def live_runtime_tokens(self) -> List[str]:
        return list(self.cloud.proxy_tokens) if self.provider == "modal" else []

    def close(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)


def parse_progress(lines: List[str], op: str) -> List[Tuple[str, str, Optional[int]]]:
    """Every line must be exactly one IC-2 record; returns (step, state, pct)."""
    records = []
    for line in lines:
        match = IC2_LINE.match(line)
        assert match, f"not an IC-2 line: {line!r}"
        record = json.loads(line)
        assert list(record) == ["mc_progress", "op", "step", "state", "pct"], line
        assert record["op"] == op and record["step"] in STEPS and record["state"] in STATES, line
        assert record["pct"] is None or 0 <= record["pct"] <= 100, line
        records.append((record["step"], record["state"], record["pct"]))
    return records


def final_states(records: List[Tuple[str, str, Optional[int]]]) -> Dict[str, str]:
    states: Dict[str, str] = {}
    for step, state, _ in records:
        states[step] = state
    return states


class ProviderCases:
    provider = ""
    expected_resource_types: List[str] = []
    provider_skips: List[str] = []

    seed_start_call = ""
    volume_delete_call = ""

    def setUp(self) -> None:
        self.h = Harness(self.provider)
        self.addCleanup(self.h.close)

    def fresh_harness(self) -> None:
        self.h = Harness(self.provider)
        self.addCleanup(self.h.close)

    def cleanup_apply(self) -> Dict[str, Any]:
        params = {
            "credentials": self.h.credentials,
            "installation_id": "mc-ab12cd",
            "approved_cleanup_plan_hash": self.cleanup_plan()["plan_hash"],
            "confirm_delete_persistent_storage": True,
        }
        response, _ = self.h.request("cleanup_apply", params)
        return response

    # ---------- plan ----------

    def test_plan_is_read_only_and_describes_real_resources(self) -> None:
        plan = self.h.plan()
        allocation = plan["resource_allocation"]
        self.assertEqual(allocation["gpu"], self.default_gpu)
        self.assertEqual(allocation["gpu_options"], list(self.gpu_options))
        self.assertTrue(all(re.match(r"^[A-Za-z0-9_-]{1,32}$", g) for g in allocation["gpu_options"]))
        self.assertEqual((allocation["idle_seconds"], allocation["model_weights_bytes"]), (120, PROD_SNAPSHOT_TOTAL_BYTES))
        self.assertEqual([r["type"] for r in plan["resources_to_create"]], self.expected_resource_types)
        self.assertTrue(all(r["name"].startswith(("mc-ab12cd", "MC_MC_AB12CD")) for r in plan["resources_to_create"]))
        self.assertRegex(plan["plan_hash"], r"^[0-9a-f]{64}$")
        self.assertIn("List prices on 2026-09-23", plan["estimated_monthly_cost"])
        self.assertFalse((self.h.root / "installations").exists(), "plan writes nothing locally")
        self.assertEqual(self.mutating_calls(), [], "plan creates nothing in the cloud")
        self.assertEqual(final_states(parse_progress(self.h.progress_lines, "plan")), {"inspect": "done", "validate": "done"})

    def test_plan_honours_and_checks_options(self) -> None:
        plan = self.h.plan(options={"gpu": self.gpu_options[1], "idle_seconds": 300})
        self.assertEqual((plan["resource_allocation"]["gpu"], plan["resource_allocation"]["idle_seconds"]), (self.gpu_options[1], 300))
        self.assertNotEqual(plan["plan_hash"], self.h.plan()["plan_hash"])
        for bad in ({"gpu": "H100"}, {"idle_seconds": 30}, {"idle_seconds": 601}, {"idle_seconds": "120"}, {"idle_seconds": True}, {"region": "eu"}):
            response, _ = self.h.request(
                "plan", {"credentials": self.h.credentials, "installation_id": "mc-ab12cd", "options": bad}
            )
            self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR", bad)

    def test_9b_plan_pins_larger_gpu_and_separate_checkpoint(self) -> None:
        required_gpu = "L40S" if self.provider == "modal" else "RTX5090"
        plan = self.h.plan(options={"model_id": MODEL_PROD_FLUX_9B})
        allocation = plan["resource_allocation"]
        self.assertEqual(allocation["gpu"], required_gpu)
        self.assertEqual(allocation["model_id"], MODEL_PROD_FLUX_9B)
        self.assertEqual(allocation["model_weights_bytes"], PROD_SNAPSHOT_9B_TOTAL_BYTES)
        # Two copies on the Modal L40S (24 + 24); one on the Beam RTX5090.
        self.assertEqual(allocation["worker_memory_gib"], 48 if self.provider == "modal" else 24)
        self.assertEqual(allocation["model_license"], "FLUX non-commercial")
        self.assertNotEqual(plan["plan_hash"], self.h.plan()["plan_hash"])
        rejected, _ = self.h.request("plan", {"credentials": self.h.credentials,
            "installation_id": "mc-ab12cd", "options": {"model_id": MODEL_PROD_FLUX_9B, "gpu": self.default_gpu}})
        self.assertEqual(rejected["error"]["code"], "ERR_VALIDATION_ERROR")

    def test_default_plan_quotes_memory_for_two_pipeline_copies(self) -> None:
        # FLUX.2 Klein 4B on the default GPU: two renders at once (common/capacity.py).
        # Modal adds one resident copy to one process; Beam runs a second worker process.
        plan = self.h.plan()
        allocation = plan["resource_allocation"]
        self.assertEqual(allocation["worker_memory_gib"], 19 if self.provider == "modal" else 24)
        self.assertIn(f"{allocation['worker_memory_gib']} GiB", plan["estimated_monthly_cost"])

    def test_9b_apply_passes_the_endpoint_check(self) -> None:
        # The check expects the model the installation chose, not the default one.
        self.h.cloud.seed_script = [seed_state("done", PROD_SNAPSHOT_9B_TOTAL_BYTES, now=2.0, model_id=MODEL_PROD_FLUX_9B)]
        response = self.h.apply(options={"model_id": MODEL_PROD_FLUX_9B})
        self.assertTrue(response["success"], response)
        self.assertEqual(response["data"]["model"]["model_id"], MODEL_PROD_FLUX_9B)
        self.assertEqual(response["data"]["compatibility_status"], "compatible")

    def test_qwen_plan_and_apply(self) -> None:
        required_gpu = "L40S" if self.provider == "modal" else "RTX5090"
        plan = self.h.plan(options={"model_id": MODEL_PROD_QWEN})
        allocation = plan["resource_allocation"]
        self.assertEqual(allocation["gpu"], required_gpu)
        self.assertEqual(allocation["model_weights_bytes"], PROD_SNAPSHOT_QWEN_TOTAL_BYTES)
        # Two copies on the Modal L40S (32 + 24); one on the Beam RTX5090.
        self.assertEqual(allocation["worker_memory_gib"], 56 if self.provider == "modal" else 32)
        self.assertEqual(allocation["model_license"], "Apache-2.0")
        option = next(o for o in allocation["model_options"] if o["model_id"] == MODEL_PROD_QWEN)
        self.assertEqual((option["label"], option["required_gpu"]), ("Qwen-Image-Edit-2511 (4-bit, Lightning)", required_gpu))
        rejected, _ = self.h.request("plan", {"credentials": self.h.credentials,
            "installation_id": "mc-ab12cd", "options": {"model_id": MODEL_PROD_QWEN, "gpu": self.default_gpu}})
        self.assertEqual(rejected["error"]["code"], "ERR_VALIDATION_ERROR")
        self.h.cloud.seed_script = [seed_state("done", PROD_SNAPSHOT_QWEN_TOTAL_BYTES, now=2.0, model_id=MODEL_PROD_QWEN)]
        response = self.h.apply(options={"model_id": MODEL_PROD_QWEN})
        self.assertTrue(response["success"], response)
        self.assertEqual(response["data"]["model"]["model_id"], MODEL_PROD_QWEN)

    def test_selected_analysis_graphs_change_the_plan_and_apply(self) -> None:
        chosen = ["text_regions_rt@1", "text_mask_sam_ts@1"]
        options = {"analysis_models": chosen}
        plan = self.h.plan(options=options)
        self.assertEqual(plan["resource_allocation"]["analysis_models"], ["text_mask_sam_ts@1", "text_regions_rt@1"])
        self.assertNotEqual(plan["plan_hash"], self.h.plan()["plan_hash"])
        if self.provider == "beam":
            self.assertIn("mc-ab12cd-analysis", [item["name"] for item in plan["resources_to_create"]])
        response = self.h.apply(options=options)
        self.assertTrue(response["success"], response)
        self.assertEqual(response["data"]["analysis_models"], ["text_mask_sam_ts@1", "text_regions_rt@1"])
        if self.provider == "beam":
            deployed = [entry for entry in self.h.cloud.calls if entry[0] == "deploy"]
            self.assertTrue(any(entry[1].get("handler") == "analyze" for entry in deployed))

    # ---------- apply ----------

    def test_apply_returns_ic1_and_reports_ic2(self) -> None:
        response = self.h.apply()
        self.assertTrue(response["success"], response)
        data = response["data"]
        self.assertEqual(set(data), IC1_KEYS)
        self.assertEqual((data["installation_id"], data["provider"], data["stage"]), ("mc-ab12cd", self.provider, "completed"))
        self.assertTrue(data["endpoint_url"].startswith("https://") and data["endpoint_url"].endswith("/mc/v1"))
        self.assertEqual((data["gpu"], data["idle_seconds"], data["compatibility_status"]), (self.default_gpu, 120, "compatible"))
        self.assertEqual(set(data["model"]), {"model_id", "model_revision", "recipe_id", "preprocessing_version"})
        self.assertEqual([r["type"] for r in data["resources_created"]], self.expected_resource_types)
        self.assertIs(data["setup_credential_forgotten"], True)
        self.check_runtime_credential(data["runtime_credential"])

        records = parse_progress(self.h.progress_lines, "apply")
        states = final_states(records)
        for step in PIPELINE:
            self.assertEqual(states[step], "skip" if step in self.provider_skips else "done", step)
        self.assertEqual((states["inspect"], states["validate"]), ("done", "done"))
        weights = [(state, pct) for step, state, pct in records if step == "weights"]
        self.assertEqual(weights, [("start", None), ("start", 50), ("start", 100), ("done", 100)], "weights report their percentage")
        order: List[str] = []
        for step, _, _ in records:
            if step not in order:
                order.append(step)
        self.assertEqual(order, ["inspect", "validate"] + PIPELINE)

    def test_apply_keeps_setup_credentials_out_of_the_journal_and_response(self) -> None:
        response = self.h.apply()
        self.assertTrue(response["success"])
        text = self.h.journal_text()
        for secret in self.h.credentials.values():
            self.assertNotIn(secret, text)
        if self.provider == "modal":
            self.assertNotIn(response["data"]["runtime_credential"]["token_secret"], text)
            for secret in self.h.credentials.values():
                self.assertNotIn(secret, self.h.raw)
        for line in self.h.progress_lines:
            for secret in self.h.credentials.values():
                self.assertNotIn(secret, line)

    def test_resume_at_completed_reissues_the_credential(self) -> None:
        first = self.h.apply()["data"]
        deploys_before = self.deploy_count()
        response = self.h.resume(credentials=self.second_key())
        self.assertTrue(response["success"], response)
        data = response["data"]
        self.assertEqual(set(data), IC1_KEYS)
        self.assertEqual((data["stage"], data["endpoint_url"]), ("completed", first["endpoint_url"]))
        self.check_runtime_credential(data["runtime_credential"], reissued_from=first["runtime_credential"])
        states = final_states(parse_progress(self.h.progress_lines, "resume"))
        # Modal redeploys the same app so an older installation picks up this
        # release's gateway; nothing else is created or re-seeded.
        redeployed = ("image", "deploy") if self.provider == "modal" else ()
        self.assertEqual(self.deploy_count(), deploys_before + (1 if redeployed else 0))
        for step in ("volume", "state", "image", "deploy", "weights", "endpoint"):
            self.assertEqual(states[step], "done" if step in redeployed else "skip", step)
        self.assertEqual(states["health"], "done")
        self.assert_single_installation()

    def test_kill_during_apply_then_resume_finishes_without_duplicates(self) -> None:
        for point in self.kill_points:
            with self.subTest(kill_after=point):
                self.h.close()
                self.h = Harness(self.provider)
                self.h.cloud.kill_after = point
                with self.assertRaises(Killed):
                    self.h.apply()
                self.assertIsNone(self.h.cloud.kill_after, "the kill point was reached")
                response = self.h.resume()
                self.assertTrue(response["success"], (point, response))
                self.assert_single_installation()

    # ---------- failures ----------

    def test_bad_credentials_fail_before_any_change(self) -> None:
        response, lines = self.h.request(
            "apply",
            {"credentials": self.wrong_key(), "installation_id": "mc-ab12cd", "approved_plan_hash": "a" * 64, "options": {}},
        )
        self.assertEqual(response["error"]["code"], "ERR_ACTIONABLE_MISSING_PERMISSION")
        self.assertFalse((self.h.root / "installations").exists())
        self.assertEqual(self.mutating_calls(), [])
        self.assertEqual(final_states(parse_progress(lines, "apply")), {"inspect": "fail"})
        response, _ = self.h.request("inspect", {"credentials": {}})
        self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR")

    def test_missing_sdk_is_provider_unavailable(self) -> None:
        self.h.sdk_loader = _unavailable
        response, _ = self.h.request("inspect", {"credentials": self.h.credentials})
        self.assertEqual(response["error"]["code"], "ERR_PROVIDER_UNAVAILABLE")

    def test_unapproved_plan_hash_changes_nothing(self) -> None:
        response, _ = self.h.request(
            "apply",
            {"credentials": self.h.credentials, "installation_id": "mc-ab12cd", "approved_plan_hash": "0" * 64, "options": {}},
        )
        self.assertEqual(response["error"]["code"], "ERR_UNAPPROVED_PLAN")
        self.assertFalse((self.h.root / "installations").exists())
        self.assertEqual(self.mutating_calls(), [])

    def test_failed_weights_are_typed_and_resume_downloads_again(self) -> None:
        self.h.cloud.seed_script = [seed_state("failed", 0, error_code="weights_download_failed", message="hub said 503", now=3.0)]
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("hub said 503", response["error"]["message"])
        journal = self.h.journal()
        self.assertEqual(journal.stage, "failed")
        self.assertIn("hub said 503", journal.last_error)
        self.assertEqual(final_states(parse_progress(self.h.progress_lines, "apply"))["weights"], "fail")
        self.h.cloud.seed_script = [seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=4.0)]
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertEqual(self.seed_starts(), 2)

    def test_slow_weights_time_out_typed_and_resume_reattaches(self) -> None:
        running = seed_state("running", 10, now=5.0)
        self.h.cloud.seed_script = [running] * 1000
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_TIMEOUT")
        self.assertIn("Resume", response["error"]["actionable_guidance"])
        self.h.cloud.seed_script = [running, seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=6.0)]
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertEqual(self.seed_starts(), 1, "resume waits for the running download instead of starting another")

    def test_endpoint_refusing_the_credential_is_typed(self) -> None:
        self.refuse_runtime_credential()
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("refused the runtime credential", response["error"]["message"])
        self.assertEqual(self.h.journal().stage, "failed")

    def test_resume_refuses_other_options_other_accounts_and_cleanup(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        response = self.h.resume(options={"idle_seconds": 600})
        self.assertEqual(response["error"]["code"], "ERR_UNAPPROVED_PLAN")
        self.use_other_account()
        response = self.h.resume()
        self.assertEqual(response["error"]["code"], "ERR_UNAPPROVED_PLAN")
        # A cleanup that has started, even one that stopped half way, is never resumed.
        self.use_own_account()
        self.h.cloud.fail[self.volume_delete_call] = RuntimeError("storage service unavailable")
        response = self.cleanup_apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("cleanup again", response["error"]["actionable_guidance"])
        self.assertEqual(self.h.journal().stage, "cleanup_planned")
        response = self.h.resume()
        self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR")
        self.assertIn("Finish the cleanup", response["error"]["actionable_guidance"])

    def test_same_size_file_replacement_changes_the_plan_hash(self) -> None:
        from dataclasses import replace
        from deploy.cloud.common.manifest import PRODUCTION_MODELS
        before = self.h.plan()
        model = PRODUCTION_MODELS[MODEL_PROD_FLUX]
        replaced = replace(model, files=((model.files[0][0] + ".v2", model.files[0][1]), *model.files[1:]))
        with mock.patch.dict(PRODUCTION_MODELS, {MODEL_PROD_FLUX: replaced}):
            after = self.h.plan()
        self.assertEqual(before["resource_allocation"]["model_weights_bytes"], after["resource_allocation"]["model_weights_bytes"])
        self.assertNotEqual(before["plan_hash"], after["plan_hash"])

    def test_resume_reseeds_after_same_size_file_replacement(self) -> None:
        from dataclasses import replace
        from deploy.cloud.common.manifest import PRODUCTION_MODELS
        self.assertTrue(self.h.apply()["success"])
        before = self.h.journal().plan_hash
        model = PRODUCTION_MODELS[MODEL_PROD_FLUX]
        replaced = replace(model, files=((model.files[0][0] + ".v2", model.files[0][1]), *model.files[1:]))
        with mock.patch.dict(PRODUCTION_MODELS, {MODEL_PROD_FLUX: replaced}):
            self.h.cloud.next_seed_scripts = [[seed_state("done", model.total_bytes, now=9.0)]]
            response = self.h.resume()
            self.assertTrue(response["success"], response)
            self.assertNotEqual(before, self.h.journal().plan_hash)
            self.assertEqual(self.seed_starts(), 2)

    def test_qwen_adapter_update_redeploys_before_reseeding(self) -> None:
        from dataclasses import replace
        from deploy.cloud.common.manifest import LIGHTNING_FILE, PRODUCTION_MODELS
        model = PRODUCTION_MODELS[MODEL_PROD_QWEN]
        old_filename = LIGHTNING_FILE.replace("8steps", "4steps")
        old_model = replace(
            model,
            files=tuple((path.replace(LIGHTNING_FILE, old_filename), size) for path, size in model.files),
            adapters=tuple(replace(a, filename=old_filename) if a.filename == LIGHTNING_FILE else a
                           for a in model.adapters),
        )
        with mock.patch.dict(PRODUCTION_MODELS, {MODEL_PROD_QWEN: old_model}):
            self.h.cloud.seed_script = [seed_state("done", model.total_bytes, now=2.0, model_id=MODEL_PROD_QWEN)]
            self.assertTrue(self.h.apply(options={"model_id": MODEL_PROD_QWEN})["success"])
        before = self.h.journal()
        deploys = self.deploy_count()
        self.h.cloud.next_seed_scripts = [[seed_state("done", model.total_bytes, now=9.0, model_id=MODEL_PROD_QWEN)]]
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertEqual(self.deploy_count(), deploys + (1 if self.provider == "modal" else 3))
        states = final_states(parse_progress(self.h.progress_lines, "resume"))
        self.assertEqual((states["image"], states["deploy"], states["weights"], states["endpoint"]),
                         ("done", "done", "done", "done"))
        self.assertEqual((states["volume"], states["state"]), ("skip", "skip"))
        self.assertEqual(self.h.journal().options, before.options)
        self.assertEqual(self.seed_starts(), 2)
        calls = self.h.cloud.calls
        last_deploy = max(i for i, (name, _) in enumerate(calls) if name == ("App.deploy" if self.provider == "modal" else "deploy"))
        last_seed = max(i for i, (name, _) in enumerate(calls) if name == self.seed_start_call)
        self.assertLess(last_deploy, last_seed, "the current seed code must be deployed before its job starts")

        # An unchanged resume keeps its completed seed and the same persistent resources.
        deploys = self.deploy_count()
        self.assertTrue(self.h.resume()["success"])
        self.assertEqual(self.seed_starts(), 2)
        self.assertEqual(self.deploy_count(), deploys + (1 if self.provider == "modal" else 0))

    def test_a_release_with_another_recipe_updates_in_place(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        before = self.h.journal()
        deploys = self.deploy_count()
        real = driver_base.production_model

        def next_release(model_id):
            return {**real(model_id), "recipe_id": "mc-next-recipe-v2"}

        with mock.patch.object(driver_base, "production_model", next_release):
            # Another account is still refused, before anything changes.
            self.use_other_account()
            response = self.h.resume()
            self.assertEqual(response["error"]["code"], "ERR_UNAPPROVED_PLAN")
            self.assertEqual(self.h.journal().plan_hash, before.plan_hash)
            self.use_own_account()
            # The same account redeploys with the new recipe and records its plan.
            response = self.h.resume()
            self.assertTrue(response["success"], response)
            plan = self.h.plan()["plan_hash"]
        journal = self.h.journal()
        self.assertNotEqual(plan, before.plan_hash)
        self.assertEqual((journal.plan_hash, journal.approved_plan_hash), (plan, plan))
        self.assertEqual(journal.options, before.options)
        self.assertEqual(self.deploy_count(), deploys + (1 if self.provider == "modal" else 3),
                         "the new release is deployed")

    def test_the_account_is_journaled_with_the_plan_and_cleanup_needs_it(self) -> None:
        # The first set_state of the run dies: the account is already in the journal,
        # written by the same save that created it.
        with mock.patch.object(InstallationJournal, "set_state", side_effect=Killed("set_state")):
            with self.assertRaises(Killed):
                self.h.apply()
        recorded = {k: v for k, v in self.h.journal().provider_state.items() if k in ("account_id", "environment_name")}
        self.assertTrue(recorded.get("account_id"))
        # A journal without the account (written by an older helper) is never cleaned up,
        # since a key for another account would find nothing and forget everything.
        journal = InstallationJournal(self.h.root, "mc-ab12cd", self.provider)
        journal.load()
        journal.set_state(account_id=None, environment_name=None)
        before = self.h.journal_text()
        response = self.cleanup_apply()
        self.assertEqual(response["error"]["code"], "ERR_UNAPPROVED_PLAN")
        self.assertIn("Resume", response["error"]["actionable_guidance"])
        self.assertEqual(self.h.journal_text(), before, "the refused cleanup changed nothing")
        # Resume with the right account records it again, and cleanup then works.
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        state = self.h.journal().provider_state
        self.assertEqual({k: state.get(k) for k in recorded}, recorded)
        response = self.cleanup_apply()
        self.assertTrue(response["success"], response)
        self.assert_cloud_empty()

    def test_seed_that_never_started_is_started_once_after_the_start_grace(self) -> None:
        # The helper stopped after journaling the intent but before the job was queued.
        self.h.cloud.fail[self.seed_start_call] = Killed(self.seed_start_call)
        with self.assertRaises(Killed):
            self.h.apply()
        self.assertIn(SEED_INTENT_KEY, self.h.journal().provider_state)
        del self.h.cloud.fail[self.seed_start_call]
        started = self.h.clock.now
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertEqual(self.seed_starts(), 1)
        self.assertGreaterEqual(self.h.clock.now - started, SEED_START_GRACE_SECONDS, "no new job while the first may start")

    def test_seed_that_stopped_reporting_is_replaced_once(self) -> None:
        # The helper stopped before it learned the job's id; the job then died after one report.
        cloud = self.h.cloud
        cloud.kill_after = self.seed_start_call
        cloud.seed_script = [seed_state("running", 10, now=5.0)]
        cloud.next_seed_scripts = [[seed_state("running", 20, now=70.0), seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=71.0)]]
        with self.assertRaises(Killed):
            self.h.apply()
        started = self.h.clock.now
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertEqual(self.seed_starts(), 2)
        self.assertGreaterEqual(self.h.clock.now - started, SEED_STALE_SECONDS)

    def test_apply_on_completed_installation_is_refused(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR")

    # ---------- cleanup ----------

    def cleanup_plan(self) -> Dict[str, Any]:
        response, _ = self.h.request("cleanup_plan", {"installation_id": "mc-ab12cd"})
        self.assertTrue(response["success"], response)
        return response["data"]

    def test_cleanup_removes_everything_the_journal_recorded(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        before = self.h.journal_text()
        calls = len(self.h.cloud.calls)
        plan = self.cleanup_plan()
        self.assertEqual(self.h.journal_text(), before, "cleanup_plan changes nothing")
        self.assertEqual(len(self.h.cloud.calls), calls, "cleanup_plan calls no provider")
        self.assertEqual(sorted(r["type"] for r in plan["resources_to_delete"]), sorted(self.expected_resource_types))
        self.assertTrue(all(r["name"] and r["resource_type"] == r["type"] for r in plan["resources_to_delete"]))
        self.assertEqual(plan["foreign_resources_ignored"], [])
        self.assertTrue(plan["persistent_storage_requires_explicit_confirmation"])

        params = {"credentials": self.h.credentials, "installation_id": "mc-ab12cd", "approved_cleanup_plan_hash": plan["plan_hash"]}
        response, _ = self.h.request("cleanup_apply", params)
        self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR", "the volume needs explicit confirmation")
        response, _ = self.h.request("cleanup_apply", dict(params, approved_cleanup_plan_hash="f" * 64, confirm_delete_persistent_storage=True))
        self.assertEqual(response["error"]["code"], "ERR_UNAPPROVED_PLAN")

        response, lines = self.h.request("cleanup_apply", dict(params, confirm_delete_persistent_storage=True))
        self.assertTrue(response["success"], response)
        self.assertEqual(response["data"]["stage"], "cleaned_up")
        self.assertEqual(sorted(r["type"] for r in response["data"]["deleted"]), sorted(self.expected_resource_types))
        self.assert_cloud_empty()
        journal = self.h.journal()
        self.assertEqual((journal.stage, journal.resources), ("cleaned_up", []))
        records = parse_progress(lines, "cleanup_apply")
        self.assertEqual([s for s, st, _ in records if st == "start" and _ is None], ["validate", "inspect", "cleanup"])
        self.assertEqual([p for s, _, p in records if s == "cleanup" and p is not None][-1], 100)
        self.assertEqual(self.cleanup_plan()["resources_to_delete"], [])
        response = self.h.resume()
        self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR")

    def test_cleanup_after_a_failed_apply_and_with_another_account(self) -> None:
        self.refuse_runtime_credential()
        self.assertFalse(self.h.apply()["success"])
        plan = self.cleanup_plan()
        self.use_other_account()
        params = {
            "credentials": self.h.credentials,
            "installation_id": "mc-ab12cd",
            "approved_cleanup_plan_hash": plan["plan_hash"],
            "confirm_delete_persistent_storage": True,
        }
        response, _ = self.h.request("cleanup_apply", params)
        self.assertEqual(response["error"]["code"], "ERR_UNAPPROVED_PLAN")
        self.assertNotEqual(self.h.journal().stage, "cleaned_up")
        self.assertEqual(sorted(r["type"] for r in self.cleanup_plan()["resources_to_delete"]), sorted(self.expected_resource_types))
        self.assert_single_installation()

    def test_probe_compatibility_checks_health_with_the_runtime_credential(self) -> None:
        data = self.h.apply()["data"]
        params = {"endpoint_url": data["endpoint_url"], "runtime_credential": data["runtime_credential"]}
        response, lines = self.h.request("probe_compatibility", params)
        self.assertTrue(response["success"], response)
        self.assertEqual((response["data"]["healthy"], response["data"]["gpu_incurred"]), (True, False))
        self.assertEqual(final_states(parse_progress(lines, "probe_compatibility")), {"health": "done"})
        for secret in data["runtime_credential"].values():
            if secret not in ("modal_proxy", "beam_bearer"):
                self.assertNotIn(secret, self.h.raw)
        response, _ = self.h.request("probe_compatibility", dict(params, endpoint_url="http://example.com/mc/v1"))
        self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR")

    # ---------- provider hooks ----------

    def mutating_calls(self) -> List[str]:
        return [name for name, _ in self.h.cloud.calls if name in self.mutating]


class ModalDriverTest(ProviderCases, unittest.TestCase):
    provider = "modal"
    default_gpu = "L4"
    gpu_options = ("L4", "A10", "L40S")
    expected_resource_types = ["volume", "dict", "app", "proxy_token"]
    provider_skips = ["secret"]
    kill_points = ["Volume.objects.create", "Dict.objects.create", "App.deploy", "Function.spawn"]
    mutating = {"Volume.objects.create", "Dict.objects.create", "App.deploy", "Function.spawn", "proxy_tokens.create"}

    def test_seed_allocation_and_cost_disclose_sam_memory_and_retry_window(self) -> None:
        for selected, memory, timeout, hourly in (
            ([], 4, 3600, "$0.13/h"),
            (["text_regions_rt@1"], 4, 3600, "$0.13/h"),
            (["text_mask_sam_ts@1"], 24, 7200, "$0.29/h"),
        ):
            with self.subTest(analysis_models=selected):
                plan = self.h.plan(options={"analysis_models": selected})
                allocation = plan["resource_allocation"]
                self.assertEqual((allocation["seed_cpu"], allocation["seed_memory_gib"],
                                  allocation["seed_timeout_seconds"]), (2.0, memory, timeout))
                self.assertIn(f"2 CPU and {memory} GiB", plan["estimated_monthly_cost"])
                self.assertIn(hourly, plan["estimated_monthly_cost"])
                self.assertIn("any retries", plan["estimated_monthly_cost"])
                self.assertIn(f"{memory} GiB", plan["notes"][0])

    def check_runtime_credential(self, credential: Dict[str, str], reissued_from: Optional[Dict[str, str]] = None) -> None:
        self.assertEqual(set(credential), {"kind", "token_id", "token_secret"})
        self.assertEqual(credential["kind"], "modal_proxy")
        self.assertEqual(list(self.h.cloud.proxy_tokens), [credential["token_id"]], "exactly one live proxy token")
        self.assertEqual(self.h.cloud.proxy_tokens[credential["token_id"]]["secret"], credential["token_secret"])
        self.assertEqual(self.h.cloud.proxy_tokens[credential["token_id"]]["environments"], ["main"])
        if reissued_from is not None:
            self.assertNotEqual(credential["token_id"], reissued_from["token_id"])
            self.assertNotIn(reissued_from["token_id"], self.h.cloud.proxy_tokens, "the old token was deleted first")

    def second_key(self) -> Dict[str, str]:
        return self.h.credentials

    def wrong_key(self) -> Dict[str, str]:
        return {"token_id": "ak-other-token", "token_secret": "as-other-secret"}

    seed_start_call = "Function.spawn"
    volume_delete_call = "Volume.objects.delete"

    def use_other_account(self) -> None:
        self.h.cloud.workspace_name = "someone-else"

    def use_own_account(self) -> None:
        vars(self.h.cloud).pop("workspace_name", None)

    def refuse_runtime_credential(self) -> None:
        self.h.cloud.http = lambda: (lambda url, headers, timeout: (403, b"{}"))

    def deploy_count(self) -> int:
        return self.h.cloud.count("App.deploy")

    def seed_starts(self) -> int:
        return self.h.cloud.count("Function.spawn")

    def assert_single_installation(self) -> None:
        cloud = self.h.cloud
        self.assertEqual((cloud.volumes, set(cloud.dicts), set(cloud.apps)), ({"mc-ab12cd-weights"}, {"mc-ab12cd-jobs"}, {"mc-ab12cd"}))
        self.assertEqual(len(cloud.proxy_tokens), 1)
        self.assertEqual(self.seed_starts(), 1, "one seeding job")

    def assert_cloud_empty(self) -> None:
        cloud = self.h.cloud
        self.assertEqual((cloud.volumes, cloud.dicts, cloud.apps, cloud.proxy_tokens), (set(), {}, {}, {}))

    def test_resume_with_an_approved_plan_adds_page_denoise(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        before = self.h.journal()
        deploys = self.deploy_count()
        wanted = {"denoise": True}

        # New options need the hash of a plan made for exactly them.
        other = self.h.plan(options={"idle_seconds": 600})["plan_hash"]
        response = self.h.resume(options=wanted, approved_plan_hash=other)
        self.assertEqual(response["error"]["code"], "ERR_UNAPPROVED_PLAN")
        self.assertEqual((self.h.journal().options, self.h.journal().plan_hash), (before.options, before.plan_hash))
        self.assertEqual(self.deploy_count(), deploys, "a refused update changes nothing")

        plan = self.h.plan(options=wanted)
        self.assertTrue(plan["resource_allocation"]["denoise"])
        self.h.cloud.next_seed_scripts = [[seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=9.0)]]
        response = self.h.resume(options=wanted, approved_plan_hash=plan["plan_hash"])
        self.assertTrue(response["success"], response)
        self.assertTrue(response["data"]["denoise"])
        journal = self.h.journal()
        self.assertTrue(journal.options["denoise"])
        self.assertEqual((journal.plan_hash, journal.approved_plan_hash), (plan["plan_hash"], plan["plan_hash"]))
        states = final_states(parse_progress(self.h.progress_lines, "resume"))
        self.assertEqual((states["deploy"], states["weights"]), ("done", "done"))
        self.assertEqual(self.deploy_count(), deploys + 1)
        self.assertEqual(self.seed_starts(), 2, "the update downloads the denoise models")
        self.assertEqual(set(self.h.cloud.apps), {"mc-ab12cd"}, "the same installation, updated in place")

        # A plain resume later keeps the new options and does not download again.
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertTrue(response["data"]["denoise"])
        self.assertEqual(self.seed_starts(), 2)

    def test_resume_with_an_approved_plan_moves_the_gateway_to_another_routing_region(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        first = self.h.journal()
        self.assertEqual(first.endpoint_url, "https://studio-ws--mc-ab12cd-gateway.modal.run/mc/v1")
        default = self.h.plan()
        self.assertEqual(default["resource_allocation"]["routing_region"], "us-east")
        self.assertEqual(default["resource_allocation"]["routing_region_options"], ["us-east", "us-west", "eu-west", "ap-south"])
        # Naming the default is no change: the plan approved before the choice existed still resumes.
        self.assertEqual(self.h.plan(options={"routing_region": "us-east"})["plan_hash"], first.plan_hash)
        self.assertEqual(default["plan_hash"], first.plan_hash)

        wanted = {"routing_region": "ap-south"}
        plan = self.h.plan(options=wanted)
        self.assertEqual(plan["resource_allocation"]["routing_region"], "ap-south")
        self.assertNotEqual(plan["plan_hash"], first.plan_hash)
        self.h.cloud.next_seed_scripts = [[seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=9.0)]]
        response = self.h.resume(options=wanted, approved_plan_hash=plan["plan_hash"])
        self.assertTrue(response["success"], response)

        # Modal fixes a Function's routing region at its first deploy, so the region
        # has a gateway Function of its own name and with it a new endpoint.
        journal = self.h.journal()
        self.assertEqual(journal.options["routing_region"], "ap-south")
        self.assertEqual(journal.endpoint_url, "https://studio-ws--mc-ab12cd-gateway-ap-south.ap-south.modal.run/mc/v1")
        self.assertEqual(response["data"]["endpoint_url"], journal.endpoint_url)
        functions = self.h.cloud.apps["mc-ab12cd"]["functions"]
        self.assertIn("gateway_ap_south", functions)
        self.assertNotIn("gateway", functions)
        self.assertEqual(self.h.cloud.dicts["mc-ab12cd-jobs"]["install:options"]["routing_region"], "ap-south")
        self.assertEqual(set(self.h.cloud.apps), {"mc-ab12cd"}, "the same installation, updated in place")

        # A plain resume later keeps the region.
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertEqual(response["data"]["endpoint_url"], journal.endpoint_url)

        response, _ = self.h.request("plan", {"credentials": self.h.credentials, "installation_id": "mc-ab12cd",
                                              "options": {"routing_region": "mars"}})
        self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR")

    def test_kill_while_reissuing_the_credential_then_resume(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        old_token = next(iter(self.h.cloud.proxy_tokens))
        self.h.cloud.kill_after = "proxy_tokens.delete"
        with self.assertRaises(Killed):
            self.h.resume()
        self.assertEqual(self.h.journal().stage, "validated")
        self.assertNotIn(old_token, self.h.cloud.proxy_tokens)
        self.assertEqual(len(self.h.cloud.proxy_tokens), 1, "the verified replacement remains live")
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.check_runtime_credential(response["data"]["runtime_credential"])

    def test_replacement_is_checked_before_old_token_is_revoked(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        old_token = next(iter(self.h.cloud.proxy_tokens))
        before = len(self.h.cloud.calls)
        self.assertTrue(self.h.resume()["success"])
        calls = [name for name, _ in self.h.cloud.calls[before:]]
        self.assertLess(calls.index("http.get"), calls.index("proxy_tokens.delete"))
        self.assertNotIn(old_token, self.h.cloud.proxy_tokens)

    def test_interrupted_replacement_keeps_old_token_live(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        old_token = next(iter(self.h.cloud.proxy_tokens))
        self.h.cloud.kill_after = "proxy_tokens.create"
        with self.assertRaises(Killed):
            self.h.resume()
        self.assertIn(old_token, self.h.cloud.proxy_tokens)
        self.assertEqual(len(self.h.cloud.proxy_tokens), 2)
        response = self.h.resume()
        self.assertEqual(response["error"]["code"], "ERR_ORPHANED_TOKEN")

    def test_plan_describes_environment_scoped_proxy_token(self) -> None:
        plan = self.h.plan()
        token = next(r for r in plan["resources_to_create"] if r["type"] == "proxy_token")
        self.assertIn("endpoints in this environment", token["description"])
        self.assertNotIn("only call this app", token["description"])

    def test_kill_after_token_create_reports_orphan_on_resume(self) -> None:
        self.h.cloud.kill_after = "proxy_tokens.create"
        with self.assertRaises(Killed):
            self.h.apply()
        self.assertIs(self.h.journal().provider_state[TOKEN_CREATE_INTENT_KEY], True)
        self.assertEqual(len(self.h.cloud.proxy_tokens), 1)
        self.assertIsNone(self.h.journal().runtime_credential_ref)

        creates_before = self.h.cloud.count("proxy_tokens.create")
        deletes_before = self.h.cloud.count("proxy_tokens.delete")
        response = self.h.resume()
        self.assertEqual(response["error"]["code"], "ERR_ORPHANED_TOKEN")
        self.assertIn("Modal dashboard", response["error"]["actionable_guidance"])
        self.assertIn("remove", response["error"]["remedy_steps"][0])
        self.assertIn("Cleanup", response["error"]["remedy_steps"][1])
        self.assertEqual(self.h.cloud.count("proxy_tokens.create"), creates_before)
        self.assertEqual(self.h.cloud.count("proxy_tokens.delete"), deletes_before)
        self.assertEqual(len(self.h.cloud.proxy_tokens), 1)
        self.h.cloud.proxy_tokens.clear()  # Simulate removal in the Modal dashboard.
        self.assertTrue(self.cleanup_apply()["success"])

    def test_a_probed_token_id_stays_readable_for_cleanup(self) -> None:
        """Each request is its own redaction session, so a later journal save keeps the token ID."""
        data = self.h.apply()["data"]
        params = {"endpoint_url": data["endpoint_url"], "runtime_credential": data["runtime_credential"]}
        self.assertTrue(self.h.request("probe_compatibility", params)[0]["success"])
        self.assertTrue(self.h.request("forget_credential", {"installation_id": "mc-ab12cd"})[0]["success"])
        self.assertIn(data["runtime_credential"]["token_id"], self.h.journal_text())
        plan = self.cleanup_plan()
        response, _ = self.h.request(
            "cleanup_apply",
            {
                "credentials": self.h.credentials,
                "installation_id": "mc-ab12cd",
                "approved_cleanup_plan_hash": plan["plan_hash"],
                "confirm_delete_persistent_storage": True,
            },
        )
        self.assertTrue(response["success"], response)
        self.assertEqual(self.h.cloud.proxy_tokens, {})

    def test_a_call_modal_no_longer_knows_is_replaced_once(self) -> None:
        stuck = seed_state("running", 10, now=5.0)
        cloud = self.h.cloud
        cloud.next_call_outcomes = [{"expired": True}]
        cloud.seed_script = [stuck]
        cloud.next_seed_scripts = [[seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=9.0)]]
        self.assertTrue(self.h.apply()["success"])
        self.assertEqual(self.seed_starts(), 2)
        # A replacement that also vanishes fails typed instead of spawning again.
        self.fresh_harness()
        cloud = self.h.cloud
        cloud.next_call_outcomes = [{"expired": True}, {"expired": True}]
        cloud.seed_script, cloud.next_seed_scripts = [stuck], [[stuck]]
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("without reporting a result", response["error"]["message"])
        self.assertEqual(self.seed_starts(), 2)
        self.assertNotIn(SEED_INTENT_KEY, self.h.journal().provider_state)

    def test_every_call_uses_the_resolved_environment(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        self.assertEqual(self.h.cloud.environments_seen, {"main"})

    def test_proxy_tokens_are_not_setup_credentials(self) -> None:
        response, _ = self.h.request("inspect", {"credentials": {"token_id": "wk-abc12345", "token_secret": "ws-abc12345"}})
        self.assertEqual(response["error"]["code"], "ERR_VALIDATION_ERROR")
        self.assertIn("API token", response["error"]["actionable_guidance"])

    def test_unreachable_modal_is_provider_unavailable(self) -> None:
        self.h.cloud.hello_error = self.h.cloud.sdk().exception.ConnectionError("Deadline exceeded")
        response, _ = self.h.request("inspect", {"credentials": self.h.credentials})
        self.assertEqual(response["error"]["code"], "ERR_PROVIDER_UNAVAILABLE")

    def test_silent_modal_is_provider_unavailable_within_the_budget(self) -> None:
        gate = threading.Event()
        self.addCleanup(gate.set)
        self.h.cloud.hello_gate = gate
        self.h.sign_in_seconds = 0.2
        started = time.monotonic()
        response, lines = self.h.request("inspect", {"credentials": self.h.credentials})
        self.assertEqual(response["error"]["code"], "ERR_PROVIDER_UNAVAILABLE")
        self.assertIn("did not answer", response["error"]["message"])
        self.assertLess(time.monotonic() - started, 5.0)
        self.assertEqual(final_states(parse_progress(lines, "inspect")), {"inspect": "fail"})

    def test_environment_allow_failure_is_tolerated(self) -> None:
        self.h.cloud.allow_error = self.h.cloud.sdk().exception.InvalidError("no environment access control")
        self.assertTrue(self.h.apply()["success"])
        self.assertIs(self.h.journal().provider_state["environment_allowed"], False)

    def test_deploy_failure_is_typed_and_redacted(self) -> None:
        self.h.cloud.fail["App.deploy"] = RuntimeError("build failed while using as-good-token-secret")
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertNotIn("as-good-token-secret", self.h.raw)
        self.assertNotIn("as-good-token-secret", self.h.journal_text())
        self.assertEqual(final_states(parse_progress(self.h.progress_lines, "apply"))["image"], "fail")


class BeamDriverTest(ProviderCases, unittest.TestCase):
    provider = "beam"
    default_gpu = "RTX4090"
    gpu_options = ("RTX4090", "A10G", "RTX5090")
    expected_resource_types = ["volume", "dict", "secret", "worker", "worker", "gateway"]
    provider_skips = ["token"]
    kill_points = ["get_or_create_volume", "create_secret", "deploy", "http.post"]
    mutating = {"get_or_create_volume", "map_set", "create_secret", "update_secret", "deploy", "http.post"}

    def check_runtime_credential(self, credential: Dict[str, str], reissued_from: Optional[Dict[str, str]] = None) -> None:
        # Beam has no scoped token: the runtime credential is the key this run was given.
        self.assertEqual(credential, {"kind": "beam_bearer", "token": self.current_key})
        secret_name = "MC_MC_AB12CD_TOKEN"
        self.assertEqual(self.h.cloud.secrets[secret_name], self.current_key, "the gateway secret holds the same key")

    @property
    def current_key(self) -> str:
        return getattr(self, "_current_key", self.h.credentials["token"])

    def second_key(self) -> Dict[str, str]:
        self.h.cloud.valid_tokens.add("b9-rotated-api-key")
        self._current_key = "b9-rotated-api-key"
        return {"token": "b9-rotated-api-key"}

    def wrong_key(self) -> Dict[str, str]:
        return {"token": "b9-unknown-key"}

    seed_start_call = "http.post"
    volume_delete_call = "delete_volume"

    def use_other_account(self) -> None:
        self.h.cloud.workspace_id = "ws-someone-else"

    def use_own_account(self) -> None:
        vars(self.h.cloud).pop("workspace_id", None)

    def refuse_runtime_credential(self) -> None:
        self.h.cloud.http = lambda: (lambda url, headers, timeout: (401, b"{}"))

    def deploy_count(self) -> int:
        return self.h.cloud.count("deploy")

    def seed_starts(self) -> int:
        return self.h.cloud.count("http.post")

    def assert_single_installation(self) -> None:
        cloud = self.h.cloud
        active = sorted(d["name"] for d in cloud.deployments.values() if d["active"])
        self.assertEqual(active, ["mc-ab12cd-gateway", "mc-ab12cd-seed", "mc-ab12cd-worker"])
        self.assertEqual((cloud.volumes, set(cloud.secrets)), ({"mc-ab12cd-weights"}, {"MC_MC_AB12CD_TOKEN"}))
        self.assertEqual(self.seed_starts(), 1, "one seeding job")

    def assert_cloud_empty(self) -> None:
        cloud = self.h.cloud
        self.assertEqual((cloud.volumes, cloud.secrets, cloud.deployments), (set(), {}, {}))
        self.assertEqual(cloud.maps.get("mc-ab12cd-jobs", {}), {})

    def test_session_channel_is_explicit_and_released(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        self.assertEqual((self.h.cloud.channel_open, self.h.cloud.global_channel), (0, None))
        self.assertTrue(all(0 < seconds <= 1620 for seconds in self.h.cloud.rpc_timeouts), "every RPC has a deadline")
        addresses = {details["addr"] for name, details in self.h.cloud.calls if name == "Channel"}
        self.assertEqual(addresses, {"gateway.beam.cloud:443"})

    def test_gateway_gets_the_worker_url_and_the_secret_name(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        gateway = [d for d in self.h.cloud.deployments.values() if d["name"] == "mc-ab12cd-gateway"][0]
        worker = [d for d in self.h.cloud.deployments.values() if d["name"] == "mc-ab12cd-worker"][0]
        self.assertEqual(gateway["env"]["MC_BEAM_WORKER_URL"], worker["invoke_url"])
        self.assertEqual(gateway["env"]["MC_BEAM_SECRET"], "MC_MC_AB12CD_TOKEN")
        self.assertNotIn(self.h.credentials["token"], json.dumps(gateway["env"]))

    def test_unreachable_beam_is_provider_unavailable(self) -> None:
        self.h.cloud.authorize_error = self.h.cloud.rpc_error("UNAVAILABLE", "connection refused")
        response, _ = self.h.request("inspect", {"credentials": self.h.credentials})
        self.assertEqual(response["error"]["code"], "ERR_PROVIDER_UNAVAILABLE")

    def test_path_style_gateway_url_is_refused(self) -> None:
        self.h.cloud.path_style_urls = True
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("path", response["error"]["message"])

    def test_seed_status_is_read_without_unpickling_code(self) -> None:
        plain = {"state": "done", "bytes_done": 1, "message": None}
        self.assertEqual(load_plain(pickle.dumps(plain)), plain)
        with self.assertRaises(pickle.UnpicklingError):
            load_plain(pickle.dumps(Path("/tmp")))
        # A status document that would run code when unpickled is ignored, never run.
        self.h.cloud.seed_script = [pickle.dumps(_Tripwire())] * 1000
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_TIMEOUT")
        self.assertEqual(_TRIPPED, [])

    # ---------- a seed task that ended without a result ----------

    def test_finished_or_unlisted_seed_task_without_a_result_is_replaced_once(self) -> None:
        stuck = seed_state("running", 10, now=5.0)
        working = [seed_state("running", 50, now=80.0), seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=81.0)]
        for status in ("COMPLETE", None):
            with self.subTest(task_status=status):
                self.fresh_harness()
                cloud = self.h.cloud
                # The first task's last status write never landed: its document says running.
                cloud.next_task_statuses = [status]
                cloud.seed_script, cloud.next_seed_scripts = [stuck], [list(working)]
                response = self.h.apply()
                self.assertTrue(response["success"], response)
                self.assertEqual(self.seed_starts(), 2)

    def test_a_second_seed_without_a_result_fails_typed_and_resume_starts_afresh(self) -> None:
        cloud = self.h.cloud
        cloud.next_task_statuses = ["COMPLETE", "COMPLETE"]
        cloud.seed_script = [seed_state("running", 10, now=5.0)]
        cloud.next_seed_scripts = [[seed_state("running", 10, now=6.0)]]
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("without reporting a result", response["error"]["message"])
        self.assertIn("Resume", response["error"]["actionable_guidance"])
        state = self.h.journal().provider_state
        self.assertNotIn("seed_task_id", state)
        self.assertNotIn(SEED_INTENT_KEY, state)
        self.assertEqual(self.seed_starts(), 2)
        cloud.next_seed_scripts = [[seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=9.0)]]
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertEqual(self.seed_starts(), 3)

    def test_an_unlisted_seed_that_still_reports_is_never_replaced(self) -> None:
        cloud = self.h.cloud
        cloud.next_task_statuses = [None]
        polls = 4 * int(SEED_STALE_SECONDS // 5)
        cloud.seed_script = [seed_state("running", 1000 * i, now=10.0 + i) for i in range(polls)]
        cloud.seed_script.append(seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=999.0))
        response = self.h.apply()
        self.assertTrue(response["success"], response)
        self.assertEqual(self.seed_starts(), 1)

    def test_a_failed_task_listing_is_typed_and_keeps_the_seed(self) -> None:
        cloud = self.h.cloud
        cloud.refused["list_tasks"] = "backend unavailable"
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("backend unavailable", response["error"]["message"])
        self.assertIn("seed_task_id", self.h.journal().provider_state)
        del cloud.refused["list_tasks"]
        cloud.seed_script = [seed_state("running", 50, now=6.0), seed_state("done", PROD_SNAPSHOT_TOTAL_BYTES, now=7.0)]
        response = self.h.resume()
        self.assertTrue(response["success"], response)
        self.assertEqual(self.seed_starts(), 1, "resume waits on the same task")

    # ---------- job state cleanup ----------

    def test_job_state_cleanup_is_verified(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        cloud = self.h.cloud
        # A refused listing is never taken for a missing map.
        cloud.refused["map_keys"] = ""
        response = self.cleanup_apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("mc-ab12cd-jobs", response["error"]["message"])
        self.assertIn("cleanup again", response["error"]["actionable_guidance"])
        self.assertIn("dict", [r.resource_type for r in self.h.journal().resources])
        self.assertTrue(cloud.maps["mc-ab12cd-jobs"], "the job state is still there")
        # Deletes that do not take leave keys behind: still a failure, never success.
        del cloud.refused["map_keys"]
        cloud.refused["map_delete"] = ""
        response = self.cleanup_apply()
        self.assertEqual(response["error"]["code"], "ERR_EXECUTION_FAILED")
        self.assertIn("keys remain", response["error"]["message"])
        self.assertIn("dict", [r.resource_type for r in self.h.journal().resources])
        del cloud.refused["map_delete"]
        response = self.cleanup_apply()
        self.assertTrue(response["success"], response)
        self.assertIn({"type": "dict", "name": "mc-ab12cd-jobs"}, response["data"]["deleted"])
        self.assert_cloud_empty()

    def test_an_empty_job_state_map_is_missing(self) -> None:
        self.assertTrue(self.h.apply()["success"])
        self.h.cloud.maps["mc-ab12cd-jobs"].clear()  # every key expired
        response = self.cleanup_apply()
        self.assertTrue(response["success"], response)
        self.assertIn({"type": "dict", "name": "mc-ab12cd-jobs"}, response["data"]["missing"])
        self.assert_cloud_empty()


if __name__ == "__main__":
    unittest.main()
