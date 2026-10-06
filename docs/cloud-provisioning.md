# Manga Cleaner Cloud Provisioning Helper

**Status:** Implemented and tested offline against fake SDKs and the real SDK source (`modal` 1.5.5, `beam-client` 0.2.211, `beta9` 0.1.268). Nothing has run against a live Modal or Beam account; see [Not verified live](#9-not-verified-live).

The helper in `provisioner/` turns a pasted Modal token or Beam API key into a working `/mc/v1` endpoint in the user's own account: a weights volume seeded by a CPU job, a GPU worker, a CPU gateway behind the provider's edge auth, durable job state and a runtime credential. The desktop (`src-tauri/src/provision.rs`) runs it once per operation. The deployed code is `deploy/cloud/`; the wire contract is [cloud-contract.md](cloud-contract.md).

## 1. Process and protocol

The helper runs as `python -m provisioner` in a checkout or as the frozen `manga-cleaner-provisioner` binary. It reads one JSON request of at most 64 KiB from stdin and writes exactly one JSON document to stdout. stderr carries IC-2 progress lines and nothing else. Before any SDK is imported, `cli.main` keeps private copies of fd 1 and fd 2 and points the process-level stdout and stderr at the null device, so SDK prints, logging, warnings and C extension output never reach the desktop.

The CLI also removes the Modal and Beam token and profile variables from its environment and points `MODAL_CONFIG_PATH` and beta9's `CONFIG_PATH` at a private directory that is deleted at exit. The app starts the helper with a cleared environment: only the variables in `HELPER_ENV_ALLOWLIST` (`src-tauri/src/provision.rs`) get through, so a `MODAL_SERVER_URL`, `MC_BEAM_GATEWAY_HOST`, `PYTHONPATH` or token variable the app was started with never reaches a run that holds a pasted key. Direct CLI runs, like the tests, still honour `MODAL_SERVER_URL` and `MC_BEAM_GATEWAY_*`. The helper never reads or writes `~/.modal.toml` or `~/.beam/config.ini` and never prompts. Credentials arrive only in `params.credentials`, stay in memory and are redacted from everything the request writes. The journal root is `--journal-root DIR`, else `MANGA_CLEANER_JOURNAL_ROOT`, else `~/.manga-cleaner/cloud/journal`.

```json
{"protocol_version": "1.0.0", "request_id": "req-1", "op": "plan", "provider": "modal",
 "params": {"credentials": {"token_id": "ak-...", "token_secret": "as-..."}, "installation_id": "mc-ab12cd", "options": {"gpu": "L4"}}}
```

A response is `{"protocol_version", "request_id", "success": true, "data"}` or `"success": false` with `"error": {"code", "message", "actionable_guidance", "remedy_steps"}`.

| Op | Params | Budget | What it does |
|---|---|---|---|
| `inspect` | `credentials` | 50 s | Signs in and reads the workspace (and the Modal environment), and lists the installations already there (section 4.1). |
| `plan` | `credentials`, `installation_id`, `options` | 50 s | Read only: `resources_to_create`, `resource_allocation`, costs, `plan_hash`. |
| `apply` | as `plan`, plus `approved_plan_hash`, `forget_setup_credential` | 27 min | Runs the pipeline; returns IC-1. |
| `resume` | `credentials`, `installation_id`, `options`, `forget_setup_credential` | 27 min | Continues the journaled pipeline; returns IC-1. |
| `cleanup_plan` | `installation_id` | 50 s | Read only, no credentials: what cleanup deletes. |
| `cleanup_apply` | `credentials`, `installation_id`, `approved_cleanup_plan_hash`, `confirm_delete_persistent_storage` | 25 min | Deletes the journaled resources. |
| `probe_compatibility` | `endpoint_url`, `runtime_credential` | 50 s | Checks `/health` and `/model-info` through the edge. |
| `forget_credential` | `installation_id` | 50 s | Records that the desktop dropped the setup key. |

Budgets stay under the desktop's kill timers (60 s and 30 min). `options` accepts `gpu`, from the provider allowlist, `idle_seconds`, 60 to 600 (default 120), `model_id` (one of `PRODUCTION_MODELS` in deploy/cloud/common/manifest.py: FLUX.2 Klein 4B, the default; Klein 9B; Qwen-Image-Edit-2511), `analysis_models`, `denoise` and, on Modal, `routing_region` (`us-east`, the default, `us-west`, `eu-west` or `ap-south`: where Modal's proxy for the gateway sits). A model with a required GPU (`required_gpu` in its spec: L40S on Modal and RTX5090 on Beam for Klein 9B and for Qwen) gets it by default and refuses any other. Modal credentials are an API token (`ak-`/`as-`); a proxy token (`wk-`/`ws-`) is refused. Beam credentials are `{"token": ...}`.

