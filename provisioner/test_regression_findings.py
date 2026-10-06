"""Regression tests covering the P7/P8 review findings.

Validates:
1. Cross-provider journal collision protection.
2. Installation ID mismatch and tampered stage/hash/resource shape fail closed.
3. Apply and resume plan drift is refused before any provider change.
4. Mandatory confirm_delete_persistent_storage=true before volume deletion.
5. Non-empty HTTPS endpoint_url guard, for probes and for discovered endpoints.
6. Legal state machine transitions (forbids arbitrary jumps, allows resume/cleanup).
7. Symlink hazards on journal file and directory fail closed with ERR_SECURITY_VIOLATION.
8. An interrupted cleanup is finished by another cleanup, never resumed.
9. Apply against completed or cleaned_up journals changes nothing.

The controller-level cases run the real drivers against the fake SDKs of test_drivers.
"""

import json
import os
from pathlib import Path
import tempfile
import unittest

from provisioner.journal import (
    STAGE_CLEANED_UP,
    STAGE_CLEANUP_PLANNED,
    STAGE_COMPLETED,
    STAGE_DEPLOYING,
    STAGE_PLANNED,
    STAGE_SEEDED,
    STAGE_SEEDING,
    InstallationJournal,
)
from provisioner.protocol import (
    ERR_EXECUTION_FAILED,
    ERR_SECURITY_VIOLATION,
    ERR_UNAPPROVED_PLAN,
    ERR_VALIDATION,
    PROVIDER_BEAM,
    PROVIDER_MODAL,
    ProtocolError,
)
from provisioner.redaction import GLOBAL_REGISTRY
from provisioner.test_drivers import Harness

INSTALLATION = "mc-ab12cd"
MODAL_MUTATIONS = {"Volume.objects.create", "Dict.objects.create", "App.deploy", "Function.spawn", "proxy_tokens.create"}
BEAM_MUTATIONS = {"get_or_create_volume", "create_secret", "update_secret", "deploy", "http.post"}


def mutations(harness: Harness) -> list:
    wanted = MODAL_MUTATIONS if harness.provider == PROVIDER_MODAL else BEAM_MUTATIONS
    return [name for name, _ in harness.cloud.calls if name in wanted]


