"""`python -m provisioner` as the desktop runs it: a subprocess with piped streams.

Proves:
1. Bounded input: oversized, empty and malformed requests get a typed error.
2. One response: stdout carries exactly one JSON document, whatever the SDKs print.
3. Progress only: every stderr line is an IC-2 record, and no credential reaches either stream.
4. Typed failures: no credentials is a validation error; a provider that cannot be
   reached (no SDK installed, or the SDK pointed at a closed local port) is
   ERR_PROVIDER_UNAVAILABLE.

Every run is hermetic: token variables are removed, HOME is a scratch directory,
and both SDKs are pointed at 127.0.0.1 port 9, so no test can reach a real provider
even when the SDKs are installed.
"""

import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

from provisioner.protocol import (
    ERR_INVALID_PAYLOAD,
    ERR_PAYLOAD_TOO_LARGE,
    ERR_PROVIDER_UNAVAILABLE,
    ERR_UNSUPPORTED_OP,
    ERR_UNSUPPORTED_PROVIDER,
    ERR_VALIDATION,
    HELPER_PROTOCOL_VERSION,
    MAX_REQUEST_BYTES,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
RUN_TIMEOUT_SECONDS = 120
IC2_LINE = re.compile(r'^\{"mc_progress":1,"op":"[a-z_]+","step":"[a-z]+","state":"(start|done|fail|skip)","pct":(null|\d{1,3})\}$')
MODAL_CREDENTIALS = {"token_id": "ak-subprocess-token-id", "token_secret": "as-subprocess-token-secret-123456"}
BEAM_CREDENTIALS = {"token": "subprocess-beam-token-7890abcdef"}
HAS_SDKS = importlib.util.find_spec("modal") is not None and importlib.util.find_spec("beta9") is not None

NOISY_HELPER = r"""
import logging, os, sys, warnings
from provisioner import cli
from provisioner.progress import Progress

class Noisy:
    def handle_request(self, raw):
        print("print to stdout")
        print("print to stderr", file=sys.stderr)
        os.write(1, b"fd 1 noise\n")
        os.write(2, b"fd 2 noise\n")
        logging.getLogger("grpc").error("log noise")
        warnings.warn("warning noise")
        Progress("inspect", PROGRESS).emit("inspect", "start")
        return '{"success": true}'

RESPONSE, PROGRESS = cli.isolate_standard_streams()
sys.exit(cli.run_helper(sys.stdin.buffer, RESPONSE, PROGRESS, controller=Noisy()))
"""


def hermetic_env(home: Path) -> dict:
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("MODAL_", "BEAM_", "BETA9_", "MC_BEAM_", "MANGA_CLEANER_"))
        and key not in ("CONFIG_PATH", "GATEWAY_HOST", "GATEWAY_PORT", "API_HOST", "API_PORT")
    }
    env.update(
        HOME=str(home),
        USERPROFILE=str(home),
        MODAL_SERVER_URL="http://127.0.0.1:9",
        MC_BEAM_GATEWAY_HOST="127.0.0.1",
        MC_BEAM_GATEWAY_PORT="9",
        PYTHONDONTWRITEBYTECODE="1",
    )
    return env