Every failure is typed: `ERR_VALIDATION_ERROR` (bad params, missing credentials, a stage that forbids the op), `ERR_UNAPPROVED_PLAN` (hash mismatch, plan drift, another account), `ERR_ACTIONABLE_MISSING_PERMISSION` (the provider refused the key), `ERR_PROVIDER_UNAVAILABLE` (SDK missing, or the provider unreachable before any change), `ERR_EXECUTION_FAILED` (a step failed; Resume continues), `ERR_EXECUTION_TIMEOUT` (the budget ran out; Resume continues), `ERR_SECURITY_VIOLATION` (symlinked journal, unsafe URL), and the envelope codes `ERR_INVALID_REQUEST_PAYLOAD`, `ERR_PAYLOAD_TOO_LARGE`, `ERR_INVALID_PROTOCOL_VERSION`, `ERR_UNSUPPORTED_OPERATION` and `ERR_UNSUPPORTED_PROVIDER`. No platform refusal remains: both providers run on macOS and Windows.

## 2. IC-1 result

A successful `apply` or `resume` returns:

```json
{"installation_id": "mc-ab12cd", "provider": "modal", "stage": "completed",
 "endpoint_url": "https://<host>/mc/v1",
 "runtime_credential": {"kind": "modal_proxy", "token_id": "wk-...", "token_secret": "ws-..."},
 "gpu": "L4", "idle_seconds": 120,
 "model": {"model_id": "...", "model_revision": "...", "recipe_id": "...", "preprocessing_version": "..."},
 "compatibility_status": "compatible", "resources_created": [{"type": "volume", "name": "mc-ab12cd-weights"}],
 "setup_credential_forgotten": true}
```

Beam returns `{"kind": "beam_bearer", "token": "<the user's key>"}` as `runtime_credential`. The response is redacted like any other, then `runtime_credential` and `endpoint_url` are merged back verbatim (`HelperResponse.verbatim` in `protocol.py`). This carve-out exists only in this response; the desktop moves the credential into the OS keyring and adds `profile`, `health` and `selected`.

The Modal setup key (`ak-`/`as-`) can stay on the computer too, because every update redeploys and needs it again. The webview sends two fields the helper never sees (`take_setup_key` in `provision.rs`): `remember_setup_credential: true` keeps the pasted key after a successful `apply` or `resume`, in the keyring under the profile's setup role; `saved_setup_credential: "<profile id>"` asks the desktop to put that saved key into `credentials` for `inspect`, `plan`, `resume` or `cleanup_apply`. A missing or unreadable saved key is `ERR_VALIDATION_ERROR` before the helper starts. The desktop sets `forget_setup_credential` to match, so the journal records whether the key was kept. A run with a pasted key and no `remember_setup_credential` deletes any saved key; removing the endpoint deletes it with the runtime token. `CloudProvisioner` ticks "Remember" by default, uses the saved key on Resume, Update and Clean up of that setup without showing the key fields, and shows them again when the saved key is refused. Beam is paused, and its runtime credential already is the user's key.

## 3. IC-2 progress

