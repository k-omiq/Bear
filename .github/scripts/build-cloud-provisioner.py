"""Freeze the cloud setup helper (manga-cleaner-provisioner) on this machine.

    python .github/scripts/build-cloud-provisioner.py <release-target> [--dist DIR] [--work DIR] [--no-config] [--online]

Run it in an environment that installed provisioner/requirements.lock with
--require-hashes. The desktop gives the helper one JSON request on stdin; credentials
never travel through argv or the environment.

The provider SDKs are collected whole, with their data files and the package
metadata they read at import time. The deploy tree ships as plain files next to the
bundled modules and is kept out of the PYZ: Modal uploads those files into its
containers and Beam copies them into its staging directory, so they must exist on
disk (provisioner.cli.ensure_deploy_importable puts them on sys.path). PyInstaller
therefore never analyses the deploy tree, and every module it imports on the desktop
is named here as a hidden import.

The frozen binary is then smoke tested offline: the self check imports both SDKs and
builds both apps; a request without credentials is a validation error; fake
credentials against a closed local port give a typed ERR_PROVIDER_UNAVAILABLE. Every
run must leave exactly one JSON document on stdout and only IC-2 progress lines on
stderr. With --online it also signs in to the real Modal API with an unknown token,
which Modal must refuse: proof that the binary alone reaches Modal over TLS.
"""

import argparse
import ast
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Dict, List, Optional


ROOT = Path(__file__).resolve().parents[2]
TARGETS = {
    "aarch64-apple-darwin": ("Darwin", "arm64"),
    "x86_64-pc-windows-msvc": ("Windows", "AMD64"),
    "x86_64-unknown-linux-gnu": ("Linux", "x86_64"),
}
# Import packages collected with their submodules, data files and binaries.
# betterproto is left to the analysis of beta9: its protoc plugin subpackage exits
# the interpreter when imported without the compiler extras.
SDK_PACKAGES = ("modal", "modal_proto", "modal_version", "beam", "beta9", "synchronicity", "grpclib")
# Distributions whose metadata is read at import time (betterproto asks for its own
# version, for one) or by the SDKs' user agents.
SDK_METADATA = ("modal", "beam-client", "beta9", "betterproto-beta9")
# Third-party packages the deploy tree imports on the desktop. Anything else it
# imports (torch, diffusers, huggingface_hub, ...) runs only inside the containers.
DESKTOP_THIRD_PARTY = {"modal", "beam", "beta9", "starlette"}
# Modal attaches these files to the SAM seed image from the helper process.
# They must exist beside the frozen deploy tree at runtime, not only in a
# developer checkout. The self-check imports the SAM-enabled app and catches
# missing files before an installer is built.
ANALYSIS_SOURCE_FILES = (
    ("sam-bootstrap/bootstrap.py", "sam-bootstrap"),
    ("spikes/sam-ts-l/export_mask.py", "spikes/sam-ts-l"),
    ("spikes/sam-ts-l/fixtures/synthetic_page.png", "spikes/sam-ts-l/fixtures"),
)
IC2_LINE = re.compile(r'^\{"mc_progress":1,"op":"[a-z_]+","step":"[a-z]+","state":"(start|done|fail|skip)","pct":(null|\d{1,3})\}$')
SMOKE_TIMEOUT_SECONDS = 120


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Freeze and smoke test the cloud setup helper.")
    parser.add_argument("target", choices=sorted(TARGETS))
    parser.add_argument("--dist", type=Path, default=ROOT / "src-tauri" / "binaries", help="where the binary goes")
    parser.add_argument("--work", type=Path, default=ROOT / "target" / "cloud-helper-build", help="PyInstaller work dir")
    parser.add_argument(
        "--no-config", action="store_true", help="leave src-tauri/tauri.conf.json alone (local builds)"
    )
    parser.add_argument(
        "--online", action="store_true", help="also sign in to the real Modal API with an unknown token"
    )
    return parser.parse_args()


def production_modules() -> List[str]:
    """The provisioner package without its tests and fakes."""
    names = []
    for path in sorted((ROOT / "provisioner").glob("*.py")):
        if path.stem.startswith("test_") or path.stem == "fake_sdks":
            continue
        names.append("provisioner" if path.stem == "__init__" else f"provisioner.{path.stem}")
    return names


def excluded_modules() -> List[str]:
    tests = [f"provisioner.{path.stem}" for path in sorted((ROOT / "provisioner").glob("test_*.py"))]
    return ["deploy", "provisioner.fake_sdks", *tests]


def deploy_sources() -> List[Path]:
    root = ROOT / "deploy"
    return sorted(path for path in root.rglob("*.py") if "tests" not in path.relative_to(root).parts)