class JournalFindings(unittest.TestCase):
    """Findings 1, 2, 6 and 7: the journal itself fails closed."""

    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root_path = Path(self.temp_dir.name)

    def tearDown(self):
        self.temp_dir.cleanup()

    def tampered(self, installation_id: str, change) -> ProtocolError:
        journal = InstallationJournal(self.root_path, installation_id, PROVIDER_MODAL)
        journal.load_or_initialize("c" * 64, "c" * 64, f"mc-app-{installation_id}")
        journal.record_resource("vol-1", "volume", f"mc-app-{installation_id}-weights", STAGE_SEEDING)
        data = json.loads(journal.journal_file.read_text(encoding="utf-8"))
        change(data)
        journal.journal_file.write_text(json.dumps(data), encoding="utf-8")
        with self.assertRaises(ProtocolError) as ctx:
            journal.load()
        return ctx.exception

    def test_cross_provider_journal_collision_direct_load(self):
        InstallationJournal(self.root_path, "inst-x-prov", PROVIDER_MODAL).load_or_initialize("a" * 64, "a" * 64, "mc-app-x")
        with self.assertRaises(ProtocolError) as ctx:
            InstallationJournal(self.root_path, "inst-x-prov", PROVIDER_BEAM).load()
        self.assertEqual(ctx.exception.code, ERR_VALIDATION)
        self.assertIn("Journal provider mismatch", ctx.exception.message)

    def test_tampered_journals_fail_closed(self):
        cases = {
            "installation_id mismatch": lambda d: d.update(installation_id="tampered-other-id"),
            "Invalid or tampered stage": lambda d: d.update(stage="unauthorized_super_stage"),
            "plan_hash": lambda d: d.update(plan_hash="tampered_non_hex_hash_value"),
            "approved_plan_hash": lambda d: d.update(approved_plan_hash="short_hash"),
            "managed_by tag": lambda d: d["resources"][0]["ownership_tags"].update(managed_by="attacker"),
        }
        for index, (expected, change) in enumerate(cases.items()):
            with self.subTest(expected):
                error = self.tampered(f"inst-tampered-{index}", change)
                self.assertEqual(error.code, ERR_VALIDATION)
                self.assertIn(expected, error.message)

    def test_arbitrary_and_backwards_state_transitions_rejected(self):
        journal = InstallationJournal(self.root_path, "inst-state-rules", PROVIDER_MODAL)
        journal.load_or_initialize("a" * 64, "a" * 64, "mc-app-state")
        self.assertEqual(journal.record.stage, STAGE_PLANNED)
        with self.assertRaises(ProtocolError) as ctx:
            journal.transition_to(STAGE_COMPLETED)
        self.assertIn("Illegal state transition", ctx.exception.message)
        journal.transition_to(STAGE_SEEDING)
        journal.transition_to(STAGE_SEEDED)
        with self.assertRaises(ProtocolError):
            journal.transition_to(STAGE_PLANNED)
        journal.transition_to(STAGE_CLEANUP_PLANNED)
        journal.transition_to(STAGE_CLEANED_UP)
        for target in (STAGE_PLANNED, STAGE_SEEDING, STAGE_DEPLOYING, STAGE_COMPLETED):
            with self.assertRaises(ProtocolError) as ctx:
                journal.transition_to(target)
            self.assertEqual(ctx.exception.code, ERR_VALIDATION)

    def test_symlink_journal_file_hazard_fails_closed(self):
        inst_dir = self.root_path / "installations" / "inst-symlink-hazard"
        inst_dir.mkdir(parents=True)
        target = self.root_path / "secret_file.txt"
        target.write_text("sensitive data", encoding="utf-8")
        os.symlink(str(target), str(inst_dir / "journal.json"))
        journal = InstallationJournal(self.root_path, "inst-symlink-hazard", PROVIDER_MODAL)
        for action in (journal.exists, journal.load):
            with self.assertRaises(ProtocolError) as ctx:
                action()
            self.assertEqual(ctx.exception.code, ERR_SECURITY_VIOLATION)

    def test_symlink_installation_dir_and_journal_file_save_fails_closed(self):
        installations = self.root_path / "installations"
        installations.mkdir(parents=True)
        target_dir = self.root_path / "target_dir"
        target_dir.mkdir()
        os.symlink(str(target_dir), str(installations / "inst-symlink-dir-save"))
        with self.assertRaises(ProtocolError) as ctx:
            InstallationJournal(self.root_path, "inst-symlink-dir-save", PROVIDER_MODAL).load_or_initialize(
                "a" * 64, "a" * 64, "mc-app-symdir"
            )
        self.assertEqual(ctx.exception.code, ERR_SECURITY_VIOLATION)
        self.assertIn("Journal directory cannot be a symbolic link", ctx.exception.message)

        inst_dir = installations / "inst-symlink-file-save"
        inst_dir.mkdir()
        target_file = self.root_path / "target_file.json"
        target_file.write_text("{}", encoding="utf-8")
        os.symlink(str(target_file), str(inst_dir / "journal.json"))
        with self.assertRaises(ProtocolError) as ctx:
            InstallationJournal(self.root_path, "inst-symlink-file-save", PROVIDER_MODAL).save()
        self.assertEqual(ctx.exception.code, ERR_SECURITY_VIOLATION)
        self.assertIn("Journal file cannot be a symbolic link", ctx.exception.message)
        self.assertEqual(target_file.read_text(encoding="utf-8"), "{}")