Each line on stderr is one record: `{"mc_progress":1,"op":"apply","step":"deploy","state":"start","pct":null}`. States are `start`, `done`, `fail` and `skip`. `pct` is null or 0 to 100: `weights` reports the download, `cleanup` reports each deleted resource, and a step that reported a percentage ends `done` at 100.

- `inspect`: inspect. `plan`: inspect, validate.
- `apply` and `resume`: inspect, validate, then volume, state, secret, image, deploy, weights, token, endpoint, health in that order.
- `cleanup_plan`: validate. `cleanup_apply`: validate, inspect, cleanup.
- `probe_compatibility`: health. `forget_credential`: none.

A step the provider does not have is `skip` (Modal: secret; Beam: token), and so is a step the journal records as done. Modal repeats token and health on every run; Beam repeats secret and health. A request refused before sign-in (for example missing credentials) emits no lines.

## 4. Stages and the journal

The journal is `<root>/installations/<installation_id>/journal.json`: mode 0600, written through a temporary file, fsync and rename, refused when it or its directory is a symlink, capped at 256 KiB, and checked on load for provider, installation id, stage, hash shapes and the `managed_by: manga-cleaner` tag on every resource. On save, secret-named keys and every secret value of the current request are redacted everywhere; token-shape guessing runs only over `last_error`, so ids, names and the endpoint URL round-trip exactly.

Stages run `planned`, `deploying`, `deployed`, `seeding`, `seeded`, `credential_created`, `discovered`, `validated`, `completed`, with `failed` beside them and `cleanup_planned`, `cleaned_up` after them. volume, state, secret and image run in `deploying`; deploy ends in `deployed`; weights moves through `seeding` to `seeded`; token is `credential_created`; endpoint is `discovered`; health is `validated`. `completed` is reachable only from `validated`.

Resume never repeats a side effect:

- A resource is recorded under its fixed name before the create call, and the creates are idempotent by name (a redeploy replaces the same app or deployment), so a kill between the two is harmless. The proxy token is the exception (see section 9). A step is marked done only after its side effect, and done steps are skipped.
- The seed job is journaled twice: the intent (`seed_started_at`, a wall-clock second) before the spawn or enqueue, then its id (Modal FunctionCall id, Beam task id). Resume waits on a job that may still run instead of starting a second download. The provider's status decides when it has one (Modal: the call result, Beam: the task status); otherwise the status document does, whose `updated_at` the job rewrites every 5 s (10 s at most between attempts). A job is gone when the provider says it ended without `done` (Beam `COMPLETE`, Modal expired call), when its document kept the same heartbeat for 60 s, or when no document appeared within 180 s of the intent. A gone job is replaced once per run; a second one fails with `ERR_EXECUTION_FAILED` and clears the intent and id, so the next Resume starts afresh. A failed job starts again on Resume, and the download resumes partial files. Both seed queues run one job at a time, so a replacement never downloads beside its predecessor.
- The journal is created with the account id and environment in its first save. A journal without them (written by an older helper) gets them on apply or resume once the plan hash matched.
- `apply` refuses an existing journal with another plan. `resume` rebuilds the plan from the journal's approved options and the current account and must match both stored hashes, so another account or other options fail with `ERR_UNAPPROVED_PLAN` before any change.
- Resume at `completed` reruns the credential steps and health and returns a fresh IC-1. Modal also repeats image and deploy (`repeat_steps`), so an installation made by an older release gets this release's gateway; `App.deploy` is idempotent and reuses built images, and the weights are not touched. Modal keeps the old proxy token recorded and usable while it creates, allows, and health-checks the replacement, then revokes the old token. Beam stays at `completed` and stores the key it was given again.
- A failure moves to `failed` with a redacted `last_error`. Any stage short of `cleanup_planned` resumes. `apply` against `completed`, `cleanup_planned` or `cleaned_up` is `ERR_VALIDATION_ERROR` and changes nothing.

### 4.1 Existing installations and taking one over

The journal is the only link between a computer and what it created. When it is lost (a new computer, a reinstall that cleared app data, a second machine on the same account), a fresh setup would otherwise make a second app and volume and download the weights again, with the storage billed twice.