def deploy_imports() -> List[str]:
    """Modules the deploy tree imports that the desktop side needs: stdlib and DESKTOP_THIRD_PARTY."""
    found = set()
    for path in deploy_sources():
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                found.update(alias.name for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                found.add(node.module)
    wanted = []
    for name in sorted(found):
        top = name.split(".")[0]
        if top in ("deploy", "__future__"):
            continue
        if top in sys.stdlib_module_names or top in DESKTOP_THIRD_PARTY:
            if importlib.util.find_spec(top) is None:
                raise SystemExit(f"the deploy tree imports {name}, which this environment does not have")
            wanted.append(name)
    return wanted


def frozen_sources() -> List[Path]:
    """Every checkout file the binary is built from: the stamp lists these."""
    provisioner = [ROOT / "provisioner" / ("__init__.py" if name == "provisioner" else name.split(".", 1)[1] + ".py")
                   for name in production_modules()]
    analysis = [ROOT / source for source, _ in ANALYSIS_SOURCE_FILES]
    lock = ROOT / "provisioner" / "requirements.lock"
    return sorted({*provisioner, *deploy_sources(), *analysis, lock, Path(__file__).resolve()})


def write_stamp(binary: Path) -> Path:
    """`<binary>.sha256`, one sha256sum line per source file, paths from the checkout root.

    src-tauri/build.rs reads it: a release build refuses a helper whose sources
    changed since it was frozen, and a development build runs the live Python
    instead. `shasum -a 256 -c` checks it from the checkout root too.
    """
    lines = [f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(ROOT).as_posix()}"
             for path in frozen_sources()]
    stamp = binary.with_name(binary.name + ".sha256")
    stamp.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    return stamp


def stage_deploy_tree(work: Path) -> Path:
    """A clean copy of the runtime deploy files: sources only, no tests, fixtures or caches."""
    staged = work / "deploy-data"
    shutil.rmtree(staged, ignore_errors=True)
    for source in deploy_sources():
        target = staged / "deploy" / source.relative_to(ROOT / "deploy")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
    return staged / "deploy"


def freeze(target: str, dist: Path, work: Path) -> Path:
    expected_os = TARGETS[target][0]
    base_name = f"manga-cleaner-provisioner-{target}"
    dist.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)
    entry = work / "manga_cleaner_provisioner.py"
    entry.write_text("import sys\n\nfrom provisioner.cli import main\n\nsys.exit(main())\n", encoding="utf-8")
    command = [
        sys.executable, "-m", "PyInstaller", "--clean", "--noconfirm", "--onefile", "--console",
        "--name", base_name,
        "--distpath", str(dist),
        "--workpath", str(work / "pyinstaller"),
        "--specpath", str(work),
        "--paths", str(ROOT),
        "--add-data", f"{stage_deploy_tree(work)}{os.pathsep}deploy",
    ]
    for source, destination in ANALYSIS_SOURCE_FILES:
        command += ["--add-data", f"{ROOT / source}{os.pathsep}{destination}"]
    for package in SDK_PACKAGES:
        command += ["--collect-all", package]
    for distribution in SDK_METADATA:
        command += ["--copy-metadata", distribution]
    for module in production_modules() + deploy_imports():
        command += ["--hidden-import", module]
    for module in excluded_modules():
        command += ["--exclude-module", module]
    command.append(str(entry))
    binary = dist / (base_name + (".exe" if expected_os == "Windows" else ""))
    # The old stamp would vouch for a binary that failed its smoke test.
    binary.with_name(binary.name + ".sha256").unlink(missing_ok=True)
    subprocess.run(command, cwd=ROOT, check=True)
    if not binary.is_file():
        raise SystemExit("frozen helper binary missing")
    return binary


def hermetic_env(home: Path) -> Dict[str, str]:
    """No token variables, a scratch home, and both SDKs pointed at a closed local port."""
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
    )
    return env


def run_binary(
    binary: Path, home: Path, args: List[str], request: Optional[dict], env: Optional[Dict[str, str]] = None
) -> dict:
    completed = subprocess.run(
        [str(binary), *args],
        input=json.dumps(request) if request is not None else "",
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=SMOKE_TIMEOUT_SECONDS,
        env=env if env is not None else hermetic_env(home),
        cwd=str(home),
    )
    stdout = completed.stdout
    if not stdout.startswith("{"):
        raise SystemExit(f"frozen helper wrote something before its response: {stdout[:200]!r}")
    try:
        document = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise SystemExit(f"frozen helper did not write exactly one JSON document: {stdout[:400]!r}") from error
    for line in completed.stderr.splitlines():
        if not IC2_LINE.match(line):
            raise SystemExit(f"frozen helper wrote a non-progress line on stderr: {line[:200]!r}")
    return document


