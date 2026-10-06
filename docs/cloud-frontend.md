# Cloud Frontend Reference

This is the developer reference for the Svelte side of cloud rendering and cloud setup: the calls it makes, which module owns what, the flows, and the rules that hold. The native side is described in [cloud-consent.md](cloud-consent.md), [cloud-journal.md](cloud-journal.md) and [cloud-security.md](cloud-security.md).

## 1. Frontend API

Every method below is on the `Backend` seam in `src/lib/api/backend.js`. In a Tauri window, `src/lib/api/tauri.js` passes each one straight to its command. A rejection reaches the caller, and no cloud method falls back to the browser mock. Native rejections are prose; stable codes come in answers and events.

| Method | Command | Arguments | Answer |
|---|---|---|---|
| `readInferenceConfig()` | `read_inference_config` | none | `InferenceConfig {schemaVersion, selectedTarget, beamProfiles, modalProfiles}` |
| `writeInferenceConfig` | `write_inference_config` | `{config}` | the saved `InferenceConfig`, with `canonicalOrigin` and `canonicalOriginFingerprint` filled in |
| `storeCloudSecret` | `store_cloud_secret` | `{provider, profileId, role, secret, tokenId?, sessionOnly = false}` | `SecretSummary {provider, profileId, role, present, backend}` |
| `deleteCloudSecret` | `delete_cloud_secret` | `{provider, profileId, role}` | `SecretSummary` |
| `getCloudSecretSummary` | `get_cloud_secret_summary` | `{provider, profileId, role}` | `SecretSummary` |
| `checkCloudConnection` | `check_cloud_connection` | `{provider, profileId}` | `{ok, status, provider, profileId, latencyMs?, message?}` |
| `getCloudModelInfo` | `get_cloud_model_info` | `{provider, profileId}` | `{supportedProtocolVersion, pinnedModelId, pinnedModelRevision, pinnedRecipeId, preprocessingVersion, nativeMaskConditioning, limits}` |
| `prepareCloudConsent` | `prepare_cloud_consent` | `{target, recipe, intent, regionId, chapterId, pageIndex}` | `ConsentProposal {proposalId, profileId, provider, endpointUrl, canonicalOriginFingerprint, profileEpoch, cropSha256, hintSha256, sourceHash, maskHash, regionRevision, rect, recipe, intent, createdAtMs, expiresAtMs, estimatedCostUsd?}` |
| `confirmCloudConsent` | `confirm_cloud_consent` | `{proposalId, intent}` | `Grant {nonce, scope, issuedAtMs, expiresAtMs, allowedAttempts, usedAttempts}` |
| `submitCloudAttempt` | `submit_cloud_attempt` | `{attemptId, grantNonce, proposalId?, target?, recipe?, snapshot?}` | `{attemptId, handle?, status, requestDigest?, autoRetryable?}` |
| `getCloudAttemptStatus` | `get_cloud_attempt_status` | `{attemptId, handle?}` | `{attemptId, handle, status, reportedCostUsd, acknowledged?}` |
| `getCloudAttemptResult` | `get_cloud_attempt_result` | `{attemptId, handle?}` | `{attemptId, handle, resultDigest, reportedCostUsd, width, height, cached}` |
| `cancelCloudAttempt` | `cancel_cloud_attempt` | `{attemptId, handle?}` | `{handle, status, acknowledged}` |
| `reconcileCloudRecovery` | `reconcile_cloud_recovery` | `{apply?, attemptId?, chapterId?, pageIndex?, regionId?, regionRevision?, sourceImageHash?}` | `CloudRecoveryDecision {decision, attemptId?, handle?, ...}`; with `apply: true`, `CloudRecoveryReport {attached, stillRunning, needsAttention}` |
| `runCloudProvisioner` | `run_cloud_provisioner` | `{op = 'inspect', provider = 'modal', params = {}}` | the helper envelope `{protocol_version, request_id, success, data, error {code, message}}` |
| `cancelCloudProvisioner()` | `cancel_cloud_provisioner` | none | `{cancelled}` |

