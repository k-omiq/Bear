"""Comprehensive unit tests for credential and secret redaction."""

import json
import unittest

from provisioner.protocol import make_success_response
from provisioner.redaction import (
    GLOBAL_REGISTRY,
    REDACTED_CREDENTIAL,
    RedactionRegistry,
    is_sensitive_key,
    redact_data,
    redact_string,
)


class TestCredentialRedaction(unittest.TestCase):
    """Tests for regex-based and key-based credential redaction."""

    def setUp(self):
        GLOBAL_REGISTRY.clear()

    def tearDown(self):
        GLOBAL_REGISTRY.clear()

    def test_modal_token_regex_redaction(self):
        # Modal token secret: as- followed by alphanumeric
        sample = "Error creating client with token as-1234567890abcdef and id ak-9876543210zyxwvu"
        redacted = redact_string(sample)
        self.assertNotIn("as-1234567890abcdef", redacted)
        self.assertNotIn("ak-9876543210zyxwvu", redacted)
        self.assertIn("as-[REDACTED]", redacted)
        self.assertIn("ak-[REDACTED]", redacted)

    def test_beam_token_regex_redaction(self):
        sample = "Connecting with Beam token b9_9876543210fedcba and beam_1234567890abcdef"
        redacted = redact_string(sample)
        self.assertNotIn("b9_9876543210fedcba", redacted)
        self.assertNotIn("beam_1234567890abcdef", redacted)
        self.assertIn("b9_[REDACTED]", redacted)
        self.assertIn("beam_[REDACTED]", redacted)

    def test_bearer_authorization_redaction(self):
        sample = "Headers: Authorization: Bearer super_secret_jwt_token_123456"
        redacted = redact_string(sample)
        self.assertNotIn("super_secret_jwt_token_123456", redacted)
        self.assertIn("Bearer [REDACTED]", redacted)

    def test_quoted_json_key_value_redaction(self):
        self.assertEqual(redact_string('{"secret": "hunter2hunter2"}'), '{"secret": "[REDACTED]"}')
        self.assertEqual(redact_string("{'api_key':'abcdef123456'}"), "{'api_key':'[REDACTED]'}")
        self.assertEqual(redact_string("password = hunter2hunter2"), "password = [REDACTED]")
        # Names that only start with a secret word stay readable.
        self.assertEqual(redact_string('{"secret_name": "MC_MC_AB12CD_TOKEN"}'), '{"secret_name": "MC_MC_AB12CD_TOKEN"}')

    def test_query_param_token_redaction(self):
        url = "https://gateway.beam.cloud/v1/jobs/123/result?token=secret_query_val_999&format=png"
        redacted = redact_string(url)
        self.assertNotIn("secret_query_val_999", redacted)
        self.assertIn("token=[REDACTED]", redacted)
        self.assertIn("format=png", redacted)

    def test_sensitive_key_detection(self):
        sensitive_keys = [
            "token",
            "secret",
            "password",
            "api_key",
            "access_token",
            "setup_credential",
            "modal_token_secret",
            "beam_token",
            "client_secret",
            "token_id",
            "custom_token_id",
        ]
        for k in sensitive_keys:
            self.assertTrue(is_sensitive_key(k), f"Key '{k}' should be detected as sensitive")

        safe_keys = [
            "username",
            "resource_id",
            "installation_id",
            "status",
            "created_at",
            "profile_id",
        ]
        for k in safe_keys:
            self.assertFalse(is_sensitive_key(k), f"Key '{k}' should not be detected as sensitive")

    def test_nested_dict_and_list_redaction(self):
        payload = {
            "installation_id": "inst-100",
            "credentials": {
                "token_id": "ak-abcdef123456",
                "token_secret": "as-abcdef123456",
                "nested_tokens": ["b9_token_123456", "b9_token_789012"],
            },
            "status": "active",
            "logs": [
                "User authenticated with token b9_xyz987654321",
                {"detail": "Bearer my_secret_token_val"},
            ],
        }
        redacted = redact_data(payload)

        # Token keys are replaced with placeholder
        self.assertEqual(redacted["credentials"]["token_id"], REDACTED_CREDENTIAL)
        self.assertEqual(redacted["credentials"]["token_secret"], REDACTED_CREDENTIAL)
        self.assertEqual(
            redacted["credentials"]["nested_tokens"],
            [REDACTED_CREDENTIAL, REDACTED_CREDENTIAL],
        )
        self.assertEqual(redacted["installation_id"], "inst-100")
        self.assertEqual(redacted["status"], "active")

        # In log strings, tokens are masked
        self.assertNotIn("b9_xyz987654321", redacted["logs"][0])
        self.assertIn("b9_[REDACTED]", redacted["logs"][0])
        self.assertNotIn("my_secret_token_val", redacted["logs"][1]["detail"])
        self.assertIn("Bearer [REDACTED]", redacted["logs"][1]["detail"])

    def test_non_exact_token_id_and_benign_profile_id_redaction(self):
        """Verify non-exact token-bearing *_id keys are fully redacted and benign profile_id remains."""
        payload = {
            "profile_id": "profile-us-east-1",
            "custom_token_id": "cust-tok-998877",
            "nested": {
                "proxy_token_id": "proxy-tok-445566",
            },
        }
        redacted = redact_data(payload)
        # Benign profile_id is safe metadata and remains unredacted
        self.assertEqual(redacted["profile_id"], "profile-us-east-1")
        # Token-bearing *_id keys undergo full credential redaction
        self.assertEqual(redacted["custom_token_id"], REDACTED_CREDENTIAL)
        self.assertEqual(redacted["nested"]["proxy_token_id"], REDACTED_CREDENTIAL)

    def test_session_registry_dynamic_secret_redaction(self):
        registry = RedactionRegistry()
        custom_secret = "custom_dynamic_secret_value_xyz"
        registry.register(custom_secret)

        text = f"An error occurred while passing {custom_secret} to upstream service."
        redacted = redact_string(text, additional_secrets=registry.registered_secrets)
        self.assertNotIn(custom_secret, redacted)
        self.assertIn(REDACTED_CREDENTIAL, redacted)

        # Test unregister
        registry.unregister(custom_secret)
        self.assertNotIn(custom_secret, registry.registered_secrets)

    def test_exception_redaction(self):
        err = ValueError("Invalid credential b9_secret_token_123 in request")
        redacted = redact_data(err)
        self.assertNotIn("b9_secret_token_123", redacted)
        self.assertIn("b9_[REDACTED]", redacted)

    def test_response_envelope_serialization_automatic_redaction(self):
        resp = make_success_response(
            "req-sec",
            {
                "token": "b9_my_secret_token_000",
                "secret_key": "as-modal-secret-111",
                "message": "Token b9_another_secret was created",
            },
        )
        serialized = resp.serialize()
        self.assertNotIn("b9_my_secret_token_000", serialized)
        self.assertNotIn("as-modal-secret-111", serialized)
        self.assertNotIn("b9_another_secret", serialized)


    def test_modal_proxy_secret_redacted_but_token_id_kept(self):
        """ws- secrets are redacted in text; the wk- id stays, the journal needs it for cleanup."""
        text = "proxy token wk-AbC123def456 secret ws-XyZ789ghi012"
        redacted = redact_string(text)
        self.assertNotIn("ws-XyZ789ghi012", redacted)
        self.assertIn("ws-[REDACTED]", redacted)
        self.assertIn("wk-AbC123def456", redacted)

    def test_names_ending_in_a_token_prefix_are_not_tokens(self):
        """A Modal host for the workspace "studio-ws" is a name, not a ws- secret."""
        url = "https://studio-ws--mc-ab12cd-gateway.modal.run/mc/v1"
        self.assertEqual(redact_string(url), url)
        self.assertEqual(redact_string("team-as-staging-2024.example"), "team-as-staging-2024.example")
        self.assertEqual(redact_string("secret=ws-XyZ789ghi012"), "secret=ws-[REDACTED]")
        self.assertEqual(redact_string("(ws-XyZ789ghi012)"), "(ws-[REDACTED])")

    def test_patterns_off_keeps_names_and_still_redacts_known_secrets(self):
        GLOBAL_REGISTRY.register("as-registered-secret-1")
        try:
            record = {"environment_name": "ws-staging-env1", "note": "as-registered-secret-1", "token": "x-1234"}
            self.assertEqual(
                redact_data(record, patterns=False),
                {"environment_name": "ws-staging-env1", "note": "[REDACTED_CREDENTIAL]", "token": "[REDACTED_CREDENTIAL]"},
            )
            self.assertEqual(redact_data(record)["environment_name"], "ws-[REDACTED]")
        finally:
            GLOBAL_REGISTRY.unregister("as-registered-secret-1")


if __name__ == "__main__":
    unittest.main()