class ControllerFindings(unittest.TestCase):
    """Findings 1, 3, 4, 5, 8 and 9 through the controller and the real Modal driver."""

    def setUp(self):
        GLOBAL_REGISTRY.clear()
        self.h = Harness(PROVIDER_MODAL)
        self.addCleanup(self.h.close)
        self.addCleanup(GLOBAL_REGISTRY.clear)

    def apply_request(self, plan_hash: str):
        response, _ = self.h.request(
            "apply",
            {"credentials": self.h.credentials, "installation_id": INSTALLATION, "approved_plan_hash": plan_hash},
        )
        return response

    def test_cross_provider_journal_collision_controller_apply(self):
        self.assertTrue(self.h.apply()["success"])
        beam = Harness(PROVIDER_BEAM)
        self.addCleanup(beam.close)
        beam.root = self.h.root
        response = beam.apply()
        self.assertEqual(response["error"]["code"], ERR_VALIDATION)
        self.assertIn("provider mismatch", response["error"]["message"].lower())
        self.assertEqual(mutations(beam), [])

    def test_apply_with_existing_journal_plan_drift_refused_before_mutation(self):
        InstallationJournal(self.h.root, INSTALLATION, PROVIDER_MODAL).load_or_initialize("1" * 64, "", "mc-ab12cd")
        response = self.apply_request(self.h.plan()["plan_hash"])
        self.assertEqual(response["error"]["code"], ERR_UNAPPROVED_PLAN)
        self.assertIn("Plan hash mismatch with existing journal", response["error"]["message"])
        self.assertEqual(mutations(self.h), [])

    def test_resume_without_approved_hash_fails_closed_before_mutation(self):
        plan = self.h.plan()
        InstallationJournal(self.h.root, INSTALLATION, PROVIDER_MODAL).load_or_initialize(plan["plan_hash"], "", "mc-ab12cd")
        response = self.h.resume()
        self.assertEqual(response["error"]["code"], ERR_UNAPPROVED_PLAN)
        self.assertIn("no approved plan hash recorded", response["error"]["message"])
        self.assertEqual(mutations(self.h), [])

    def test_resume_plan_drift_fails_before_mutation(self):
        self.h.cloud.fail["App.deploy"] = RuntimeError("image build failed")
        self.assertEqual(self.h.apply()["error"]["code"], ERR_EXECUTION_FAILED)
        del self.h.cloud.fail["App.deploy"]
        before = mutations(self.h)

        journal = InstallationJournal(self.h.root, INSTALLATION, PROVIDER_MODAL)
        record = journal.load()
        record.plan_hash = record.approved_plan_hash = "9" * 64
        # Another account's journal: the hash alone cannot be matched to these credentials.
        record.provider_state["account_id"] = "someone-else"
        journal.save()
        response = self.h.resume()
        self.assertEqual(response["error"]["code"], ERR_UNAPPROVED_PLAN)
        self.assertIn("Plan drift detected on resume", response["error"]["message"])
        self.assertEqual(mutations(self.h), before)
        self.assertEqual(self.h.cloud.apps, {})

    def test_cleanup_apply_requires_confirm_delete_persistent_storage(self):
        self.assertTrue(self.h.apply()["success"])
        plan, _ = self.h.request("cleanup_plan", {"installation_id": INSTALLATION})
        params = {
            "installation_id": INSTALLATION,
            "approved_cleanup_plan_hash": plan["data"]["plan_hash"],
            "credentials": self.h.credentials,
        }
        for confirm in (None, False, "true", 1):
            with self.subTest(confirm=confirm):
                request = dict(params) if confirm is None else dict(params, confirm_delete_persistent_storage=confirm)
                response, _ = self.h.request("cleanup_apply", request)
                self.assertEqual(response["error"]["code"], ERR_VALIDATION)
                self.assertIn("confirm_delete_persistent_storage=true", response["error"]["message"])
                self.assertEqual(self.h.cloud.volumes, {"mc-ab12cd-weights"})
        response, _ = self.h.request("cleanup_apply", dict(params, confirm_delete_persistent_storage=True))
        self.assertTrue(response["success"], response)
        self.assertEqual(self.h.cloud.volumes, set())

    def test_probe_compatibility_refuses_non_https_and_empty_endpoints(self):
        credential = {"kind": "modal_proxy", "token_id": "wk-abcdefgh", "token_secret": "ws-abcdefghij"}
        for endpoint in (
            "",
            "   ",
            "http://insecure.example.com/mc/v1",
            "ftp://ftp.example.com/mc/v1",
            "just_a_string",
            "https://",
            "https://[::1/mc/v1",
            None,
        ):
            with self.subTest(endpoint=endpoint):
                response, _ = self.h.request(
                    "probe_compatibility", {"endpoint_url": endpoint, "runtime_credential": credential}
                )
                self.assertIn(response["error"]["code"], (ERR_VALIDATION, ERR_SECURITY_VIOLATION))
        self.assertEqual(self.h.cloud.calls, [])

    def test_pipeline_refuses_insecure_endpoint_discovery(self):
        self.h.cloud.web_url = lambda app_name: "http://studio-ws--mc-ab12cd-gateway.modal.run"
        response = self.h.apply()
        self.assertEqual(response["error"]["code"], ERR_VALIDATION)
        self.assertIn("https", response["error"]["message"])
        self.assertNotEqual(self.h.journal().stage, STAGE_COMPLETED)

    def test_interrupted_cleanup_is_finished_by_another_cleanup(self):
        self.assertTrue(self.h.apply()["success"])
        self.h.cloud.fail["Volume.objects.delete"] = RuntimeError("provider communication failure during teardown")
        plan, _ = self.h.request("cleanup_plan", {"installation_id": INSTALLATION})
        params = {"installation_id": INSTALLATION, "credentials": self.h.credentials, "confirm_delete_persistent_storage": True}
        response, _ = self.h.request("cleanup_apply", dict(params, approved_cleanup_plan_hash=plan["data"]["plan_hash"]))
        self.assertEqual(response["error"]["code"], ERR_EXECUTION_FAILED)
        self.assertEqual(self.h.journal().stage, STAGE_CLEANUP_PLANNED)
        self.assertIn("volume", [r.resource_type for r in self.h.journal().resources])
        self.assertEqual(self.h.resume()["error"]["code"], ERR_VALIDATION, "a started cleanup is never resumed")

        del self.h.cloud.fail["Volume.objects.delete"]
        plan, _ = self.h.request("cleanup_plan", {"installation_id": INSTALLATION})
        response, _ = self.h.request("cleanup_apply", dict(params, approved_cleanup_plan_hash=plan["data"]["plan_hash"]))
        self.assertTrue(response["success"], response)
        self.assertEqual(self.h.journal().stage, STAGE_CLEANED_UP)
        self.assertEqual(self.h.cloud.volumes, set())

    def test_apply_against_completed_journal_changes_nothing(self):
        plan_hash = self.h.plan()["plan_hash"]
        self.assertTrue(self.apply_request(plan_hash)["success"])
        before = (self.h.journal_text(), mutations(self.h))
        response = self.apply_request(plan_hash)
        self.assertEqual(response["error"]["code"], ERR_VALIDATION)
        self.assertIn("already in stage 'completed'", response["error"]["message"])
        self.assertEqual((self.h.journal_text(), mutations(self.h)), before)

    def test_apply_against_cleaned_up_journal_changes_nothing(self):
        plan_hash = self.h.plan()["plan_hash"]
        journal = InstallationJournal(self.h.root, INSTALLATION, PROVIDER_MODAL)
        journal.load_or_initialize(plan_hash, plan_hash, "mc-ab12cd")
        journal.transition_to(STAGE_CLEANUP_PLANNED)
        journal.transition_to(STAGE_CLEANED_UP)
        before = self.h.journal_text()
        response = self.apply_request(plan_hash)
        self.assertEqual(response["error"]["code"], ERR_VALIDATION)
        self.assertIn("already in stage 'cleaned_up'", response["error"]["message"])
        self.assertEqual(self.h.journal_text(), before)
        self.assertEqual(mutations(self.h), [])


if __name__ == "__main__":
    unittest.main()