`inspect` therefore also answers `existing_installations` and `existing_installations_complete`. Only `inspect` does this (`BaseProviderDriver.discover`), so `plan`, `apply` and `resume` pay nothing for it. On Modal, read only:

- `modal.experimental.list_deployed_apps(environment_name, client)` for apps named like an installation (`mc-` and the `installation_prefix` slug), then `Volume.objects.list(max_objects=500, ...)` for their `<app>-weights` volumes. An app without its volume is not listed.
- For each, newest first by the volume's `created_at`, at most 10: the model ids and analysis graphs whose ready markers are on the volume (`Volume.listdir("/")`, then `listdir("analysis", recursive=True)` when that directory exists; the seed writes each marker last, and a listing transfers no weights), the choices the installation recorded (`install:options` in its `-jobs` Dict, validated through `normalize_options`, else null), and `deployed_at` from `get_app_lifecycle`. Each read that fails leaves its field empty instead of failing inspect; a listing that fails answers an empty list with `existing_installations_complete: false`, and so does a list cut short by the limit or the budget (8 s kept back per installation).
- `on_this_computer` is added by the controller: this computer's journal for that id exists, can resume, and records the same account. The desktop offers Resume for those, since `apply` refuses a journal at `completed`.

Beam answers `[]` with `existing_installations_complete: false`: beta9 lists deployments by exact name filter only, and whether an unfiltered listing is supported is not verified, so it is not attempted.

An entry's `installation_id` equals its app name, and `installation_prefix` of such an id is the id itself, so an `apply` with that id plans the same volume, dict and app names. With no journal on this computer the plan hash is new and `apply` accepts it (`journal.initialize`); volume and dict creation pass `allow_existing=True` and `App.deploy` redeploys the same app. The weights step then checks the volume before it does anything else: when no seed of this journal's is in flight and the ready markers cover the chosen model and every chosen analysis graph, the step finishes without a seed job. Otherwise it starts one, which skips what is verified on the volume and downloads only what is missing (another model lands beside the first in its own snapshot directory). The token step creates a new proxy token for this computer; the previous computer's token is not in this journal, so it stays live until removed in Modal (Settings, Proxy Auth Tokens).

Once the weights are in place, every Modal apply and resume writes `install:options` (`schema`, `gpu`, `idle_seconds`, `model_id`, `analysis_models`, `denoise`, `routing_region`, `recorded_at`) into the installation's Dict, best effort, so a later discovery can offer the same choices. Installations made before this was added list `options: null`.

`provision.rs` rebuilds the list for the webview from allowlisted fields (`sanitize_existing_installations`): an id that is not an installation app name or does not equal `app_name` drops the entry, timestamps must be unix seconds before 2100, model ids must be `owner/name`, capabilities come from the fixed two, `options` must be all four valid values or null, at most 20 entries, and a dropped entry makes the list incomplete.

In the app, `CloudProvisioner` shows found installations after Connect, before any plan: "Use this setup (no new download)" plans with that id and the recorded choices, and the Review step says what is reused and whether anything downloads; "Set up a new one" stays beside it and says it downloads the models again and adds storage cost. A setup opened on a computer whose `inference.json` already has an endpoint setup made points to it first ("You already have a cloud GPU"), with Update (Resume) as the primary action and a new setup as an explicit second choice. The Vite mock shows the flow with `?cloudExisting=1` (one installation) or `?cloudExisting=2` (plus one whose download did not finish).

## 5. Cleanup

`cleanup_plan` lists exactly the journal's resources, in provider order (Modal: proxy token, app, dict, volume; Beam: gateway, workers, secret, map, volume), with a hash, and calls no provider. Nothing outside the journal is looked at, so `foreign_resources_ignored` is always empty. Its Modal `notes` explain that tokens issued on other computers for an adopted installation may remain active. Review Proxy Auth Tokens in Modal Settings and revoke any older tokens you recognize separately. Cleanup never revokes an unrecorded token.