class TestSubprocessEntrypoint(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.scratch = Path(self.temp_dir.name)
        self.journal_root = self.scratch / "journal"
        self.home = self.scratch / "home"
        self.home.mkdir()

    def tearDown(self):
        self.temp_dir.cleanup()

    def run_helper(self, stdin: str, args=None) -> subprocess.CompletedProcess:
        command = args or [sys.executable, "-m", "provisioner", "--journal-root", str(self.journal_root)]
        return subprocess.run(
            command,
            input=stdin.encode("utf-8"),
            capture_output=True,
            cwd=str(REPO_ROOT),
            env=hermetic_env(self.home),
            timeout=RUN_TIMEOUT_SECONDS,
        )

    def request(self, op: str, provider: str, params: dict) -> str:
        return json.dumps(
            {"protocol_version": HELPER_PROTOCOL_VERSION, "request_id": f"req-{op}", "op": op, "provider": provider, "params": params}
        )

    def response(self, result: subprocess.CompletedProcess) -> dict:
        """The one stdout document, after checking both streams keep their contract."""
        self.assertEqual(result.returncode, 0, result.stderr)
        stdout = result.stdout.decode("utf-8")
        self.assertTrue(stdout.startswith("{") and stdout.endswith("}\n"), f"stdout is not one JSON document: {stdout!r}")
        for line in result.stderr.decode("utf-8").splitlines():
            self.assertRegex(line, IC2_LINE)
        return json.loads(stdout)  # raises on anything after the document

    # --- 1. bounded input ---

    def test_oversized_empty_and_malformed_requests(self):
        for stdin, code in (("x" * (MAX_REQUEST_BYTES + 4096), ERR_PAYLOAD_TOO_LARGE), ("", ERR_INVALID_PAYLOAD), ("{not json", ERR_INVALID_PAYLOAD)):
            with self.subTest(code=code, size=len(stdin)):
                data = self.response(self.run_helper(stdin))
                self.assertEqual((data["success"], data["error"]["code"]), (False, code))

    def test_unknown_provider_and_operation(self):
        data = self.response(self.run_helper(self.request("inspect", "unsupported_cloud", {})))
        self.assertEqual(data["error"]["code"], ERR_UNSUPPORTED_PROVIDER)
        data = self.response(self.run_helper(self.request("arbitrary_bad_op", "modal", {})))
        self.assertEqual(data["error"]["code"], ERR_UNSUPPORTED_OP)

    # --- 4. typed failures ---

    def test_no_credentials_is_a_validation_error(self):
        for provider in ("modal", "beam"):
            with self.subTest(provider=provider):
                data = self.response(self.run_helper(self.request("inspect", provider, {})))
                self.assertEqual((data["success"], data["error"]["code"]), (False, ERR_VALIDATION))
                self.assertEqual(data["request_id"], "req-inspect")

    def test_unreachable_provider_is_typed_and_streams_stay_clean(self):
        # With the SDKs installed the Modal case waits out the sign-in bound (40 s).
        cases = (
            ("modal", MODAL_CREDENTIALS, "plan", {"installation_id": "mc-ab12cd"}),
            ("beam", BEAM_CREDENTIALS, "inspect", {}),
            ("beam", BEAM_CREDENTIALS, "apply", {"installation_id": "mc-ab12cd", "approved_plan_hash": "a" * 64}),
        )
        for provider, credentials, op, params in cases:
            with self.subTest(provider=provider, op=op):
                result = self.run_helper(self.request(op, provider, dict(params, credentials=credentials)))
                data = self.response(result)
                self.assertEqual((data["success"], data["error"]["code"]), (False, ERR_PROVIDER_UNAVAILABLE), data)
                for secret in credentials.values():
                    self.assertNotIn(secret.encode(), result.stdout)
                    self.assertNotIn(secret.encode(), result.stderr)
                progress = [json.loads(line) for line in result.stderr.decode().splitlines()]
                self.assertEqual([(p["op"], p["step"], p["state"]) for p in progress], [(op, "inspect", "start"), (op, "inspect", "fail")])
                self.assertFalse((self.journal_root / "installations" / "mc-ab12cd").exists(), "nothing was recorded")

    # --- 2 and 3. stream isolation ---

    def test_sdk_noise_never_reaches_the_desktop(self):
        result = self.run_helper("{}", args=[sys.executable, "-c", NOISY_HELPER])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b'{"success": true}\n')
        self.assertEqual(result.stderr, b'{"mc_progress":1,"op":"inspect","step":"inspect","state":"start","pct":null}\n')

    def test_self_check_reports_one_json_line(self):
        result = self.run_helper("", args=[sys.executable, "-m", "provisioner", "--self-check"])
        self.assertEqual(result.stderr, b"")
        lines = result.stdout.decode("utf-8").splitlines()
        self.assertEqual(len(lines), 1)
        report = json.loads(lines[0])
        self.assertEqual(report["ok"], HAS_SDKS, report)
        self.assertEqual(result.returncode, 0 if HAS_SDKS else 1)
        if HAS_SDKS:
            self.assertEqual(report["modal_app"], [
                "AnalysisGPU.*", "Worker.*", "gateway", "seed_weights", "AnalysisGPU", "Worker",
            ])
            self.assertEqual(report["beam_app"], ["analyze", "gateway", "render", "seed"])


if __name__ == "__main__":
    unittest.main()
