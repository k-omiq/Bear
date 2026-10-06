"""Unit tests for the resumable installation and resource journal."""

import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock

from provisioner.journal import (
    MAX_JOURNAL_BYTES,
    STAGE_CLEANED_UP,
    STAGE_CLEANUP_PLANNED,
    STAGE_COMPLETED,
    STAGE_CREDENTIAL_CREATED,
    STAGE_DEPLOYED,
    STAGE_DEPLOYING,
    STAGE_DISCOVERED,
    STAGE_FAILED,
    STAGE_PLANNED,
    STAGE_SEEDED,
    STAGE_SEEDING,
    STAGE_VALIDATED,
    InstallationJournal,
    InstallationRecord,
    ResourceRecord,
    transition_allowed,
)
from provisioner.protocol import ERR_SECURITY_VIOLATION, ERR_VALIDATION, ProtocolError


class TestInstallationJournal(unittest.TestCase):
    """Tests for installation journal durability, stage tracking, and duplicate prevention."""

    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root_path = Path(self.temp_dir.name)

    def tearDown(self):
        self.temp_dir.cleanup()

    def test_journal_initialization_and_atomic_save(self):
        journal = InstallationJournal(self.root_path, "inst-001", "modal")
        self.assertFalse(journal.exists())

        rec = journal.load_or_initialize(
            plan_hash="a" * 64,
            approved_plan_hash="a" * 64,
            app_name="mc-app-inst-001",
        )
        self.assertTrue(journal.exists())
        self.assertEqual(rec.stage, STAGE_PLANNED)
        self.assertEqual(rec.installation_id, "inst-001")
        self.assertEqual(rec.provider, "modal")

        # Reload from fresh instance
        reloaded_journal = InstallationJournal(self.root_path, "inst-001", "modal")
        loaded_rec = reloaded_journal.load()
        self.assertEqual(loaded_rec.installation_id, "inst-001")
        self.assertEqual(loaded_rec.stage, STAGE_PLANNED)
        self.assertEqual(loaded_rec.app_name, "mc-app-inst-001")

        # Only the user can read it, and a write that fails leaves the old journal: the
        # new one goes to a temporary file that replaces the journal only when complete.
        path = journal.journal_file
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        before = path.read_bytes()
        journal.record.last_error = "a newer error"
        closed = []
        real_close = os.close

        def close(fd):
            closed.append(fd)
            real_close(fd)

        with mock.patch("provisioner.journal.os.fsync", side_effect=OSError("disk full")), mock.patch(
            "provisioner.journal.os.close", side_effect=close
        ):
            with self.assertRaises(OSError):
                journal.save()
        self.assertEqual(path.read_bytes(), before)
        self.assertEqual([p.name for p in path.parent.iterdir()], ["journal.json"], "no temporary file is left")
        self.assertEqual(closed, [], "the file object closes its descriptor; nothing closes it again")

    def test_stage_transitions(self):
        journal = InstallationJournal(self.root_path, "inst-stages", "beam")
        journal.load_or_initialize("b" * 64, "b" * 64, "mc-beam-svc")

        # The IC-2 step order: deploy, then weights, token, endpoint, health.
        stages = [
            STAGE_DEPLOYING,
            STAGE_DEPLOYED,
            STAGE_SEEDING,
            STAGE_SEEDED,
            STAGE_CREDENTIAL_CREATED,
            STAGE_DISCOVERED,
            STAGE_VALIDATED,
            STAGE_COMPLETED,
        ]
        for st in stages:
            journal.transition_to(st)
            self.assertEqual(journal.record.stage, st)
            # Re-read from disk to prove durability
            reloaded = InstallationJournal(self.root_path, "inst-stages", "beam").load()
            self.assertEqual(reloaded.stage, st)

    def test_duplicate_resource_prevention(self):
        journal = InstallationJournal(self.root_path, "inst-dup", "modal")
        journal.load_or_initialize("c" * 64, "c" * 64, "mc-app-dup")

        # First recording
        r1 = journal.record_resource(
            resource_id="vol-100",
            resource_type="volume",
            name="mc-flux-vol-inst-dup",
            stage_created=STAGE_SEEDING,
        )
        self.assertEqual(len(journal.record.resources), 1)

        # Second recording with identical resource_id
        r2 = journal.record_resource(
            resource_id="vol-100",
            resource_type="volume",
            name="mc-flux-vol-inst-dup",
            stage_created=STAGE_SEEDING,
        )
        self.assertEqual(len(journal.record.resources), 1)
        self.assertEqual(r1.resource_id, r2.resource_id)

        # Third recording with same type and name but different id -> must not duplicate
        r3 = journal.record_resource(
            resource_id="vol-101",
            resource_type="volume",
            name="mc-flux-vol-inst-dup",
            stage_created=STAGE_SEEDING,
        )
        self.assertEqual(len(journal.record.resources), 1)
        self.assertEqual(r3.resource_id, "vol-100")

    def test_zero_raw_credentials_stored_on_disk(self):
        """Invariant: raw credentials must NEVER be persisted in the journal JSON."""
        journal = InstallationJournal(self.root_path, "inst-no-secret", "modal")
        rec = journal.load_or_initialize("d" * 64, "d" * 64, "mc-app-sec")

        # Simulate recording runtime credential reference
        rec.runtime_credential_ref = {
            "resource_id": "tok-1",
            "token_type": "proxy_token",
            "name": "tok-proxy-inst",
            # If an untrusted caller attempted to pass secret data:
            "secret_key": "as-SUPER-SECRET-MODAL-KEY",
        }
        journal.save()

        # Inspect raw bytes on disk
        raw_text = journal.journal_file.read_text(encoding="utf-8")
        self.assertNotIn("as-SUPER-SECRET-MODAL-KEY", raw_text)
        self.assertIn("[REDACTED_CREDENTIAL]", raw_text)

    def test_forget_setup_credential_tracking(self):
        journal = InstallationJournal(self.root_path, "inst-forget", "modal")
        journal.load_or_initialize("e" * 64, "e" * 64, "mc-app-forget")

        self.assertFalse(journal.record.setup_credential_forgotten)
        journal.forget_setup_credential()
        self.assertTrue(journal.record.setup_credential_forgotten)

        # Re-read from disk
        reloaded = InstallationJournal(self.root_path, "inst-forget", "modal").load()
        self.assertTrue(reloaded.setup_credential_forgotten)

    def test_resume_state_steps_options_and_provider_state(self):
        journal = InstallationJournal(self.root_path, "inst-resume-logic", "modal")
        journal.initialize("f" * 64, "f" * 64, "mc-app-resume", {"gpu": "L4", "idle_seconds": 120})
        self.assertTrue(journal.can_resume())
        self.assertFalse(journal.step_done("volume"))

        journal.mark_step_done("volume")
        journal.mark_step_done("volume")
        journal.set_state(environment_name="main", seed_call_id="fc-1")
        journal.set_state(seed_call_id=None)

        reloaded = InstallationJournal(self.root_path, "inst-resume-logic", "modal").load()
        self.assertEqual(reloaded.completed_steps, ["volume"])
        self.assertEqual(reloaded.provider_state, {"environment_name": "main"})
        self.assertEqual(reloaded.options, {"gpu": "L4", "idle_seconds": 120})

        with self.assertRaises(ProtocolError):
            journal.mark_step_done("not-a-step")
        with self.assertRaises(ProtocolError):
            journal.set_state(token_secret="ws-never")  # sensitive names are refused
        with self.assertRaises(ProtocolError):
            journal.set_state(seed_url="line\nbreak")

    def test_initialize_refuses_to_replace_a_journal(self):
        journal = InstallationJournal(self.root_path, "inst-once", "beam")
        journal.initialize("f" * 64, "f" * 64, "mc-once")
        with self.assertRaises(ProtocolError):
            InstallationJournal(self.root_path, "inst-once", "beam").initialize("e" * 64, "e" * 64, "mc-once")

    def test_transition_rules(self):
        self.assertTrue(transition_allowed(STAGE_PLANNED, STAGE_DEPLOYING))
        self.assertFalse(transition_allowed(STAGE_SEEDED, STAGE_DEPLOYING))
        self.assertTrue(transition_allowed(STAGE_SEEDING, STAGE_FAILED))
        self.assertTrue(transition_allowed(STAGE_FAILED, STAGE_DEPLOYING))
        # resume at completed may redeploy and issues a fresh credential; nothing
        # else goes back
        self.assertTrue(transition_allowed(STAGE_COMPLETED, STAGE_CREDENTIAL_CREATED))
        self.assertTrue(transition_allowed(STAGE_COMPLETED, STAGE_DEPLOYING))
        self.assertFalse(transition_allowed(STAGE_COMPLETED, STAGE_SEEDING))
        self.assertTrue(transition_allowed(STAGE_COMPLETED, STAGE_CLEANUP_PLANNED))
        # a started cleanup only finishes
        self.assertFalse(transition_allowed(STAGE_CLEANUP_PLANNED, STAGE_FAILED))
        self.assertFalse(transition_allowed(STAGE_CLEANUP_PLANNED, STAGE_DEPLOYING))
        self.assertTrue(transition_allowed(STAGE_CLEANUP_PLANNED, STAGE_CLEANED_UP))
        self.assertFalse(transition_allowed(STAGE_CLEANED_UP, STAGE_FAILED))
        self.assertFalse(transition_allowed(STAGE_VALIDATED, STAGE_CLEANED_UP))
        # completed only after the health check
        self.assertTrue(transition_allowed(STAGE_VALIDATED, STAGE_COMPLETED))
        self.assertFalse(transition_allowed(STAGE_SEEDED, STAGE_COMPLETED))
        self.assertFalse(transition_allowed(STAGE_FAILED, STAGE_COMPLETED))

    def test_advance_leaves_a_later_stage_alone(self):
        journal = InstallationJournal(self.root_path, "inst-advance", "beam")
        journal.initialize("a" * 64, "a" * 64, "mc-advance")
        journal.advance(STAGE_SEEDED)
        journal.advance(STAGE_DEPLOYING)  # a repeated earlier step does not move back
        self.assertEqual(journal.record.stage, STAGE_SEEDED)
        journal.advance(STAGE_VALIDATED)
        journal.transition_to(STAGE_COMPLETED)
        journal.advance(STAGE_CREDENTIAL_CREATED)
        self.assertEqual(journal.record.stage, STAGE_CREDENTIAL_CREATED)
        journal.transition_to(STAGE_CLEANUP_PLANNED)
        self.assertFalse(journal.can_resume())

    def test_remove_resource_clears_the_credential_reference(self):
        journal = InstallationJournal(self.root_path, "inst-remove", "modal")
        journal.initialize("a" * 64, "a" * 64, "mc-remove")
        journal.record_resource("wk-abc123", "proxy_token", "mc-remove-proxy-token", STAGE_CREDENTIAL_CREATED)
        journal.record.runtime_credential_ref = {"resource_type": "proxy_token", "name": "mc-remove-proxy-token"}
        journal.save()
        journal.remove_resource("proxy_token", "mc-remove-proxy-token")
        reloaded = InstallationJournal(self.root_path, "inst-remove", "modal").load()
        self.assertEqual((reloaded.resources, reloaded.runtime_credential_ref), ([], None))

    def test_tampered_new_fields_fail_closed(self):
        journal = InstallationJournal(self.root_path, "inst-tamper", "modal")
        journal.initialize("a" * 64, "a" * 64, "mc-tamper", {"gpu": "L4", "idle_seconds": 120})
        good = json.loads(journal.journal_file.read_text(encoding="utf-8"))
        for field, value in (
            ("options", {"gpu": "L4; rm -rf", "idle_seconds": 120}),
            ("options", {"gpu": "L4", "idle_seconds": True}),
            ("options", {"extra": 1}),
            ("completed_steps", ["volume", "volume"]),
            ("completed_steps", ["launch"]),
            ("provider_state", {"Bad-Key": "x"}),
            ("provider_state", {"note": ["list"]}),
        ):
            data = dict(good, **{field: value})
            journal.journal_file.write_text(json.dumps(data), encoding="utf-8")
            with self.assertRaises(ProtocolError, msg=f"{field}={value!r}"):
                InstallationJournal(self.root_path, "inst-tamper", "modal").load()

    def test_oversized_journal_rejected(self):
        journal = InstallationJournal(self.root_path, "inst-huge", "modal")
        journal.load_or_initialize("0" * 64, "0" * 64, "mc-app-huge")

        # Write oversized file to journal path
        with open(journal.journal_file, "wb") as f:
            f.seek(MAX_JOURNAL_BYTES + 2048)
            f.write(b"\0")

        with self.assertRaises(ProtocolError) as ctx:
            journal.load()
        self.assertEqual(ctx.exception.code, ERR_SECURITY_VIOLATION)

    def test_names_that_look_like_tokens_round_trip(self):
        """Ids, names and the endpoint URL survive the save exactly; the error text is still scrubbed."""
        journal = InstallationJournal(self.root_path, "inst-names", "modal")
        journal.initialize("a" * 64, "a" * 64, "mc-names")
        journal.set_state(account_id="ws-0001-workspace", environment_name="as-staging-environment")
        journal.record.endpoint_url = "https://studio-ws--mc-names-gateway.modal.run/mc/v1"
        journal.transition_to(STAGE_FAILED, error="deploy failed for token ws-abcdefghijkl")
        reloaded = InstallationJournal(self.root_path, "inst-names", "modal").load()
        self.assertEqual(reloaded.provider_state["account_id"], "ws-0001-workspace")
        self.assertEqual(reloaded.provider_state["environment_name"], "as-staging-environment")
        self.assertEqual(reloaded.endpoint_url, "https://studio-ws--mc-names-gateway.modal.run/mc/v1")
        self.assertEqual(reloaded.last_error, "deploy failed for token ws-[REDACTED]")

    def test_reject_unsafe_installation_id(self):
        for bad_id in ["../escape", "null\0byte", "slash/id", ""]:
            with self.assertRaises((ProtocolError, Exception)):
                InstallationJournal(self.root_path, bad_id, "modal")

    def test_symlink_hazards_fail_closed_on_save(self):
        """Proves InstallationJournal.save fails closed on symlinked installation_dir or journal_file."""
        import os
        # 1. Symlinked installation dir
        target_dir = self.root_path / "real_dir"
        target_dir.mkdir(parents=True, exist_ok=True)
        inst_dir = self.root_path / "installations" / "inst-symdir"
        inst_dir.parent.mkdir(parents=True, exist_ok=True)
        os.symlink(str(target_dir), str(inst_dir))

        j1 = InstallationJournal(self.root_path, "inst-symdir", "modal")
        with self.assertRaises(ProtocolError) as ctx:
            j1.save()
        self.assertEqual(ctx.exception.code, ERR_SECURITY_VIOLATION)

        # 2. Symlinked journal file
        inst_dir2 = self.root_path / "installations" / "inst-symfile"
        inst_dir2.mkdir(parents=True, exist_ok=True)
        target_file = self.root_path / "dummy.txt"
        target_file.write_text("content", encoding="utf-8")
        os.symlink(str(target_file), str(inst_dir2 / "journal.json"))

        j2 = InstallationJournal(self.root_path, "inst-symfile", "modal")
        with self.assertRaises(ProtocolError) as ctx:
            j2.save()
        self.assertEqual(ctx.exception.code, ERR_SECURITY_VIOLATION)


if __name__ == "__main__":
    unittest.main()