`cleanup_apply` needs that hash and, when a volume is listed, `confirm_delete_persistent_storage: true`. It checks that the key belongs to the account that installed (a journal that does not record its account is refused until a Resume records it), moves to `cleanup_planned` (never resumed afterwards) and deletes one resource at a time, dropping each from the journal. A resource already gone counts as `missing`. An interrupted cleanup is finished by a new `cleanup_plan` and `cleanup_apply` (its error says so); the last one ends at `cleaned_up`. A Beam map has no delete call of its own, so cleanup deletes every key, lists the map again and repeats up to three times; keys that remain, or a listing Beam refuses, fail the cleanup instead of counting as `missing`. A map that lists no keys counts as `missing`. The Beam key is not a resource and is never deleted.

## 6. Providers

Every name starts with the installation prefix, a short `mc-` slug of the installation id.

**Modal** (`provisioner/modal_driver.py`, `deploy/cloud/modal/`)

- Creates volume `<prefix>-weights`, dict `<prefix>-jobs`, app `<prefix>` (`gateway`, `Worker`, `seed_weights`) and one proxy token (journaled as `<prefix>-proxy-token` with its `wk-` id).
- GPU allowlist `L4`, `A10`, `L40S`; default `L4`, the cheapest 24 GB class ($0.80/h list on 2026-09-23).
- Worker: 2 CPU, 12 GiB, `max_containers=1`, `scaledown_window` = `idle_seconds`, job timeout 600 s, `startup_timeout` 1200 s for the 5.5 GB load.
- Gateway: 0.25 CPU, 512 MiB, one container, 32 concurrent inputs, `requires_proxy_auth=True`, 300 s per request. Modal's edge checks `Modal-Key` and `Modal-Secret`; the gateway trusts the edge and holds no copy.
- Routing region: the app's requests and the results it downloads enter Modal through the proxy of the chosen `routing_region` (`ROUTING_REGIONS` in `deploy/cloud/common/deployment.py`). Only the gateway is routed: its container and the GPU classes stay where Modal places them, and a Function outside `us-east` cannot be started with `.spawn`, which is how the gateway starts the GPU classes. Modal fixes a Function's routing region at its first deploy, so `us-east` keeps the Function `gateway` (deployed with no routing argument, as before the option) and every other region deploys its own Function, `gateway_<region>` (`ModalSettings.gateway_function`), with its own URL: `https://<workspace>--<prefix>-gateway-ap-south.ap-south.modal.run`. Choosing another region on an existing installation is an update like any other option: the redeploy removes the old Function, the endpoint moves to the new origin and the desktop re-keys its token to it. Attempts still journaled against the old origin are not resumed.
- Result downloads resume: `GET /mc/v1/jobs/{handle}/result` answers `Range: bytes=N-` with 206 and the bytes from N on (any other `Range` form is ignored and gets the whole result). The desktop asks again for what is missing when a transfer stops (`fetch_result_bytes_while` in `src-tauri/src/inference/http.rs`), gives up after three requests in a row that add nothing or after ten minutes, and checks the digest of the joined result. A gateway deployed before this answers 200 with the whole result, which the desktop takes as a restart.
- Analysis tiles and denoise pages are jobs too: Modal answers a web request past 150 s with a 303 redirect, which the desktop refuses, so the app submits each one (`POST /mc/{analysis,denoise}/v1/jobs`, with an attempt id), polls `GET .../jobs/{handle}` and cancels with `POST .../jobs/{handle}/cancel`, each request 90 s at most. The gateway spawns the `AnalysisGPU` call and keeps one `gpujob:<handle>` document per job in the Dict; the handle hashes the request digest and the attempt id, so a repeated submit spawns nothing. It lists `analysis.jobs` and `denoise.jobs` in `/mc/v1/capabilities`; an app talking to a gateway without them (deployed by an older release) uses the synchronous `/mc/analysis/v1/analyze` and `/mc/denoise/v1/page`, which now wait 120 s, then cancel the call and answer 504. A Resume redeploys the gateway (see above), which is what brings the job routes to an older installation.
- A page's analysis tiles go as batches when the gateway lists `analysis.batches`: one job (`POST /mc/analysis/v1/batches`, polled at `.../batches/{handle}`) carries up to 16 requests of one page, each distinct tile PNG once, and the `AnalysisGPU.analyze_batch` call answers every request in order. A tile job costs about 2.3 s of spawn, queue and Dict work against 0.05 to 0.7 s of GPU work, so a 1080x1536 page (four tiles, two models) went from eight jobs to one. A gateway without the operation (older, or Beam) still gets one job or request per tile. The gateway also takes a lock around every weights-volume reload, because a reload fails while another request thread has a graph open.
- Runtime credential: `Workspace.proxy_tokens.create()`, then `allow(token_id, environment)` for the resolved environment. This grants access to endpoints in that environment, not just this app. When `allow` fails (a workspace without per-environment access control), setup continues and the health step shows whether the edge accepts the token; the token may have workspace-wide endpoint access. On Resume the previous token stays live until the replacement passes health. An adopted installation can also have tokens issued on other computers.
- Job state: one document per handle in the Dict, so a restarted gateway still answers for every job and health never starts a GPU.
- Weights: `seed_weights` (2 CPU, 4 GiB, timeout 3600 s) downloads the pinned snapshot (5,475,930,180 bytes) into the volume and publishes progress under `seed:state` in the Dict. Apply spawns it and polls every 5 s, keeping 240 s for the later steps; past that it returns `ERR_EXECUTION_TIMEOUT`. With no seed of the journal's in flight, apply first lists the volume's ready markers and skips the job when they cover the chosen model and analysis graphs (section 4.1).
- Steps: volume and state create by name, image is `App.deploy` (builds the images and deploys), deploy looks up the three objects, endpoint is the gateway's `get_web_url()` plus `/mc/v1`. Sign-in is bounded at 40 s.

