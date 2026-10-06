"""Finding installations already in the account, and taking one over without a new download.

A second computer (or one whose app data was lost) has no journal for the installation
it set up before. `inspect` lists what the workspace holds, read-only, and an `apply`
that reuses the listed installation id adopts it: the same app, volume and dict, the
weights left where they are, a new proxy token for this computer.
"""

import unittest
from typing import Any, Dict, List

from provisioner.modal_driver import DISCOVERY_LIMIT, INSTALL_OPTIONS_KEY
from provisioner.test_drivers import Harness, final_states, parse_progress

from deploy.cloud.common.manifest import MODEL_PROD_FLUX, MODEL_PROD_FLUX_9B

MUTATING = {
    "Volume.objects.create", "Volume.objects.delete", "Dict.objects.create", "Dict.objects.delete", "Dict.put",
    "App.deploy", "Function.spawn", "proxy_tokens.create", "proxy_tokens.delete", "stop_app",
}
RECORDED = {"schema": 1, "gpu": "A10", "idle_seconds": 300, "model_id": MODEL_PROD_FLUX,
            "analysis_models": ["text_regions_rt@1"], "recorded_at": 1_760_000_100}


class ModalDiscoveryTest(unittest.TestCase):
    def setUp(self) -> None:
        self.h = Harness("modal")
        self.addCleanup(self.h.close)

    def other_computer(self) -> Harness:
        """A second desktop: its own empty journal directory, the same Modal account."""
        other = Harness("modal")
        self.addCleanup(other.close)
        other.cloud = self.h.cloud
        other.sdk_loader = self.h.cloud.sdk
        return other

    def inspect(self, h: Harness) -> Dict[str, Any]:
        response, _ = h.request("inspect", {"credentials": h.credentials})
        self.assertTrue(response["success"], response)
        return response["data"]

    def mutating_calls(self) -> List[str]:
        return [call for call, _ in self.h.cloud.calls if call in MUTATING]

    # ---------- discovery ----------

    def test_inspect_lists_installations_read_only(self) -> None:
        cloud = self.h.cloud
        cloud.seed_installation("mc-old123", MODEL_PROD_FLUX, analysis=["text_regions_rt@1"], options=RECORDED)
        cloud.apps["billing-dashboard"] = {"functions": set()}  # not ours
        cloud.apps["mc-novol1"] = {"functions": set()}  # ours by name, but no weights volume
        cloud.volumes.add("mc-stray9-weights")  # a volume without its app
        cloud.calls.clear()

        data = self.inspect(self.h)

        self.assertIs(data["existing_installations_complete"], True)
        self.assertEqual(len(data["existing_installations"]), 1)
        entry = data["existing_installations"][0]
        self.assertEqual(entry, {
            "installation_id": "mc-old123",
            "app_name": "mc-old123",
            "created_at": 1_760_000_000,
            "deployed_at": 1_770_500_000,
            "weights_checked": True,
            "models_ready": [MODEL_PROD_FLUX],
            "analysis_ready": ["text_regions_rt@1"],
            # Recorded before page denoise existed, so it reads as off.
            "options": {"gpu": "A10", "idle_seconds": 300, "model_id": MODEL_PROD_FLUX,
                        "analysis_models": ["text_regions_rt@1"], "denoise": False,
                        "routing_region": "us-east"},
            "on_this_computer": False,
        })
        self.assertEqual(self.mutating_calls(), [], "discovery changes nothing in the account")
        self.assertFalse((self.h.root / "installations").exists(), "discovery writes no journal")
        self.assertNotIn("as-good-token-secret", self.h.raw)
        self.assertEqual(final_states(parse_progress(self.h.progress_lines, "inspect")), {"inspect": "done"})

    def test_only_inspect_pays_for_discovery(self) -> None:
        self.h.cloud.seed_installation("mc-old123", MODEL_PROD_FLUX)
        self.h.plan("mc-ab12cd")
        self.assertEqual(self.h.cloud.count("list_deployed_apps"), 0)
        self.assertEqual(self.h.cloud.count("Volume.objects.list"), 0)

    def test_recorded_options_that_do_not_validate_are_left_out(self) -> None:
        cloud = self.h.cloud
        cloud.seed_installation("mc-bad001", MODEL_PROD_FLUX, options={**RECORDED, "gpu": "H100"}, created=1.0)
        cloud.seed_installation("mc-bad002", MODEL_PROD_FLUX, options="gpu=L4", created=2.0)
        cloud.seed_installation("mc-bad003", MODEL_PROD_FLUX, options={**RECORDED, "schema": 99}, created=3.0)
        cloud.seed_installation("mc-bad004", MODEL_PROD_FLUX, created=4.0)
        entries = self.inspect(self.h)["existing_installations"]
        self.assertEqual([e["installation_id"] for e in entries], ["mc-bad004", "mc-bad003", "mc-bad002", "mc-bad001"])
        self.assertTrue(all(e["options"] is None for e in entries))
        self.assertTrue(all(e["models_ready"] == [MODEL_PROD_FLUX] for e in entries))

    def test_the_list_is_bounded_newest_first(self) -> None:
        for index in range(DISCOVERY_LIMIT + 2):
            self.h.cloud.seed_installation(f"mc-many{index:02d}", MODEL_PROD_FLUX, created=1_760_000_000.0 + index)
        data = self.inspect(self.h)
        ids = [e["installation_id"] for e in data["existing_installations"]]
        self.assertEqual(len(ids), DISCOVERY_LIMIT)
        self.assertEqual(ids[0], f"mc-many{DISCOVERY_LIMIT + 1:02d}")
        self.assertIs(data["existing_installations_complete"], False)

    def test_a_failed_listing_still_answers_inspect(self) -> None:
        self.h.cloud.seed_installation("mc-old123", MODEL_PROD_FLUX)
        self.h.cloud.fail["list_deployed_apps"] = self.h.cloud.sdk().exception.Error("listing refused")
        data = self.inspect(self.h)
        self.assertEqual(data["existing_installations"], [])
        self.assertIs(data["existing_installations_complete"], False)
        self.assertEqual(data["workspace_name"], "studio-ws")

    def test_an_unreadable_volume_is_listed_as_unchecked(self) -> None:
        self.h.cloud.seed_installation("mc-old123", MODEL_PROD_FLUX)
        self.h.cloud.fail["Volume.listdir"] = self.h.cloud.sdk().exception.Error("no")
        entry = self.inspect(self.h)["existing_installations"][0]
        self.assertEqual((entry["weights_checked"], entry["models_ready"]), (False, []))

    def test_an_installation_this_computer_set_up_is_marked_for_resume(self) -> None:
        response = self.h.apply("mc-ab12cd", {"gpu": "A10"})
        self.assertTrue(response["success"], response)
        store = self.h.cloud.dicts["mc-ab12cd-jobs"]
        self.assertEqual(store[INSTALL_OPTIONS_KEY]["gpu"], "A10", "apply records its choices for discovery")
        entry = self.inspect(self.h)["existing_installations"][0]
        self.assertEqual(entry["installation_id"], "mc-ab12cd")
        self.assertIs(entry["on_this_computer"], True)
        self.assertEqual(entry["options"]["gpu"], "A10")
        self.assertEqual(entry["models_ready"], [MODEL_PROD_FLUX])
        # The same installation seen from a computer without the journal is not.
        self.assertIs(self.inspect(self.other_computer())["existing_installations"][0]["on_this_computer"], False)

    # ---------- adopting ----------

    def adopt(self, h: Harness, installation_id: str, options: Dict[str, Any]) -> Dict[str, Any]:
        return h.apply(installation_id, options)

    def test_adopting_reuses_everything_and_downloads_nothing(self) -> None:
        cloud = self.h.cloud
        first = self.h.apply("mc-ab12cd", {"gpu": "A10"})
        self.assertTrue(first["success"], first)
        old_token = first["data"]["runtime_credential"]["token_id"]
        spawns, deploys = cloud.count("Function.spawn"), cloud.count("App.deploy")
        volumes = set(cloud.volumes)

        second = self.other_computer()
        found = self.inspect(second)["existing_installations"][0]
        response = self.adopt(second, found["installation_id"], found["options"])

        self.assertTrue(response["success"], response)
        self.assertEqual(cloud.count("Function.spawn") - spawns, 0, "no seed job: the weights are not downloaded again")
        self.assertEqual(cloud.count("App.deploy") - deploys, 1, "the same app is redeployed once")
        self.assertEqual(cloud.volumes, volumes, "no second volume")
        self.assertEqual(set(cloud.dicts), {"mc-ab12cd-jobs"})
        new_token = response["data"]["runtime_credential"]["token_id"]
        self.assertNotEqual(new_token, old_token, "this computer gets its own proxy token")
        # The first computer's token is not this journal's to revoke: it stays live.
        self.assertEqual(set(cloud.proxy_tokens), {old_token, new_token})
        states = final_states(parse_progress(second.progress_lines, "apply"))
        self.assertEqual(states["weights"], "done")
        self.assertEqual(response["data"]["gpu"], "A10")
        cleanup, _ = second.request("cleanup_plan", {"installation_id": found["installation_id"]})
        self.assertIn("other computers", cleanup["data"]["notes"][0])

    def test_adopting_an_install_from_before_recorded_options(self) -> None:
        cloud = self.h.cloud
        cloud.seed_installation("mc-old123", MODEL_PROD_FLUX)
        response = self.adopt(self.h, "mc-old123", {})
        self.assertTrue(response["success"], response)
        self.assertEqual(cloud.count("Function.spawn"), 0)
        self.assertEqual(cloud.count("Volume.objects.create"), 1)
        self.assertEqual(sorted(cloud.volumes), ["mc-old123-weights"])
        self.assertEqual(cloud.dicts["mc-old123-jobs"][INSTALL_OPTIONS_KEY]["gpu"], "L4", "adopting records the choices")

    def test_adopting_with_a_new_analysis_model_seeds_only_what_is_missing(self) -> None:
        cloud = self.h.cloud
        cloud.seed_installation("mc-old123", MODEL_PROD_FLUX)
        response = self.adopt(self.h, "mc-old123", {"analysis_models": ["text_regions_rt@1"]})
        self.assertTrue(response["success"], response)
        # A "done" status document covers the FLUX snapshot only; the graph is missing,
        # so one seed job runs, and it skips the snapshot that is already verified.
        self.assertEqual(cloud.count("Function.spawn"), 1)
        self.assertIn("analysis/rtdetr-v2-full/ready.json", cloud.volume_files["mc-old123-weights"])

    def test_adopting_with_another_model_downloads_it_beside_the_first(self) -> None:
        cloud = self.h.cloud
        cloud.seed_installation("mc-old123", MODEL_PROD_FLUX)
        from deploy.cloud.common.manifest import PROD_SNAPSHOT_9B_TOTAL_BYTES
        from deploy.cloud.common.weights import seed_state
        cloud.seed_script = [seed_state("done", PROD_SNAPSHOT_9B_TOTAL_BYTES, now=9.0, model_id=MODEL_PROD_FLUX_9B)]
        response = self.adopt(self.h, "mc-old123", {"model_id": MODEL_PROD_FLUX_9B})
        self.assertTrue(response["success"], response)
        self.assertEqual(cloud.count("Function.spawn"), 1)
        entry = self.inspect(self.h)["existing_installations"][0]
        self.assertEqual(sorted(entry["models_ready"]), sorted([MODEL_PROD_FLUX, MODEL_PROD_FLUX_9B]))

    def test_a_marker_from_before_a_new_lora_seeds_again(self) -> None:
        # aizendazai46, 2026-10-02: the Qwen marker predated FukidashiErase, so the
        # volume looked ready, no seed ran, and the worker refused the snapshot.
        import json
        from deploy.cloud.common.manifest import FUKIDASHI_BYTES, MODEL_PROD_QWEN, PROD_SNAPSHOT_QWEN_TOTAL_BYTES
        from deploy.cloud.common.weights import expected_marker, seed_state, snapshot_name
        cloud = self.h.cloud
        cloud.seed_installation("mc-old123", MODEL_PROD_QWEN)
        marker = f"{snapshot_name(MODEL_PROD_QWEN)}.ready.json"
        stale = expected_marker(MODEL_PROD_QWEN)
        stale.update(file_count=stale["file_count"] - 1, total_bytes=stale["total_bytes"] - FUKIDASHI_BYTES)
        cloud.volume_contents["mc-old123-weights"][marker] = json.dumps(stale).encode()
        self.assertEqual(self.inspect(self.h)["existing_installations"][0]["models_ready"], [])
        cloud.seed_script = [seed_state("done", PROD_SNAPSHOT_QWEN_TOTAL_BYTES, now=9.0, model_id=MODEL_PROD_QWEN)]
        response = self.adopt(self.h, "mc-old123", {"model_id": MODEL_PROD_QWEN, "gpu": "L40S"})
        self.assertTrue(response["success"], response)
        self.assertEqual(cloud.count("Function.spawn"), 1, "the seed runs and fetches the missing LoRA")
        self.assertEqual(self.inspect(self.h)["existing_installations"][0]["models_ready"], [MODEL_PROD_QWEN])

    def test_legacy_qwen_marker_with_same_size_replacement_seeds_again(self) -> None:
        import json
        from deploy.cloud.common.manifest import MODEL_PROD_QWEN, PROD_SNAPSHOT_QWEN_TOTAL_BYTES
        from deploy.cloud.common.weights import expected_marker, seed_state, snapshot_name
        cloud = self.h.cloud
        cloud.seed_installation("mc-old123", MODEL_PROD_QWEN)
        marker = f"{snapshot_name(MODEL_PROD_QWEN)}.ready.json"
        legacy = expected_marker(MODEL_PROD_QWEN)
        legacy.pop("snapshot_fingerprint")
        cloud.volume_contents["mc-old123-weights"][marker] = json.dumps(legacy).encode()
        self.assertEqual(self.inspect(self.h)["existing_installations"][0]["models_ready"], [])
        cloud.seed_script = [seed_state("done", PROD_SNAPSHOT_QWEN_TOTAL_BYTES, now=9.0, model_id=MODEL_PROD_QWEN)]
        response = self.adopt(self.h, "mc-old123", {"model_id": MODEL_PROD_QWEN, "gpu": "L40S"})
        self.assertTrue(response["success"], response)
        self.assertEqual(cloud.count("Function.spawn"), 1)
        self.assertEqual(self.inspect(self.h)["existing_installations"][0]["models_ready"], [MODEL_PROD_QWEN])

    def test_this_computer_still_cannot_apply_over_its_own_finished_install(self) -> None:
        self.assertTrue(self.h.apply("mc-ab12cd")["success"])
        again = self.h.apply("mc-ab12cd")
        self.assertEqual(again["error"]["code"], "ERR_VALIDATION_ERROR")
        self.assertIn("already in stage", again["error"]["message"])


class BeamDiscoveryTest(unittest.TestCase):
    def test_beam_lists_nothing_and_says_it_did_not_look(self) -> None:
        h = Harness("beam")
        self.addCleanup(h.close)
        response, _ = h.request("inspect", {"credentials": h.credentials})
        self.assertTrue(response["success"], response)
        self.assertEqual(response["data"]["existing_installations"], [])
        self.assertIs(response["data"]["existing_installations_complete"], False)
        self.assertEqual(h.cloud.calls and [c for c, _ in h.cloud.calls if "list" in c.lower()], [])


if __name__ == "__main__":
    unittest.main()
