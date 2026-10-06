"""Protocol validation, fuzzing, and invalid-input unit tests."""

import json
import unittest

from provisioner.protocol import (
    ERR_INVALID_PAYLOAD,
    ERR_INVALID_VERSION,
    ERR_PAYLOAD_TOO_LARGE,
    ERR_SECURITY_VIOLATION,
    ERR_UNSUPPORTED_OP,
    ERR_UNSUPPORTED_PROVIDER,
    ERR_VALIDATION,
    HELPER_PROTOCOL_VERSION,
    MAX_REQUEST_BYTES,
    OP_INSPECT,
    OP_PLAN,
    PROVIDER_BEAM,
    PROVIDER_MODAL,
    HelperRequest,
    HelperResponse,
    ProtocolError,
    make_error_response,
    make_success_response,
    parse_request,
)


class TestHelperProtocol(unittest.TestCase):
    """Tests for helper request envelope validation and protocol conformance."""

    def test_valid_request_parsing(self):
        payload = {
            "protocol_version": HELPER_PROTOCOL_VERSION,
            "request_id": "req-123",
            "op": OP_INSPECT,
            "provider": PROVIDER_MODAL,
            "params": {"credentials": {"token_id": "ak-test", "token_secret": "as-test"}},
        }
        req = parse_request(payload)
        self.assertEqual(req.protocol_version, HELPER_PROTOCOL_VERSION)
        self.assertEqual(req.request_id, "req-123")
        self.assertEqual(req.op, OP_INSPECT)
        self.assertEqual(req.provider, PROVIDER_MODAL)
        self.assertEqual(req.params["credentials"]["token_id"], "ak-test")

    def test_bytes_and_string_parsing(self):
        payload = {
            "protocol_version": HELPER_PROTOCOL_VERSION,
            "request_id": "req-bytes",
            "op": OP_PLAN,
            "provider": PROVIDER_BEAM,
            "params": {"installation_id": "inst-test"},
        }
        json_str = json.dumps(payload)
        # Parse from string
        req_from_str = parse_request(json_str)
        self.assertEqual(req_from_str.request_id, "req-bytes")

        # Parse from bytes
        req_from_bytes = parse_request(json_str.encode("utf-8"))
        self.assertEqual(req_from_bytes.request_id, "req-bytes")

    def test_reject_invalid_protocol_version(self):
        for bad_ver in ["0.9.0", "2.0.0", "", None, 1]:
            payload = {
                "protocol_version": bad_ver,
                "request_id": "req-1",
                "op": OP_INSPECT,
                "provider": PROVIDER_MODAL,
                "params": {},
            }
            with self.assertRaises(ProtocolError) as ctx:
                parse_request(payload)
            self.assertEqual(ctx.exception.code, ERR_INVALID_VERSION)

    def test_reject_unsupported_operation(self):
        for bad_op in ["shell_exec", "run_bash", "eval", "arbitrary_cmd", ""]:
            payload = {
                "protocol_version": HELPER_PROTOCOL_VERSION,
                "request_id": "req-2",
                "op": bad_op,
                "provider": PROVIDER_MODAL,
                "params": {},
            }
            with self.assertRaises(ProtocolError) as ctx:
                parse_request(payload)
            self.assertEqual(ctx.exception.code, ERR_UNSUPPORTED_OP)

    def test_reject_unsupported_provider(self):
        for bad_prov in ["aws", "gcp", "azure", "unknown", ""]:
            payload = {
                "protocol_version": HELPER_PROTOCOL_VERSION,
                "request_id": "req-3",
                "op": OP_INSPECT,
                "provider": bad_prov,
                "params": {},
            }
            with self.assertRaises(ProtocolError) as ctx:
                parse_request(payload)
            self.assertEqual(ctx.exception.code, ERR_UNSUPPORTED_PROVIDER)

    def test_reject_unknown_envelope_fields(self):
        payload = {
            "protocol_version": HELPER_PROTOCOL_VERSION,
            "request_id": "req-4",
            "op": OP_INSPECT,
            "provider": PROVIDER_MODAL,
            "params": {},
            "forbidden_extra_field": "malicious_injection",
        }
        with self.assertRaises(ProtocolError) as ctx:
            parse_request(payload)
        self.assertEqual(ctx.exception.code, ERR_VALIDATION)
        self.assertIn("unknown fields", ctx.exception.message)

    def test_reject_oversized_payload(self):
        large_bytes = b"{" + b'"padding": "' + b"a" * (MAX_REQUEST_BYTES + 1024) + b'"}'
        with self.assertRaises(ProtocolError) as ctx:
            parse_request(large_bytes)
        self.assertEqual(ctx.exception.code, ERR_PAYLOAD_TOO_LARGE)

    def test_reject_malformed_json(self):
        for bad_json in [b"{not-valid-json}", "{incomplete", "random_string", b"\xff\xfe\x00"]:
            with self.assertRaises(ProtocolError) as ctx:
                parse_request(bad_json)
            self.assertEqual(ctx.exception.code, ERR_INVALID_PAYLOAD)

    def test_fuzz_path_traversal_and_null_bytes(self):
        traversal_attempts = [
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "/absolute/path/traversal",
            "valid_name\0_null_byte",
            "sub/dir/attack",
        ]
        for bad_val in traversal_attempts:
            payload = {
                "protocol_version": HELPER_PROTOCOL_VERSION,
                "request_id": "req-attack",
                "op": OP_PLAN,
                "provider": PROVIDER_MODAL,
                "params": {"installation_id": bad_val},
            }
            with self.assertRaises(ProtocolError) as ctx:
                parse_request(payload)
            self.assertIn(
                ctx.exception.code,
                [ERR_SECURITY_VIOLATION, ERR_VALIDATION],
            )

    def test_nested_param_path_traversal_rejected(self):
        payload = {
            "protocol_version": HELPER_PROTOCOL_VERSION,
            "request_id": "req-deep",
            "op": OP_INSPECT,
            "provider": PROVIDER_MODAL,
            "params": {"config": {"template_path": "../../../sensitive"}},
        }
        with self.assertRaises(ProtocolError) as ctx:
            parse_request(payload)
        self.assertEqual(ctx.exception.code, ERR_SECURITY_VIOLATION)

    def test_response_envelope_serialization(self):
        resp = make_success_response("req-ok", {"status": "ready"})
        serialized = resp.serialize()
        data = json.loads(serialized)
        self.assertEqual(data["protocol_version"], HELPER_PROTOCOL_VERSION)
        self.assertEqual(data["request_id"], "req-ok")
        self.assertTrue(data["success"])
        self.assertEqual(data["data"]["status"], "ready")

    def test_error_response_envelope(self):
        resp = make_error_response(
            "req-err",
            ERR_VALIDATION,
            "Field missing",
            actionable_guidance="Provide the required parameter",
            remedy_steps=["Step 1", "Step 2"],
        )
        serialized = resp.serialize()
        data = json.loads(serialized)
        self.assertEqual(data["protocol_version"], HELPER_PROTOCOL_VERSION)
        self.assertFalse(data["success"])
        self.assertEqual(data["error"]["code"], ERR_VALIDATION)
        self.assertEqual(data["error"]["actionable_guidance"], "Provide the required parameter")
        self.assertEqual(len(data["error"]["remedy_steps"]), 2)


    def test_ic1_carve_out_keeps_only_the_runtime_credential_and_endpoint(self):
        """Everything is redacted except the credential and endpoint apply or resume issue."""
        credential = {"kind": "modal_proxy", "token_id": "wk-abcdef123456", "token_secret": "ws-abcdef123456"}
        resp = make_success_response(
            "req-ic1",
            {
                "installation_id": "mc-ab12cd",
                "setup_token": "as-setup-secret-123456",
                "runtime_credential": dict(credential),
                "note": "setup used as-setup-secret-123456",
            },
            verbatim={"runtime_credential": credential, "endpoint_url": "https://ws--mc-ab12cd-gateway.modal.run/mc/v1"},
        )
        data = json.loads(resp.serialize())["data"]
        self.assertEqual(data["runtime_credential"], credential)
        self.assertEqual(data["endpoint_url"], "https://ws--mc-ab12cd-gateway.modal.run/mc/v1")
        self.assertEqual(data["setup_token"], "[REDACTED_CREDENTIAL]")
        self.assertNotIn("as-setup-secret-123456", json.dumps(data))
        # to_dict never carries the carve-out, so no other path can leak it
        self.assertEqual(resp.to_dict()["data"]["runtime_credential"]["token_secret"], "ws-abcdef123456")
        self.assertNotIn("verbatim", resp.to_dict())

    def test_carve_out_is_limited_to_two_fields_and_success(self):
        with self.assertRaises(ValueError):
            make_success_response("req-x", {}, verbatim={"setup_token": "as-nope"})
        failed = HelperResponse(HELPER_PROTOCOL_VERSION, "req-f", False, {}, {"code": "E"}, verbatim={"endpoint_url": "x"})
        self.assertEqual(json.loads(failed.serialize())["data"], {})

    def test_without_carve_out_the_runtime_credential_is_redacted(self):
        resp = make_success_response(
            "req-plain", {"runtime_credential": {"kind": "beam_bearer", "token": "b9_secret_token_value"}}
        )
        data = json.loads(resp.serialize())["data"]
        self.assertEqual(data["runtime_credential"], {"kind": "beam_bearer", "token": "[REDACTED_CREDENTIAL]"})


if __name__ == "__main__":
    unittest.main()
