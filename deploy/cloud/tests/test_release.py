"""The code digest a deployment reports, and the files it covers.

`src-tauri/src/cloud_code_digest.rs` builds the same fixture tree and must reach
the same digest, or Settings would ask for an update that changes nothing.
"""

import tempfile
import unittest
from pathlib import Path

from deploy.cloud.common.api import CloudGateway
from deploy.cloud.common.release import code_digest, shipped, shipped_files

GOLDEN = "875a779b6276a7b7ad9827559628c804c86ea4b3703729ca3ac1a54e41c97333"

FILES = {
    "__init__.py": b"",
    "cloud/__init__.py": b"# cloud\n",
    "cloud/common/api.py": b"print('gateway')\n",
    "cloud/common/notes.txt": b"not shipped\n",
    "cloud/common/deep/inner.py": b"not shipped\n",
    "cloud/modal/app.py": b"app = 1\r\n",
    "cloud/beam/app.py": b"not shipped\n",
    "./tools.py": b"not shipped\n",
}


class CodeDigestTest(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        for name, data in FILES.items():
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_bytes(data)

    def test_covers_only_the_shipped_sources_and_matches_the_desktop(self) -> None:
        listed = [path.as_posix() for path in shipped_files(self.root)]
        self.assertEqual(listed, ["__init__.py", "cloud/__init__.py", "cloud/common/api.py", "cloud/modal/app.py"])
        self.assertTrue(all(shipped(Path(name)) == (name in listed) for name in FILES))
        self.assertEqual(code_digest(self.root), GOLDEN)

    def test_any_shipped_change_changes_it_and_nothing_else_does(self) -> None:
        (self.root / "cloud/beam/app.py").write_bytes(b"changed\n")
        (self.root / "cloud/common/notes.txt").write_bytes(b"changed\n")
        self.assertEqual(code_digest(self.root), GOLDEN)
        (self.root / "cloud/modal/app.py").write_bytes(b"app = 2\r\n")
        self.assertNotEqual(code_digest(self.root), GOLDEN)

    def test_the_repository_code_hashes(self) -> None:
        deploy = Path(__file__).resolve().parents[2]
        self.assertRegex(code_digest(deploy), r"^[0-9a-f]{64}$")
        self.assertIn(Path("cloud/common/release.py"), shipped_files(deploy))


class AdvertisedDigestTest(unittest.TestCase):
    def capabilities(self, gateway: CloudGateway) -> dict:
        import json
        status, _, body = gateway.handle_http_request("GET", "/mc/v1/capabilities", {}, b"")
        self.assertEqual(status, 200, body)
        return json.loads(body)

    def test_the_gateway_says_which_code_it_runs_when_it_knows(self) -> None:
        with_digest = CloudGateway(provider="modal", trust_edge_auth=True, code_digest=GOLDEN)
        self.assertEqual(self.capabilities(with_digest)["code_digest"], GOLDEN)
        without = CloudGateway(provider="modal", trust_edge_auth=True)
        self.assertNotIn("code_digest", self.capabilities(without))


if __name__ == "__main__":
    unittest.main()