**Beam** (`provisioner/beam_driver.py`, `deploy/cloud/beam/`)

- Creates volume `<prefix>-weights`, map `<prefix>-jobs` (type `dict`), secret `MC_<PREFIX>_TOKEN`, task queues `<prefix>-seed` and `<prefix>-worker` (type `worker`) and the ASGI app `<prefix>-gateway` (type `gateway`).
- GPU allowlist `RTX4090`, `A10G`, `RTX5090`; default `RTX4090`. beta9's `GpuType` lists `L4`, but Beam documents only T4, A10G, RTX4090 and RTX5090 for serverless, and RTX4090 is the cheapest 24 GB class there ($0.69/h list on 2026-09-23; A10G and RTX5090 have no listed price). `DEFAULT_GPU` in `deploy/cloud/beam/settings.py` is the one line to change.
- Worker: 1 CPU, 12 GiB, one worker and one container, `keep_warm_seconds` = `idle_seconds`, job timeout 600 s, `retries=0`. The pipeline loads in `on_start`; the beta9 task queue has no separate startup timeout.
- Gateway: 0.5 CPU, 1 GiB, kept warm 120 s, `authorized=True`. Beam's edge checks `Authorization: Bearer <key>`; the gateway trusts the edge.
- Runtime credential: the user's own key, returned as `beam_bearer`. The SDK has `create_token`, but whether a restricted token works for `authorized=True` endpoints cannot be proven offline, so no token is created, listed or deleted.
- The secret holds the user's key. The gateway uses it to enqueue GPU jobs over HTTP and to stop tasks; the secret step stores the key given in each run.
- Job state: one document per handle in the Map.
- Weights: the seed task queue (2 CPU, 4 GiB, timeout 3600 s). Apply posts to its URL with the key, journals the task id and polls `seed:state` in the Map. The worker looks for the ready marker four times, 30 s apart, before it reports `weights_missing`.
- Steps: volume is `get_or_create_volume`; state writes an ownership key into the map; image deploys the seed and worker task queues; deploy deploys the gateway with the worker URL; endpoint is the gateway invoke URL plus `/mc/v1` (a path-style URL is refused). The SDK talks to `gateway.beam.cloud:443` over an explicit channel carrying the key; its terminal output is captured.
- `stage.py` copies `mc_beam_app.py` to the root of a temporary directory with the needed deploy files and deploys with that directory as the working directory, so module names stay flat on every platform.

