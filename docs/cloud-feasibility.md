# Manga Cleaner Cloud Provisioner Feasibility Report (P1)

> **Current state:** this is the P1 record. The provisioning drivers that replaced its probe, matrix, plans and templates are described in [cloud-provisioning.md](cloud-provisioning.md).

**Status:** P1 Offline Preparation Complete (Candidate Matrix, AST Probes, Staging/Cleanup Plans Verified)
**Target Plan:** [`manga-cleaner-cloud-master-plan.md`](../manga-cleaner-cloud-master-plan.md) § P1
**Wire Specification:** [`docs/cloud-api.md`](cloud-api.md) (`/mc/v1` Wire Protocol)
**Execution Mode:** Strictly Offline & CPU-Only (Zero Cloud Requests, Zero Account Mutations, Zero Global Config Writes, Zero GPU Imports, Zero Model Downloads)
**Verification Date:** 2026-09-19

---

## 1. Executive Summary & Verification Invariants

This report documents the offline feasibility preparation for Phase 1 (P1) of the Manga Cleaner Cloud Integration architecture. All findings are derived strictly from released source code packages on PyPI, official vendor documentation, and local CPU-only AST introspection.

### Core Invariants Maintained
1. **Zero Secret Reads / Zero Global Writes:** No global CLI configuration files (`~/.modal.toml`, `~/.beam/config.ini`) were accessed or written. No credentials were read or generated.
2. **Zero GPU Stacks Loaded:** Introspection and packaging probes ran strictly on CPU with standard library AST tools; `torch`, `torchvision`, `diffusers`, and `cuda` were not imported into the Python runtime.
3. **Source-Verified vs. Hosted Platform Separation:** APIs demonstrated in released client code are cataloged as *source-verified*, while server-side policy enforcement, lease restrictions, and container residency are cataloged as *unverified hosted behaviors*.
4. **No Speculative Implementation / No Fake Fallbacks:** Full cloud provisioning wizards are explicitly gated on physical account access and Windows execution evidence. Templates are nondeployable feasibility source samples; fake provider fallback classes and fabricated model/recipe strings have been completely removed.
5. **Real Account Verification is a P1 Prerequisite:** Live account checks are mandatory prerequisite gates within P1 before building any provisioning wizard or writing deployment code, not deferred to later phases.
6. **No Cleanup Guarantee Until Observed:** Teardown procedures are defined programmatically, but explicitly recognized as unobserved and unverified until executed and validated against an authorized live test account.

---

## 2. Pinned Released SDK Candidate Matrix

All metadata, version tags, file sizes, and SHA256 hashes below were verified directly from official PyPI JSON endpoints (`https://pypi.org/pypi/{package}/json`) on 2026-09-19:

| Property | Modal Candidate | Beam Candidate | Beta9 Candidate (Beam Core Dep) |
|---|---|---|---|
| **Package Name** | `modal` | `beam-client` | `beta9` |
| **Pinned Released Version** | `1.5.5` | `0.2.211` | `0.1.268` |
| **Release Upload Date** | 2026-08-28T19:51:32Z | 2026-09-15T18:10:38Z | 2026-09-15T17:51:18.988082Z |
| **Python Version Support** | `<3.15, >=3.10` | `<4.0, >=3.8` | `<4.0, >=3.8` |
| **PyPI URL** | [pypi.org/pypi/modal/1.5.5/json](https://pypi.org/pypi/modal/1.5.5/json) | [pypi.org/pypi/beam-client/0.2.211/json](https://pypi.org/pypi/beam-client/0.2.211/json) | [pypi.org/pypi/beta9/0.1.268/json](https://pypi.org/pypi/beta9/0.1.268/json) |
| **Official Documentation** | [modal.com/docs/sdk/py/latest/App](https://modal.com/docs/sdk/py/latest/App) | [docs.beam.cloud/v2/getting-started/installation](https://docs.beam.cloud/v2/getting-started/installation) | [pypi.org/project/beta9](https://pypi.org/project/beta9/) |
| **Wheel Filename** | `modal-1.5.5-py3-none-any.whl` | `beam_client-0.2.211-py3-none-any.whl` | `beta9-0.1.268-py3-none-any.whl` |
| **Wheel Size (Bytes)** | 985,163 | 10,935 | 318,857 |
| **Wheel SHA256** | `8d10d3ee09818aaba1973b73ce2521ab8961b63a29b5b52e3ff0d25e7a74808e` | `ffa8c9bb086d85ad1bff07bf8ebde65d5e122a4df1469d1aec56598ddf44c561` | `eb21e48b9ae0871449b2434b2e735024a59298ffce64953a066d9d36b8358a75` |
| **Source Tarball (sdist)** | `modal-1.5.5.tar.gz` | `beam_client-0.2.211.tar.gz` | `beta9-0.1.268.tar.gz` |
| **sdist SHA256** | `30df363ed1898cc3d91a09ff3f95c38ab043f6b6294011b01085312c6a0ac777` | `8ca975ce814ada1100d737764024c7f489ac05a72cba3265517ee6452ff7daed` | `693e4f65771e40d925946f149d70eedb82baea846de351567a383b7dd11a4b96` |
| **Platform Compatibility** | macOS / Linux / Windows: **Source-compatible / unverified** (Python >= 3.10 gate) | macOS / Linux: **Source-compatible / unverified**; Windows: **Unverified / blocked** | macOS / Linux / Windows: **Source-compatible / unverified** |

---

## 3. Source-Verified APIs vs. Click CLI Wrappers vs. Hosted Behaviors

> **Superseded by the drivers.** The candidate matrix (`provisioner/matrix.py`) is removed. [`provisioner/modal_driver.py`](../provisioner/modal_driver.py) makes the Modal calls below; [`provisioner/beam_driver.py`](../provisioner/beam_driver.py) talks to the beta9 gRPC stubs over an explicit channel instead of `beam.client.client.Client`, and uses the user's own key as the runtime credential instead of a restricted token.

### 3.1 Modal Provider Analysis
- **Callable SDK Methods (Source-Verified in `modal-1.5.5`):**
  - [`modal.Client.from_credentials(token_id, token_secret) -> _Client`](../provisioner/modal_driver.py): Callable client constructor isolating credentials in memory. Avoids writing to `~/.modal.toml`.
  - [`modal.App.deploy(client=client, name=..., ...)`](../provisioner/modal_driver.py): Callable programmatic deployment accepting an explicit injected client.
  - [`modal.Workspace.from_context(client=client) -> _Workspace`](../provisioner/modal_driver.py): Callable workspace inspection.
  - `workspace.proxy_tokens.create()`, `allow()`, `revoke()`, `delete()`: Callable webhook proxy token lifecycle management.
- **Source CLI Commands (Click Command Wrappers):**
  - `modal token set`: Click command wrapper that mutates `~/.modal.toml`. MUST NOT be used by provisioner.
  - `modal app deploy` / `modal app stop`: Click command CLI entry points for shell usage.
- **Unverified Hosted Behaviors (P1 Prerequisite Gate):**
  - Real server-side enforcement of proxy token authorization against `/mc/v1` web endpoints in Modal's hosted proxy.
  - Custom domain routing and deterministic URL generation for persistent named apps.

### 3.2 Beam / Beta9 Provider Analysis
- **Callable SDK Methods (Source-Verified in `beam-client-0.2.211` / `beta9-0.1.268`):**
  - `beam.client.client.Client(token=...)`: Callable client constructor inheriting from `beta9.client.client.Client`.
  - `beta9.config.SDKSettings(api_token=os.getenv("BEAM_TOKEN"))`: Callable settings initialization reading token from environment variable without touching disk.
- **Source CLI Commands (Click Command Wrappers, NOT Callable SDK API):**
  - `beam configure`: Click Command wrapper in `beam/cli/configure.py` calling `save_config(...)` to mutate `~/.beam/config.ini`. MUST NOT be invoked.
  - `beta9.cli.deployment.delete_deployment` / `list_deployments`: Click Command wrappers designed for CLI subprocess invocation.
  - `beta9.cli.token.create_token` / `delete_token`: Click Command wrappers accepting `--token-type`.
  - `beta9.cli.config.list_contexts` / `export_config`: Click Command wrappers that expose tokens via `--show-token` or JSON dumps.
- **Unverified Hosted Behaviors (P1 Prerequisite Gate):**
  - Actual hosted operations permitted vs denied for `workspace_restricted` tokens. **Never infer hosted token scope from enum names.**
  - Task queue retry semantics and presigned output URL retention duration.

---

## 4. Platform Packaging Realities

> **Superseded by the drivers.** The helper now ships frozen with its own Python 3.11 (see [cloud-provisioning.md](cloud-provisioning.md), section 7), and the Beam refusal on native Windows is dropped because the deploy path uses no POSIX-only code. Native Windows remains unverified live.

| Platform / OS | Status | Verified Reality & Constraints |
|---|---|---|
| **macOS (Apple Silicon)** | Source-compatible / unverified | Pure Python wheels are installable. **Runtime Gate:** `modal` requires Python `>= 3.10`. The macOS host system Python is `/usr/bin/python3` (Python 3.9.6). Provisioner packaging must bundle a dedicated Python 3.10+ runtime; it cannot rely on macOS system Python. |
| **Linux (x86_64)** | Source-compatible / unverified | Both `modal` and `beam-client` wheels are `py3-none-any` pure wheels; standard Linux glibc runtime with Python >= 3.10 is required for execution verification. |
| **Windows (x64)** | **UNVERIFIED / BLOCKED** | **Beam Documentation Invariant:** Upstream Beam documentation ([docs.beam.cloud/v2/getting-started/installation](https://docs.beam.cloud/v2/getting-started/installation)) explicitly instructs Windows users to install via WSL (`Ubuntu-22.04`). Native Windows packaging and CLI subprocess execution remain **unverified without native Windows execution**. The provisioner must not claim native Windows support until proven on a physical Windows test host. |

---

## 5. CPU-Only Local Introspection & Source-Packaging Probe

> **Superseded by the drivers.** `provisioner/probe.py` and `provisioner/templates/` are removed. The deployable apps are [`deploy/cloud/modal/app.py`](../deploy/cloud/modal/app.py) and [`deploy/cloud/beam/app.py`](../deploy/cloud/beam/app.py), and the packaging check is the frozen helper's `--self-check` run by [`.github/scripts/build-cloud-provisioner.py`](../.github/scripts/build-cloud-provisioner.py). The record below describes the removed probe.

Implemented in `provisioner/probe.py` (removed), the probe tests trusted cloud templates as nondeployable feasibility source samples.

### 5.1 Probe Safeguards & Mechanism
1. **Pre-Read Size Guard:** Inspects `stat().st_size` *before* reading file bytes. Files exceeding `100 KiB` are rejected immediately without content allocation.
2. **Mandatory Provider Templates:** Enforces presence of both `modal/app.py` and `beam/app.py`. Missing templates fail `all_templates_valid`.
3. **Full AST Traversal:** Walks the entire syntax tree to detect prohibited GPU modules (`torch`, `diffusers`, `cuda`, etc.) whether declared at top-level or nested inside `if`, `try`, or helper functions.
4. **Weight & Binary Guard:** Scans staging tree for prohibited extensions (`.safetensors`, `.bin`, `.pt`, `.whl`).
5. **Real UTC Timestamps:** Generates ISO 8601 UTC timestamps with timezone offset.
6. **AST Static Analysis Limitation Disclosure:** Static AST analysis proves syntax and absence of explicit import statements. It does *not* prove zero runtime side effects if dynamic execution (`__import__`, `importlib`, C-extension init) occurs.

### 5.2 Verification Evidence
- **Log URI:** `/tmp/manga-cloud-p1-fixed-probe.log`
- **Templates Verified:**
  - `provisioner/templates/modal/app.py` (removed): Valid syntax, 0 prohibited imports, minimal health stub present, sha256 `46a6433933eb6ef355f4c848c025025745fa4688b8a017e68d8558fdc092ebf3` (936 bytes).
  - `provisioner/templates/beam/app.py` (removed): Valid syntax, 0 prohibited imports, minimal health stub present, sha256 `9667d233956e3f93a1ea05a3209de6e63587a13c9f43d38967f7490c7acce922` (919 bytes).

---

## 6. Minimal CPU-Only Staging & Cleanup Plans

> **Superseded by the drivers.** `provisioner/plans.py` is removed. The `plan` operation of [`provisioner/modal_driver.py`](../provisioner/modal_driver.py) and [`provisioner/beam_driver.py`](../provisioner/beam_driver.py) now plans the full installation (weights volume, GPU worker, CPU gateway, job state, runtime credential) and [`provisioner/controller.py`](../provisioner/controller.py) plans cleanup from the journal. The CPU-only staging plans below are the P1 record.

Formulated in `provisioner/plans.py` (removed):

### 6.1 Invariants Maintained
- **Strictly CPU-Only:** Zero GPU allocation, zero model weights, zero GPU idle retention settings.
- **Zero Global Profile Writes:** No mutations to `~/.modal.toml` or `~/.beam/config.ini`.
- **Real Account Checks as P1 Prerequisites:** Live account verification of proxy/restricted token scoping is a prerequisite gate before any deployment wizard or production code is built.
- **No Cleanup Guarantee Until Observed:** Cleanup is explicitly cataloged as unobserved and unverified until executed and observed on an active account.
- **Docstrings Do Not Assert Auth:** Unauthenticated health endpoints in templates serve solely as control-reachability stubs; live credential exchange is required for real authentication.

### 6.2 Provider Plans Summary
- **Modal Plan (`modal-cpu-offline-staging-v1`):**
  - Resource: CPU `0.125`, Memory `128 MiB`, GPU `None`, Weights `None`, Endpoint `/mc/v1/health`.
  - Auth: `Client.from_credentials(token_id, token_secret)` in-memory.
  - Token Lifecycle: `workspace.proxy_tokens.create()` scoped via `allow()`, verified on `/health`, revoked, and deleted.
  - Cleanup: Delete proxy token, call `client.app_stop()`, inspect container count is 0.
- **Beam Plan (`beam-cpu-offline-staging-v1`):**
  - Resource: CPU `0.5`, Memory `512 MiB`, GPU `None`, Weights `None`, Endpoint `/mc/v1/health`.
  - Auth: `BEAM_TOKEN` environment variable in-memory; zero calls to `beam configure`.
  - Token Lifecycle: Request `token_type="workspace_restricted"`, probe `/health`, negative test deployment mutation, delete token.
  - Cleanup: Delete deployment, delete token, verify `~/.beam/config.ini` unchanged.

---

## 7. Test Suite Execution & Evidence Logs

> **Superseded by the drivers.** `provisioner/test_feasibility.py` is removed with the probe. The current tests are listed in [cloud-provisioning.md](cloud-provisioning.md), section 8.

Automated offline tests in `provisioner/test_feasibility.py` (removed) verified matrix metadata, AST introspection, packaging failure modes, and plan invariants:

- **Command Executed:** `python3 provisioner/test_feasibility.py`
- **Total Tests:** 19
- **Passed:** 19 (0 failures, 0 errors)
- **Execution Duration:** 0.016s
- **Logs:** `/tmp/manga-cloud-p1-fixed-probe.log` and `/tmp/manga-cloud-p1-fixed-test.log`

### Failure Modes Tested
1. `test_failure_mode_missing_required_template`: Rejects staging directory when required provider template (`beam/app.py`) is missing.
2. `test_failure_mode_pre_read_oversized_file`: Rejects >100 KiB file before reading content into memory.
3. `test_failure_mode_nested_gpu_import`: Detects prohibited `import torch` nested inside functions or conditional blocks.
4. `test_failure_mode_prohibited_weight_artifact`: Rejects `.safetensors` weight artifacts in staging tree.
5. `test_failure_mode_syntax_error`: Catches invalid Python syntax.
6. `test_real_utc_timestamp_generated`: Verifies probe log timestamp is valid ISO 8601 UTC.
7. `test_beta9_candidate_metadata`: Verifies beta9 0.1.268 wheel SHA256 `eb21e48b9ae0871449b2434b2e735024a59298ffce64953a066d9d36b8358a75`.
8. `test_platform_status_labeled_unverified`: Verifies no platform is claimed supported without empirical test evidence.
9. `test_cli_wrappers_labeled_as_commands`: Verifies Click CLI command wrappers are distinguished from callable SDK APIs.
10. `test_cpu_only_no_gpu_or_retention`: Verifies zero GPU and zero retention parameters in CPU plans.
11. `test_p1_prerequisites_and_unobserved_cleanup`: Verifies real account checks are P1 prerequisites and cleanup is unobserved.

---

## 8. Remaining Acceptance Gates & Explicit Blockers

> **Current state:** gate 1 is met by the frozen helper, which bundles Python 3.11. Gate 2 is reduced to live verification: the Windows refusal is dropped. Gates 3 and 4 remain open and are listed under "Not verified live" in [cloud-provisioning.md](cloud-provisioning.md); the Beam driver uses the user's own key, so no restricted token is created.

1. **macOS Python Interpreter Gate:** macOS system Python (`/usr/bin/python3`) is 3.9.6. `modal-1.5.5` requires Python `>= 3.10`. Packaging must bundle an isolated Python 3.10+ runtime.
2. **Windows Native Packaging Blocker:** Beam officially documents Windows setup exclusively through WSL (`Ubuntu-22.04`). Native Windows packaging remains unverified without Windows test execution. Manual endpoint connection must remain available.
3. **Hosted Token Least-Privilege Gate (P1 Prerequisite):** Live negative tests on an authorized account must prove that proxy/restricted tokens are rejected from deployment and workspace mutations before declaring least-privilege tokens.
4. **Live Cleanup Verification Gate (P1 Prerequisite):** Live account execution must empirically observe and confirm complete resource deallocation in vendor dashboards after test execution.