- `role` is `setup`, `runtime` or `model_download`, and `backend` is `keyring`, `session` or `unavailable`. `tokenId` is the Modal proxy token id of a Modal runtime token.
- The connection statuses the interface names are `reachable`, `http_error`, `unauthorized`, `credential_missing`, `configuration_error` and `unreachable`. A check never starts a GPU.
- Several commands also take `simulate*` fields. They are test knobs, and the interface never sends them.
- The interface never calls `submitCloudAttempt`, `getCloudAttemptStatus` or `getCloudAttemptResult`. A render runs inside the region command that starts it, and the native side submits, polls and fetches.
- The provisioner `op` is `inspect`, `plan`, `apply`, `resume`, `cleanup_plan`, `cleanup_apply`, `forget_credential` or `probe_compatibility`; the interface uses the first six. Provider keys go in `params.credentials`, and the core hands the request to the helper on stdin only. `apply` adds `installation_id`, `approved_plan_hash`, `options {gpu, idle_seconds}` and `forget_setup_credential: true`; `resume` repeats the installation id and options; `cleanup_apply` adds `approved_cleanup_plan_hash` and `confirm_delete_persistent_storage`.
- A successful `apply` or `resume` answers `data.profile {provider, profile_id, name, endpoint_url}`, `data.health {ok, status, latency_ms}` and `data.selected` (IC-1). The runtime credential never reaches the webview.
- `provision_inspect`, `provision_plan`, `provision_apply`, `provision_resume`, `provision_cleanup` and `provision_probe` are registered too, but have no seam method.

The region commands that can render in the cloud carry the grant in `params` as `CloudRunParams {grantNonce, executionTarget, recipe, intent}`:

- `applyTool({tool, params, chapterId, pageIndex, regionId})` answers `ApplyResult {status, errorCode?, region?, mask?, pageStatus?}`. A cloud render ends `applied`, `failed`, `cancelled` or `unknown`; `needs-confirmation` means a cloud engine was named without a grant, and `blocked` means cloud is off.
- `rerunMask({maskId, kind, engine, params})` answers `{region, mask, reopenTool, pageStatus}`, or null when the render does not commit; the reason is on the event. The adapter leaves `params` off for a local rung.
- `cleanAnyway({regionId, engine, params})` takes the same `params`, but the interface always calls it without them. `createRegion` always renders locally.

`readCloudReadiness(backend)` in `backend.js` is not a command. It reads the permission (`cloudEngines === 'allowed'` in settings), the inference config and the runtime secret summary, and answers `CloudReadiness {allowed, configured, ready, reason, target, profile, endpoints}`. `configured` means the default target is a saved endpoint with a runtime token, `ready` is `allowed` and `configured` together, and `reason` is the first thing missing: `off`, `noTarget`, `noSecret` or `unknown`.

There are two event channels, named in `CLOUD_EVENTS` and `EVENT_METHODS` in `tauri.js`. Each method answers a promise of its unlisten function; outside a Tauri window that function does nothing.

- `onProvisionProgress(handler)` listens on `provision://progress` (IC-2): `{op, provider, step, state, pct}`. `step` is inspect, validate, volume, state, secret, image, deploy, weights, token, endpoint, health or cleanup; `state` is start, done, fail or skip; `pct` is 0 to 100, or null.
- `onCloudAttempt(handler)` listens on `cloud://attempt` (IC-3): `{attemptId, regionId, chapterId, pageIndex, phase, elapsedMs, errorCode}`. The phases are preparing, submitting, queued, running, downloading and compositing, then exactly one of committed, failed, cancelled or unknown. `errorCode` is a snake_case code on failed and null otherwise. The native side emits the ending before the command answers.

## 2. Modules

Paths are under `src/lib/`.