On both providers a worker that finds no seeded snapshot fails the job with the typed `weights_missing`. The Beam worker stays up and loads on a later job; the Modal worker refuses to start, so no memory snapshot is taken without the model loaded, and the next cold start tries again ([cloud-api.md](cloud-api.md) section 5).

## 7. Packaging

`.github/scripts/build-cloud-provisioner.py <target> [--dist DIR] [--work DIR] [--no-config]` freezes the helper with PyInstaller 6.22.3 into the onefile `manga-cleaner-provisioner-<target>`. It collects `modal`, `modal_proto`, `modal_version`, `beam`, `beta9`, `synchronicity` and `grpclib` whole and copies the metadata of `modal`, `beam-client`, `beta9` and `betterproto-beta9`. The deploy tree (sources only, no tests or fixtures) ships as data outside the PYZ (`--exclude-module deploy`), because Modal uploads those files and Beam stages them; `cli.ensure_deploy_importable` puts them on `sys.path`. An AST scan names every stdlib or SDK module the deploy tree imports as a hidden import. Tests and `fake_sdks` are excluded.

The script then smoke tests the binary offline: `--self-check` imports both SDKs and builds both apps from files on disk; no credentials must give `ERR_VALIDATION_ERROR`; fake credentials against 127.0.0.1:9 must give `ERR_PROVIDER_UNAVAILABLE`; stdout must be one JSON document and stderr IC-2 lines only. Last it adds `bundle.externalBin` to `tauri.conf.json` (skipped with `--no-config`). A local macOS arm64 build is 39,202,496 bytes (37.4 MiB).

After a passing smoke test the script writes `manga-cleaner-provisioner-<target>.sha256` beside the binary: one `sha256sum` line per source file it froze (the production `provisioner/` modules, the deploy tree, the SAM files, `requirements.lock` and the script itself). `src-tauri/build.rs` checks that stamp. A release build fails when any listed file changed since the freeze, because the app would otherwise offer the models and deploy the code of an older checkout (seen 2026-09-30: Qwen was missing from setup). A development build prints a warning and runs `python -m provisioner` from the checkout instead, preferring a Python that can import `modal`, `target/cloud-build-venv` included.

For local builds, `npm run helpers` (`scripts/update-helpers.sh`) does the whole refresh: it creates `target/cloud-build-venv` with Python 3.12, installs `requirements.lock` hash-checked when the lock changed, freezes the helper with `--no-config` when its stamp no longer matches (`--force` to freeze anyway), and stages `uv` when it is missing. `npm run helpers:install` then copies any changed helper into `/Applications/Manga Cleaner.app` (or `MC_APP`), signs it ad hoc again and verifies the signature. It refuses while the app runs, and it refuses an app whose binary does not contain this checkout's cloud code digest: that app would call every setup the helper makes out of date. The new signature makes the app ask once more for each cloud key.

`provisioner/requirements.lock` hash-pins all 74 packages, resolved for Python 3.11+ on every platform, from `modal[api-proxy-support]==1.5.5`, `beam-client==0.2.211`, `beta9==0.1.268` and `pyinstaller==6.22.3`. The proxy extra is there because Modal needs `python-socks` and `aiohttp-socks` whenever a proxy is set, a macOS system proxy included. The release workflow runs `python -m pip install --require-hashes -r provisioner/requirements.lock` on macOS, Windows, and Linux x64, then the build script for the target.