def request(op: str, provider: str, params: dict) -> dict:
    return {"protocol_version": "1.0.0", "request_id": f"smoke-{op}-{provider}", "op": op, "provider": provider, "params": params}


def smoke_test(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="mc-helper-smoke-") as scratch:
        home = Path(scratch)
        journal = ["--journal-root", str(home / "journal")]

        report = run_binary(binary, home, ["--self-check"], None)
        if report.get("ok") is not True:
            raise SystemExit(f"frozen helper self check failed: {report}")
        print(f"self check: modal {report.get('modal')}, apps {report.get('modal_app')} and {report.get('beam_app')}")

        for provider in ("modal", "beam"):
            response = run_binary(binary, home, journal, request("inspect", provider, {}))
            if response.get("success") is not False or response["error"]["code"] != "ERR_VALIDATION_ERROR":
                raise SystemExit(f"{provider}: no credentials must be a validation error, got {response}")

        fake = {
            "modal": {"token_id": "ak-smoke-test-token-id", "token_secret": "as-smoke-test-token-secret"},
            "beam": {"token": "smoke-test-beam-token-000000"},
        }
        for provider, credentials in fake.items():
            response = run_binary(binary, home, journal, request("inspect", provider, {"credentials": credentials}))
            if response.get("success") is not False or response["error"]["code"] != "ERR_PROVIDER_UNAVAILABLE":
                raise SystemExit(f"{provider}: an unreachable provider must be ERR_PROVIDER_UNAVAILABLE, got {response}")
            if any(secret in json.dumps(response) for secret in credentials.values()):
                raise SystemExit(f"{provider}: the response carries the credential")
            print(f"{provider} unreachable: {response['error']['message']}")


def online_check(binary: Path) -> None:
    """Sign in to the real Modal API with a token Modal does not know.

    The smoke test never leaves the machine, so it cannot show that the frozen
    SDK reaches Modal over TLS on a computer with no Python and no Modal of its
    own. Modal refusing the token does: the request crossed the network and the
    handshake passed on the certificates bundled in the binary, with every
    certificate override removed from the environment. It needs the network,
    so the release workflow runs it on manual validation runs only.
    """
    with tempfile.TemporaryDirectory(prefix="mc-helper-online-") as scratch:
        home = Path(scratch)
        env = hermetic_env(home)
        for key in ("MODAL_SERVER_URL", "SSL_CERT_FILE", "SSL_CERT_DIR", "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE"):
            env.pop(key, None)
        # Well formed, so Modal looks it up and answers "Token not found".
        credentials = {"token_id": "ak-onlinecheck0000000000000", "token_secret": "as-onlinecheck0000000000000"}
        journal = ["--journal-root", str(home / "journal")]
        response = run_binary(binary, home, journal, request("inspect", "modal", {"credentials": credentials}), env)
        code = (response.get("error") or {}).get("code")
        if response.get("success") is not False or code != "ERR_ACTIONABLE_MISSING_PERMISSION":
            raise SystemExit(f"modal online: an unknown token must be refused by Modal itself, got {response}")
        if any(secret in json.dumps(response) for secret in credentials.values()):
            raise SystemExit("modal online: the response carries the credential")
        print(f"modal online: {response['error']['message']}")


def inject_external_bin() -> None:
    # Tauri expects the source sidecar path with the target triple suffix and
    # installs it beside the desktop executable without that suffix. Keep the
    # source config free of externalBin so ordinary cargo check and dev builds
    # work without generated release artefacts.
    config_path = ROOT / "src-tauri" / "tauri.conf.json"
    config = json.loads(config_path.read_text(encoding="utf-8"))
    config["bundle"]["externalBin"] = ["binaries/manga-cleaner-provisioner"]
    config_path.write_text(json.dumps(config, indent=2) + "\n", encoding="utf-8")


def main() -> None:
    args = parse_args()
    expected_os, expected_cpu = TARGETS[args.target]
    if (platform.system(), platform.machine()) != (expected_os, expected_cpu):
        raise SystemExit(f"helper must be frozen on {expected_os}/{expected_cpu} for {args.target}")
    binary = freeze(args.target, args.dist.resolve(), args.work.resolve())
    smoke_test(binary)
    if args.online:
        online_check(binary)
    write_stamp(binary)
    if not args.no_config:
        inject_external_bin()
    size_mib = binary.stat().st_size / (1024 * 1024)
    print(f"Cloud helper ready: {binary} ({size_mib:.1f} MiB)")


if __name__ == "__main__":
    main()