- `api/attempt.js`: `sha256Hex`, and `cloudAttemptId(nonce)`, which is `att-` plus the first 24 hex digits of the SHA-256 of the grant nonce, as `service.rs#attempt_id_for_nonce` computes it. The interface and the mock both use it, so a render has its id, and its Cancel, before its first event.
- `state/cloud.svelte.js` owns the shared state `cloud {readiness, checked, jobs, lastCommitAt, recovery}`.
  - `refreshCloudReadiness` keeps only the newest answer. `cloudUsable()` is `session.cloudAllowed && cloud.readiness.configured`, and every Cloud choice in the editor reads it.
  - `setCloudPermission` saves the switch, puts it back with `notice.cloud.permissionFailed` when the save fails, and reads readiness again. `openCloudSettings` opens Settings on the `inference` tab, labelled Cloud.
  - `startCloud`, called once from `App.svelte` after the settings load, listens to `cloud://attempt`, reads readiness again whenever the permission changes, and starts recovery.
  - `trackCloudJob`, `onAttemptEvent`, `settleCloudJob` and `cancelCloudJob` run the jobs, and `dismissRecovered` drops a recovery entry. `cloudErrorKey` and `cloudPhaseKey` turn codes and phases into i18n keys.
- `editor/cloudflow.svelte.js`: `cloudRefused(tool, params)`, `requestCloudConsent(request)` and `runCloudJob(grant, where, call, outcomeOf)`.
- `dialogs/CloudConsentDialog.svelte`, modal kind `cloudConsent`, blocking: Escape cancels and the backdrop does nothing. Its props are `provider`, `profileName`, `host`, `width`, `height` and `estimatedCostUsd`. It says what is sent (the crop size), where (the endpoint name and provider, or the provider alone, and the host) and the cost (the estimate, or that there is none).
- `editor/CloudJobStatus.svelte`, mounted in `App.svelte` at the bottom left above the loaded-models panel. It shows one row per job: phase, `m:ss`, page, and Cancel, which is `aria-disabled` while cancelling. When no render has committed in the last 5 minutes (`WARM_MS`), a row adds the first-run hint until its phase reaches `running`.
- `dialogs/provisioning.svelte.js` keeps the setup run outside any component: `setup {run, unfinished, watchers}`, `runSetup`, `planCleanup`, `applyCleanup`, `removeEndpoint`, `stopSetup`, `forgetUnfinished`, `newInstallationId` (`mc-` plus six lowercase letters or digits), and the key tables for steps, `ERR_*` codes, resource types and health statuses.
- `dialogs/CloudProvisioner.svelte` is the setup interface. Its props (IC-5) are `inline`, `initialProvider`, `installationId`, `existing {provider, installationId, action: 'resume'|'cleanup'}`, `onclose()`, `onconfigured({provider, profileId, endpointUrl, name, healthy})`, `oncleaned()`, `onbusychange(busy)`, and the test seams `runCloudProvisioner` and `backend`. `onbusychange(true)` fires while it plans, reads a cleanup plan or runs, and `false` when that ends or it unmounts. Its views are connect, review, running, failed, resume, done, cleanup, cleaning, cleanupFailed and cleaned. A host callback that throws or rejects cannot break the flow.
- `dialogs/InferenceSettings.svelte` is the Cloud tab, from the top: the status line (off, checking, ready, or needs attention with the reason); the Use a cloud GPU switch, which lives only here (General's Cloud GPU row opens this tab); Needs attention (an unfinished setup with Resume and Forget, a default endpoint with no token with Add token and, for one setup made, Get a new token, and recovered attempts with Dismiss); the endpoints (Default, Test, add or replace token, and Remove, with Also delete the cloud resources); Set up with Modal or Beam, the provisioner inline; and Connect an existing endpoint. Closing a task puts focus back on the control that opened it.
- Rows and menus:
  - `model/masks.js`: `CLOUD_ENGINE` is `'cloud'`. `rowEngines(available, {cloud})` puts it last when `cloud` is true; `ROW_ENGINES` stays local, because Text cleanup and the AI mask brush read it. It also has `isCloudMask`, `maskEngine` (a cloud patch reads as Cloud) and `regionMenuSections(region, {engines, cloud})`.
  - `editor/MaskRow.svelte` and `editor/RegionMenu.svelte` pass `cloudUsable()`.
  - `editor/maskactions.svelte.js#rerunMask` sends `engine: 'cloud'`, and Try again on a cloud mask, to `rerunInCloud`, with the intent `{action: 'rerunMask', mask_id, kind: 'engine', engine: 'cloud'}`. `cleanAnyway` is always local.
  - `editor/tools.js`: Content-aware fill's `engine` choice is `local` or `cloud`, and `toolSpendsCloud(id, params)` reads the options' `cloud` flags. `ToolBar.svelte` shows Cloud disabled, with the reason and Open Cloud settings, unless `cloudUsable()`.
  - `editor/toolapply.svelte.js` has `applyInCloud` (intent `{action: 'applyTool', tool, params}`) and `renderRegionInCloud`. `editor/drawing.svelte.js` creates a drawn region locally without the cloud choice, then calls `renderRegionInCloud`.
- First launch: `dialogs/onboarding/CloudStep.svelte` mounts `<CloudProvisioner inline initialProvider="modal">` with `closeFirstLaunchProvisioner`, `configureFirstLaunchCloud` and `setFirstLaunchProvisionerBusy` from `dialogs/firstlaunch.svelte.js`, which holds `firstLaunch.cloud {provider, name, healthy}` and `firstLaunch.provisionerBusy`.

## 3. Flows

A cloud render:

1. `cloudRefused` stops a cloud tool while the permission is off, before any call, with `notice.cloud.blocked`.
2. `requestCloudConsent` reads readiness again (`notice.cloud.notReady` when not ready, `notice.cloud.blocked` when off), reads `getCloudModelInfo` for the recipe, calls `prepareCloudConsent`, and shows the consent dialog. Only Confirm calls `confirmCloudConsent`. It answers `{params, attemptId}`, or null after `notice.cloud.consentFailed`. Every render asks again.
3. `runCloudJob` tracks the job under its attempt id before the call, then calls `applyTool` or `rerunMask` with the grant in `params`.
4. Events move the job through its phases. It ends once, by the ending event or the command's answer, whichever arrives first. An answer that cannot say how it ended waits `SETTLE_GRACE_MS` (1.5 seconds) for the event, then ends as failed. `attempt_busy` ends nothing, since the render already running under that id reports its own end.
5. A commit replaces the region and records one undo entry, unless the open chapter changed; the native side has already stored the result on its page.

Cancel. `cancelCloudJob` marks the job cancelling and calls `cancelCloudAttempt({attemptId})`. A live render answers `{handle: '', status: 'cancel_requested', acknowledged: false}` and stops at its next safe point with a `cancelled` event. A cancel while the result downloads comes too late: the render commits and ends as a result, the late commit. Once the result is downloaded, the command rejects with `cannot cancel attempt: job is already completed`; the row goes back to Cancel, `notice.cloud.cancelFailed` says the render may still finish, and the commit follows.

Unknown. A render whose acceptance cannot be confirmed ends `unknown`, and `notice.cloud.unknown` says nothing was applied and nothing was sent again.

Recovery. `startCloud` calls `reconcileCloudRecovery({apply: true})` once per app run: at start when cloud is allowed, otherwise the first time it is switched on, because waiting on an accepted job contacts the provider. `attached` regions reload into the open chapter with `notice.cloud.recovered`. `stillRunning` entries become background jobs whose commit reloads the region. `needsAttention` entries (ambiguous, stale, failed) raise `notice.cloud.needsAttention` and stay under Needs attention until dismissed.

Setup:

1. Connect: the provider and its keys. Continue runs `inspect`, then `plan`, and nothing is created.
2. Review: the resources, the GPU and idle time the plan offers, the weights download, the cost notes and one approval. Start runs `apply` with the plan hash.
3. Setting up: `runSetup` writes the unfinished record, listens to `provision://progress` for the checklist, and waits for the helper. Stop calls `stopSetup`, and the run ends with `ERR_CANCELLED`. Settings can close meanwhile: a provisioner mounted again picks the run up, and a run nobody watches ends with a notice.
4. Done: the native side has saved, selected and checked the endpoint (IC-1). The provisioner shows the health result, wipes the keys, and calls `onconfigured` with `healthy` set to `data.health.ok === true`.

A failed run offers Resume (`resume` with the same installation id and options) and Clean up; after `ERR_UNAPPROVED_PLAN`, which created nothing, it offers Start again instead. An `apply` refused before it created anything restores the previous unfinished record. Clean up runs `cleanup_plan`, lists what that one installation created, and after the person confirms and gives the key runs `cleanup_apply`. `removeEndpoint` then takes the profile out of `inference.json` and the runtime token out of the keychain, and the default falls back to local. The unfinished record is the `localStorage` key `mangaCleaner.cloudSetup.v1`, holding `{provider, installationId, options {gpu, idle_seconds}}` and never a token. Forget clears it and touches nothing in the account.

First launch. Step 4 offers Not now and Set up now until a setup finishes or the permission is on; Set up now shows the provisioner inline. `configureFirstLaunchCloud` stores `firstLaunch.cloud` and turns the permission on only when `healthy` is true. An endpoint that failed its first check stays saved and selected, the permission stays off, the step says to test it in Settings > Cloud, and Done reads "Off, {name} saved". While the provisioner reports busy, Escape and Skip do nothing. Once a setup finishes, the dialog offers Continue.

## 4. Invariants

- No secret enters Svelte state outside the password fields that collect it, and none enters storage or the rest of the DOM. Keys travel in one call's `params` or in `storeCloudSecret`. The Cloud tab wipes a token field as soon as `storeCloudSecret` returns; the provisioner keeps its keys in memory for Resume and wipes them on success, close and unmount. The runtime credential stays in Rust, and the interface only asks whether one is present.
- Nothing leaves the machine without a grant minted after Confirm and bound to one region revision, target, recipe and intent. Saving a profile or its token revokes that profile's grants. Text cleanup's clean goes to the cloud only when Clean on is Cloud GPU, and only after the cloud clean consent for that exact plan; a failed cloud render never falls back to a local engine.
- FLUX in a picker is always the local helper. `rerunNeedsCloud` in `api/tools.js` mirrors `region.rs#rerun_needs_cloud`: a re-run needs the cloud when it targets Cloud, or when a cloud patch re-runs at its own rung without naming an engine (`kind !== 'engine'`).
- Every visible string is an i18n key. Codes map to keys through fixed tables, and helper messages and native error prose are never shown.
- Each render ends in exactly one notice.

The mock, `createMockBackend({timing})` in `api/mock.js`, simulates the cloud in memory:

- config, secrets and readiness;
- connection checks, where an address containing `offline` or `unreachable` fails and a missing token answers `credential_missing`;
- pinned model info, and proposals and grants with expiry and attempt counts;
- the grant checks (`consent_invalid`, `target_changed`, `profile_missing`, `credential_missing`, `invalid_request`), `gateway_unreachable` and `attempt_busy`;
- IC-3 phases with a cold start on the first render, the cancel answers above, and the re-run rule;
- the provisioner: plans with `resource_allocation {gpu, gpu_options, idle_seconds, max_containers, model_weights_bytes}`, IC-2 progress, an `apply` that saves the profile, runtime secret and selection with a passing health check, stop, resume and cleanup;
- recovery from its own journal.

`?cloudSetupFail=<step>` fails the first apply at that step, and `?cloudRecovery=1` leaves two renders from a previous session. The mock's health check always passes, so component tests with a stub runner cover the unhealthy path.

The mock stays in step with the native side through `mock.cloud.test.js`, which holds it to IC-1 to IC-4 and pins the re-run rule; the shared `attempt.js`; `PINNED_CLOUD_MODEL_ID`, `PINNED_CLOUD_MODEL_REVISION` and `PINNED_CLOUD_RECIPE_ID`, which match `deploy/cloud/common/manifest.py` (IC-6); `tauri.test.js`, which pins each command name and argument shape; and comments naming the native function each branch copies, such as `commands.rs#past_cancelling` and `region.rs#render_in_cloud`.