The Rust release build checks that `bundle.externalBin` names the helper and that the target-specific binary exists. After bundling, the release workflow runs the helper's self-check from inside the macOS `.app` and checks that the Windows NSIS installer and Linux `.deb` contain the setup helper. A missing helper therefore fails the release before publication. The release matrix covers macOS Apple Silicon, Windows x64, and Linux x64.

## 8. Tests

```sh
python3 -m unittest discover -v -s deploy/cloud/tests   # 180 tests
target/cloud-build-venv/bin/python -m unittest discover -s provisioner -p 'test_*.py' -t .   # 156 tests with the SDKs
python3 -m compileall deploy/cloud provisioner
```

With the SDKs installed the fidelity tests run as well (0 skipped). Use a scratch venv and a throwaway HOME:

```sh
python3.11 -m venv /tmp/mc-sdk
/tmp/mc-sdk/bin/python -m pip install --require-hashes -r provisioner/requirements.lock
HOME="$(mktemp -d)" /tmp/mc-sdk/bin/python -m unittest discover -v -s provisioner
```

- `test_drivers.py` runs both drivers end to end against `fake_sdks.py`: IC-1 and IC-2 shapes, a kill after each side effect followed by a resume without duplicates, resume at `completed`, failed, slow and reattached weights, cleanup and redaction.
- `test_discovery.py` covers `inspect`'s list (read only, bounded, invalid recorded choices dropped, a failed listing still answering) and taking an installation over from a second journal directory: no seed job, no second volume, one redeploy, a new proxy token beside the old one, and a seed only for a model or graph that is missing.
- `test_sdk_fidelity.py` compares the fakes with the installed SDKs: call signatures, message fields and the exception hierarchy.
- `test_subprocess_entrypoint.py` runs the CLI hermetically (no token variables, scratch HOME, both SDKs pointed at 127.0.0.1:9) and checks the stream contract and typed failures. With the SDKs installed the Modal case waits out the 40 s sign-in bound.
- `test_regression_findings.py`, `test_journal.py`, `test_protocol.py` and `test_redaction.py` cover the journal, envelope and redaction rules. No test contacts a provider or reads a real config file or keychain.

## 9. Not verified live

- Modal: proxy auth header forwarding and `allow` on workspaces with and without environment access control; how long FunctionCall results stay readable; Dict limits; image build time and the cu129 torch build on Modal GPUs; GPU memory for the pipeline; the length limit of the `get_web_url` host label.
- Beam: how an HTTP enqueue body maps to task kwargs and whether the response carries `task_id`; Map access from containers; volume write visibility across containers; the invoke URL format; name and id filters in the list calls; `authorize` semantics; `stop_tasks` from a container; Starlette in Beam's runtime; RTX5090 (sm_120) with cu129; the cold start bound for `on_start`.
- Hugging Face download progress granularity.
- A native Windows deploy on either provider; the frozen Windows build has not been run.
- A kill between `proxy_tokens.create` and the journal write leaves a live proxy token the journal does not know.
- The Modal SDK unpickles Dict and FunctionCall data it reads (Beam Map values go through a restricted unpickler).
- The 40 s Modal sign-in bound can cut off a very slow network.
- Discovery: the path format `Volume.listdir` returns (leading slash or not; both are accepted), whether `list_deployed_apps` with the resolved environment name lists the same apps as the default, how long listing, the Dict read and `get_app_lifecycle` take for ten installations, and whether a v1 volume lists the same way.
- Beam on Windows: `.beamignore` matching uses OS path separators.
- Beam Map deletion: beta9 0.1.268 has no call that deletes a map itself (MapService only has MapSet, MapGet, MapDelete per key, MapCount and MapKeys), so cleanup can only empty the map; whether Beam still lists an empty map anywhere is unknown. MapKeys and MapDelete answer only `ok`, with no reason: that listing a map with no keys answers `ok` with an empty list (not a refusal) is assumed from the SDK, which reads a refusal as empty; if Beam refused instead, cleanup of such a map would report that refusal every time.
- Beam ListTasks: that an unknown task id answers `ok` with no tasks rather than a refusal (a refusal fails the step and keeps the task id).
