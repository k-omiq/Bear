# Cloud wire contract (`/mc/v1`)

This document defines the provider-neutral HTTP contract between the desktop app and a cloud gateway on Modal or Beam: the request digest, the endpoints, the capability handshake and the rules the client follows when a submission is uncertain. The exact bytes are fixed by `crates/cleaner-core/src/cloud_wire.rs` and `deploy/cloud/common/contract.py`, which share JSON fixtures in `deploy/cloud/fixtures/`.

**Protocol version:** `1.0.0`

---

## 1. Scope and invariants

The gateway implementations live in `deploy/cloud/`. The app side is `src-tauri/src/inference/` (see [cloud-transport.md](cloud-transport.md)). Page analysis and page denoise have their own wire contracts (`cloud_analysis_wire.rs`, `cloud_denoise_wire.rs`, `cloud_job_wire.rs` and the matching routes in `deploy/cloud/common/api.py`). This document covers the render contract and the routes every gateway shares.

1. **Crop-only transmission, no coordinate leakage.** Only a tightly cropped raster is sent, grown by context padding and snapped to multiples of `LATENT_STRIDE = 16`. Full pages, page-relative coordinates (`x`, `y`, `page_x`, `page_y`, `strip_rect`, `on_page`), region IDs, project archives and local filesystem paths never appear in a request. Deserialization denies unknown fields.
2. **Provider-agnostic gateway.** Modal and Beam both expose this REST interface. Container lifecycle and queueing stay behind it.
3. **Collision-free canonical digest (`MC-REQ-V1`).** Content and recipe metadata are bound by a compact, versioned JSON array of restricted ASCII strings, integers and booleans. This removes delimiter collisions (`job_id="a|b", attempt_id="c"` against `job_id="a", attempt_id="b|c"`) and floating-point differences between Rust and Python.
4. **No deduplication guarantee.** The digest gives content and metadata integrity only. It is not authorisation and not an idempotency key. The server keeps no digest index and offers no digest lookup. Every dispatch still needs a backend grant.
5. **Exact identity binding.** Every server response (`202 Accepted`, status, result and cancel) must bind `handle`, `job_id`, `attempt_id`, `request_digest` and the recipe identity (`recipe_id`, `preprocessing_version`, `model_id`, `model_revision`, `native_mask_conditioning`).
6. **Honest capability reporting.** `model-info` reports `native_mask_conditioning: false` for the current FLUX and Qwen recipes. The grayscale hint is advisory: the network ignores it as a mask. A request claiming native masked diffusion is rejected before submit. Fixed prompt and hyperparameter allowlists prevent arbitrary prompt or code execution.
7. **Fail-closed validation.** Unknown versions, unexpected fields, mutable revision tags (`main`, `master`, `latest`, `head`, `dev`, `staging`), non-hex revisions, unaligned dimensions, non-finite costs and out-of-bound limits fail before anything is enqueued.
8. **Structured rejection against ambiguous acceptance.** A `PreEnqueueRejectionResponse` with `enqueued: false` and the bound identity proves nothing was queued. A network failure during submission does not: it ends as `submission_unknown` (section 6).
9. **Bounded decode before allocation.** Wire validators check the PNG signature, the `IHDR` chunk (length 13, geometry, bit depth 8, colour mode, CRC-32) and the byte limits before any allocation. They do not decompress the `IDAT` data. The host decoder `crates/cleaner-core/src/cloud_decode.rs` does the full bounded decompression, and compositing (context alpha blending, edge replication, tone matching) stays on the host in Rust.
10. **Multipart limits.** The wire layer checks each PNG buffer and the pixel arithmetic, and `max_multipart_bytes` against the encoded body (`cloud_wire.rs`).

The reference render recipe is the FLUX SDNQ recipe in `sidecar/manga_cleaner_sidecar/backend/sdnq.py` (`WORK_LONG_SIDE = 768`).

---

## 2. Canonical request digest (`MC-REQ-V1`)

The canonical metadata is a compact JSON ordered array without whitespace:

```json
[
  "MC-REQ-V1",
  "<protocol_version>",
  "<job_id>",
  "<attempt_id>",
  "<recipe_id>",
  "<preprocessing_version>",
  "<model_id>",
  "<model_revision>",
  <native_mask_conditioning_bool>,
  <width_int>,
  <height_int>,
  <seed_int>,
  <steps_int>,
  <guidance_scaled_int>,
  "<image_sha256>",
  "<hint_sha256>"
]
```

A recipe that carries `qwen_edit` (section 7) appends one more element, `["<target>", "<description>"]`, and the array is encoded with `ensure_ascii=False` so Unicode descriptions hash the same in Python and Rust. A shared fixture checks both implementations.

A recipe that carries `work_long_side` (section 8) appends one more element after that, the bare integer. The fixture `deploy/cloud/fixtures/flux3_work_size_request.json` checks both implementations.

A recipe that carries `flux3_generation` (section 9) appends one more element after that, its four booleans in order: `[reference_prompt, marker, reference_shift, megapixel_size]`. The fixture `deploy/cloud/fixtures/flux3_generation_request.json` checks both implementations.

The request digest is the lowercase hexadecimal SHA-256 of the UTF-8 bytes of that array:

$$\text{request\_digest} = \text{hex}(\text{SHA256}(\text{CanonicalJSONArray}))$$

### Digest test vectors

Synthetic 40-hex revision `0123456789abcdef0123456789abcdef01234567`.

- **Base vector:**
  - Canonical JSON: `["MC-REQ-V1","1.0.0","job-test-001","attempt-test-001","test-sdnq-v1","1.0.0","test-flux-schnell","0123456789abcdef0123456789abcdef01234567",false,16,16,42,4,350,"2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f","3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238"]`
  - SHA-256: `42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0`
- **Collision case A (`job_id="a|b", attempt_id="c"`):** `88647a2f2f1351530c85b444cedde98d01ca9e9548242bc1aa4ac84f5e74dd2c`
- **Collision case B (`job_id="a", attempt_id="b|c"`):** `6298eeb660626079ffba0d848451d9184ee0f416691e52a7972dc9b9bc9c007c`

---

## 3. Endpoints and shared schemas

Every request is authenticated. The Rust client adapts the headers to the provider: Modal proxy token id and secret as `Modal-Key` and `Modal-Secret`, or a Beam bearer token. Unverified bearer claims are not accepted.

### 3.1 `GET /mc/v1/health`

Proves control-plane reachability and gateway authentication only. It does not prove GPU allocation, model residency or warmup, and it never starts a GPU.

```json
{ "status": "ok", "provider": "modal", "protocol_version": "1.0.0" }
```

### 3.2 `GET /mc/v1/model-info`

Returns the pinned model and recipe and the service limits.

```json
{
  "protocol_version": "1.0.0",
  "provider": "modal",
  "model_id": "test-flux-schnell",
  "model_revision": "0123456789abcdef0123456789abcdef01234567",
  "recipe_id": "test-sdnq-v1",
  "preprocessing_version": "1.0.0",
  "native_mask_conditioning": false,
  "limits": {
    "max_width": 2048,
    "max_height": 2048,
    "max_pixels": 4194304,
    "max_png_bytes": 16777216,
    "max_multipart_bytes": 33554432,
    "worker_timeout_seconds": 120
  }
}
```

The values are the fixture limits. A deployed gateway advertises the same payload bounds with `worker_timeout_seconds` set to 600, the provider job timeout (`production_limits` in `deploy/cloud/common/manifest.py`). These are safety ceilings, not measured serving limits.

### 3.3 `POST /mc/v1/jobs`

Enqueues a crop render. Accepts `multipart/form-data` with `metadata` (canonical JSON), `image` (PNG, RGB8, exact snapped dimensions) and `hint` (PNG, Gray8, same dimensions).

```json
{
  "protocol_version": "1.0.0",
  "job_id": "job-test-001",
  "attempt_id": "attempt-test-001",
  "recipe": {
    "recipe_id": "test-sdnq-v1",
    "preprocessing_version": "1.0.0",
    "model_id": "test-flux-schnell",
    "model_revision": "0123456789abcdef0123456789abcdef01234567",
    "native_mask_conditioning": false
  },
  "width": 16,
  "height": 16,
  "seed": 42,
  "steps": 4,
  "guidance_scaled": 350,
  "image_sha256": "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f",
  "hint_sha256": "3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238",
  "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0"
}
```

Response `202 Accepted`:

```json
{
  "handle": "handle-test-modal-999",
  "status": "pending",
  "job_id": "job-test-001",
  "attempt_id": "attempt-test-001",
  "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
  "recipe_id": "test-sdnq-v1",
  "preprocessing_version": "1.0.0",
  "model_id": "test-flux-schnell",
  "model_revision": "0123456789abcdef0123456789abcdef01234567",
  "native_mask_conditioning": false
}
```

If Modal's spawn raises after the job claim is stored, the gateway answers `503` with `error_code: "submission_unknown"` and no pre-enqueue rejection body. The claim stays in the job store, and another request using that attempt does not spawn a second render. The status may stay pending without a native call ID. The client must not retry automatically or create a new attempt from this answer.

### 3.4 `GET /mc/v1/jobs/{handle}`

Authoritative execution status: `pending`, `running`, `completed`, `failed` or `cancelled`.

```json
{
  "handle": "handle-test-modal-999",
  "job_id": "job-test-001",
  "attempt_id": "attempt-test-001",
  "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
  "recipe_id": "test-sdnq-v1",
  "preprocessing_version": "1.0.0",
  "model_id": "test-flux-schnell",
  "model_revision": "0123456789abcdef0123456789abcdef01234567",
  "native_mask_conditioning": false,
  "status": "completed",
  "reported_cost_usd": 0.0012,
  "error": null,
  "result_digest": "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f",
  "result_bytes": 73
}
```

`reported_cost_usd` is nullable. A failed job carries the gateway's typed `error_code` and `message`.

### 3.5 `GET /mc/v1/jobs/{handle}/result`

Raw output PNG bytes (RGB8, exact input dimensions). The headers `X-MC-Result-Digest` (SHA-256 of the payload) and `X-MC-Reported-Cost-Usd` accompany it.

The route answers `Range: bytes=N-` with `206` and the bytes from N on. Any other `Range` form is ignored and gets the whole result. The desktop asks again for what is missing when a transfer stops (`fetch_result_bytes_while` in `src-tauri/src/inference/http.rs`), gives up after three requests in a row that add nothing or after ten minutes, and checks the digest of the joined result. A gateway that answers `200` with the whole result is taken as a restart.

### 3.6 `POST /mc/v1/jobs/{handle}/cancel`

Requests cancellation. The answer is non-terminal and carries the bound request digest. The client reconciles the terminal state by asking for status again.

```json
{
  "handle": "handle-test-modal-999",
  "job_id": "job-test-001",
  "attempt_id": "attempt-test-001",
  "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
  "status": "cancel_requested",
  "acknowledged": true
}
```

### 3.7 Structured pre-enqueue rejection

Confirms the request was rejected before it was queued (`400` or `422`).

```json
{
  "enqueued": false,
  "error_code": "unsupported_recipe",
  "message": "Recipe test-unsupported is not supported on this worker",
  "job_id": "job-test-001",
  "attempt_id": "attempt-test-001",
  "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
  "retryable": false,
  "details": null
}
```

### 3.8 `POST /mc/v1/warmup`

An explicit, potentially billable readiness probe. It allocates a GPU and loads the model weights.

### 3.9 `GET /mc/v1/gpu`

Which GPU containers are up now. The data comes from heartbeats the GPU containers write to the job store (`deploy/cloud/common/gpu.py`). The route runs on the CPU gateway and never starts a GPU. A heartbeat older than its container's scale-down window plus a grace is treated as gone, because a container can die without removing it.

```json
{ "provider": "modal", "supported": true, "now": 1790000100.0,
  "containers": [{ "role": "render", "gpu": "L4", "state": "idle",
                   "started_at": 1790000000.0, "last_active_at": 1790000060.0,
                   "idle_seconds": 120, "scaledown_estimate_at": 1790000180.0 }],
  "list_price_usd_per_hour": { "L4": 0.8 } }
```

- `role` is `render` or `analysis`, at most one entry each. `state` is `starting`, `idle` or `busy`. Times are Unix seconds on the gateway's clock. `now` is that clock at the answer, so a client measures time left against it rather than its own clock. `scaledown_estimate_at` is set only for `idle`.
- `list_price_usd_per_hour` is the provider's list price for the deployed GPU, for display only. It may be empty.
- `supported: false` with no containers means the deployment cannot report its GPU (Beam today). A deployment older than this route answers `404`. Clients treat both as "unknown", never as "down".
- If the heartbeat store cannot be read, Modal also answers `supported: false`.

### 3.10 `POST /mc/v1/gpu/stop`

Stops GPU containers. The body is optional. `{}` or no body stops every role. `{"role": "render"}` or `{"role": "analysis"}` stops only that container and only its work. `"idle_only": true` makes it an idle release (below). Any other body (another role, `null`, an extra key, an `idle_only` that is not a boolean, a non-object, invalid JSON) is `400` with `error_code: "invalid_request"`.

- `render`: every pending or running job stays in the active index, including when more than 64 are queued. Each is first polled at the provider. A job the GPU already finished keeps its `completed` or `failed` result. A running job becomes `cancelled` only after Modal confirms its call termination.
- `analysis`: every analysis tile call in flight (tracked in the job store, bounded, dropped after 10 minutes) is cancelled with its container, so its request fails. A cancel that fails stays tracked for the next stop. New analysis tiles are refused for 15 s.
- A container that was busy and had its call terminated is gone, and its heartbeat is removed. Any other container that is up is sent a `release` call, which drains it and lets it exit. Its heartbeat stays until the container itself leaves, so `GET /mc/v1/gpu` keeps listing it (and it keeps billing) until then. A role is listed in `stopped` only when its call was terminated or its release was sent.
- A per-role marker makes a container of that role that cold-starts within 60 s of the stop skip its model load. The next submission for that role clears it. On Modal a render cold start refuses to start instead ([cloud-provisioning.md](cloud-provisioning.md), "Modal GPU cold starts").
- A new render or analysis request during the first 15 s after a plain stop gets a pre-enqueue refusal. Render admission and submission are serialised with stop in the gateway. A marker write failure, an unknown job index or a failed call termination answers `503` with `error_code: "stop_uncertain"`. The job and heartbeat stay tracked so Stop can be tried again.

Stop is idempotent: with nothing up it returns `stopped: []`.

```json
{ "provider": "modal", "stopped": ["render"], "cancelled_jobs": 1 }
```

`cancelled_jobs` counts render jobs only. A deployment that cannot stop its GPU answers `501` with `error_code: "unsupported"`.

**Idle release** (`{"role": "analysis", "idle_only": true}` or `{"role": "render", "idle_only": true}`). The app sends the render one once a batch cloud clean ([detect-clean.md](detect-clean.md) section 3) has its last region back. Detection and denoise leave the shared analysis GPU warm for its configured idle window: 120 seconds by default, 60 to 600 seconds by setting (`MC_MODAL_IDLE_SECONDS`). Finishing or cancelling a selection does not release its container, so another detection can reuse the loaded models during that window. Explicit **Stop GPU** ends the container at once. The endpoint still accepts an idle-only analysis release for older clients.

An idle-only release cancels nothing and writes no release marker, so no tile is refused afterwards and the next container loads its model as usual. A role's `release` is sent only when its heartbeat reads `idle` and no work of that role is tracked in flight. For `analysis` that means no tile call. For `render` it means no job pending or running in the gateway's active job index, which can include a job another device queued behind an idle heartbeat. A count that cannot be read counts as busy. A job turns terminal in the index when it is polled, so one whose caller stopped polling holds the render release back until a plain stop settles it. The GPU then scales down on its idle timer. A busy, starting or absent container is left as it is. The response has the same shape, with `cancelled_jobs: 0` and the released role in `stopped`. Without the marker, a release that races the provider's own scale-down can cold-start one container that loads its model and then leaves. The app ignores any failure (an older gateway's `400`, Beam's `501`, the network).

---

## 4. Capabilities and deployment compatibility

`GET /mc/v1/capabilities` uses the same authentication as health and model-info, does no GPU work, and returns `protocol_version`, `provider` and `operations`.

- Both providers list `jobs.submit`, `jobs.status`, `jobs.result` and `jobs.cancel`, and the app requires them.
- A gateway that renders also lists `jobs.retry_seed` (section 5). The app does not require it. Without it every request carries the pinned seed.
- Modal also lists `gpu.status`, `gpu.stop` and `gpu.release_idle`, and the app requires them there. Beam uses a provider-managed GPU lifecycle and does not advertise them.
- Analysis and denoise add `analysis.jobs`, `analysis.batches`, `denoise.jobs` and `denoise.results` when served. An app that finds them missing keeps the synchronous routes.
- A gateway that serves `analysis.batches` also lists `analysis.batch_canvas`: a batch body may then carry `canvas_png_b64` and `canvas_rect` (one PNG over the batch's tiles, white outside them) in place of `tiles_png_b64`. The gateway cuts each tile out and rebuilds its canonical PNG (`contract.py` `analysis_canonical_tile_png`), whose digest the request names. An app that finds it missing sends tile PNGs.

A missing required operation or an older endpoint produces "Gateway out of date. Redeploy the gateway" at the connection check. The model-info and analysis contracts keep binding the exact recipe, preprocessing, model revision and graph pins. Redeploy the CPU gateway and the GPU classes together, and do not clear durable job documents when upgrading.

**A deployment without a cleaning model.** A Modal deployment set up without one adds `"render": false` to the capabilities answer. A deployment with a model leaves the field out. The four `jobs.*` operations stay listed, so the connection check still takes the gateway as current. On such a deployment `/mc/v1/model-info`, `/mc/v1/jobs`, `/mc/v1/jobs/*` and `/mc/v1/warmup` answer `503` with `{"error_code": "capability_unavailable", "message": "Render is not configured"}`. Health, capabilities, GPU status and stop, analysis and denoise answer as usual. The desktop reads a `503` from model-info as "no cleaning model" only when capabilities says `render: false` (`HttpTransportError::RenderNotConfigured` in `src-tauri/src/inference/http.rs`), and the connection check then passes. A bare `503` stays a failure, so a gateway in trouble never reads as a setup choice.

---

## 5. The seed of a retry

A first attempt for a region carries the recipe's pinned seed. A retry carries a higher one, so it is a new render and not the same image again. A gateway takes a seed above the pinned one only when its capabilities list `jobs.retry_seed`. An older gateway refuses any seed but the pinned one (`flux.sampling_mismatch`), and the app sends it the pinned seed. How the app computes the seed is in [cloud-journal.md](cloud-journal.md) section 4.5.

---

## 6. Client dispatch rules

```text
[Intent persisted] -> [POST /jobs]
       |                   |
       | (network error)   +--> 202 Accepted -> [Handle persisted] -> [Poll status]
       v                                                                   |
[submission_unknown]                                                       +--> completed -> [Validate result] -> [Durable cache] -> [Project commit]
(no auto-retry)                                                            +--> failed    -> [Terminal failed]
                                                                           +--> cancel    -> [cancel_requested] -> [Reconcile]
```

1. **Intent before dispatch.** The client logs `{job_id, attempt_id, request_digest, grant_id}` before it transmits anything.
2. **Ambiguous transmission.** A timeout, disconnect or ambiguous 5xx or 429 before the `202` handle moves the attempt to `submission_unknown`. Automatic retry and provider failover are forbidden.
3. **Handle durability.** The `handle` is committed to disk before polling starts.
4. **Result validation and durable caching.** Before a result reaches the project, the encoded and decoded bounds are enforced before allocation, then the byte digest (`X-MC-Result-Digest`), the RGB8 colour mode and the pixel geometry (identical to the input) are checked. A completed result is validated and cached in the journal before the project commit.
5. **Stale results.** If a region is deleted or changed while a remote job is in flight, the completed patch stays in the journal and is not committed.
6. **No blind retries.** `POST /jobs` is repeated only after an authoritative pre-enqueue rejection (a structured error body) or a confirmed pre-transmission socket failure within the active grant. A raw HTTP 400 alone does not prove a pre-enqueue rejection. Reads of `/jobs/{handle}` or `/result` for a known handle may retry with exponential backoff.

### Unknown submissions

An unknown Modal submission is looked up with authenticated `GET /mc/v1/jobs/{handle}`. Only the Modal dispatch backend has this lookup contract. The handle is `handle-modal-` followed by the first 32 hex characters of the SHA-256 of the UTF-8 bytes of `job_id`, a NUL byte and `attempt_id`. This is not a universal handle scheme. The desktop checks the original profile endpoint fingerprint and validates every identity and recipe field of the answer against the saved request. Pending, missing, unavailable and mismatched answers stay unresolved, with no replacement `POST`. A resolved answer is saved as `lookup_evidence` before the journal moves from Unknown to Accepted. Journals without this optional field stay readable. Beam uncertainty stays unresolved until Beam has an explicit lookup contract. What happens to the attempt afterwards is in [cloud-journal.md](cloud-journal.md).

---

## 7. Guidance and the Qwen recipe

The Qwen Clean recipe and the Klein v2 recipes carry a bounded `qwen_edit` object: `{"target": "auto" | "dialogue" | "sound_effect" | "other", "description": "<text>"}`. A description holds no control characters and is at most 500 characters. Unicode is supported. The worker builds one prompt per call from the target, a coarse location taken from the hint, and fixed instructions to reconstruct hidden background and keep artwork, outlines, tone and surrounding detail. The description supplements those instructions. It is never a model prompt typed by the user, and shared pipeline settings are not changed between users.

Native consent, grant equality, batch plan hashes, the Python and Rust request digests and the durable attempt record all bind `qwen_edit`. A deployment that does not support it is refused before a description could be ignored.

**Which recipes take it.** `mc-qwen-image-edit-2511-v4`, `mc-flux2-klein-inpaint-v2`, `mc-flux2-klein-9b-inpaint-v2`, `mc-flux3-klein-edit-v1` and `mc-flux3-klein-bf16-edit-v1`, each only on its own model family. The set lives twice and a test ties the two: `GUIDED_RECIPES` in `crates/cleaner-core/src/engines/render.rs` and `GUIDED_RECIPE_IDS` in `deploy/cloud/common/contract.py`. Any other recipe with `qwen_edit` is rejected on both sides. A guided recipe always carries it: a clean that asked nothing sends Automatic with no description.

**Klein v2.** The worker fills the grown hint with the manga LaMa (`lama-manga.onnx`, a pinned adapter in the snapshot) and inpaints that hole with the filled crop as the only image, at strength 1.0 (`deploy/cloud/common/flux.py`). `flux_prompt` names the lettering type and reads the description as what the lettering covers. Automatic with no description is the pinned recipe prompt. Sampling is unchanged: seed 1, four steps, guidance 1.

Recipe `mc-qwen-image-edit-2511-v4` pins seed 1, eight steps, CFG 1, the matching Lightning 8-step V1.0 adapter and FukidashiErase, both at weight 1 (`deploy/cloud/common/manifest.py`). The model and adapter revisions stay pinned. The provisioner's content marker changes with the adapter filename, so setup seeds the matching adapter. FLUX recipes and sampling are unchanged.

**Three-seed check.** The worker renders up to three times, each with the next seed, and compares how much the hint changed against the rest of the crop (`MIN_EDIT_RATIO = 2` in `deploy/cloud/common/qwen.py`). It stops at the first render that passes. When none passes it returns the one that changed the lettering most. The worker never rejects a result: the user does. A deployment from before that rule can still answer `edit_not_applied` with no image, and the app then tells the user to update the cloud setup. A passing ratio is evidence of pixel change only. It does not prove the lettering is gone or the artwork survived, and no automatic OCR quality claim is made.

The generic prompt builder has not been benchmarked on a GPU against the earlier hand-written prompt.

The batch estimate for Qwen uses a linear eight-step extrapolation from an earlier four-step measurement and adds two renders to its high bound. It is not a GPU measurement.

## 8. The working size of the FLUX 3 recipes

The FLUX 3 edit recipes (`mc-flux3-klein-edit-v1`, the 4 bit weights, and `mc-flux3-klein-bf16-edit-v1`) may carry `work_long_side`: 768, 1024 or 1280. The worker scales the crop up until its long side is that size, renders, and scales the answer back (`working_size` in `deploy/cloud/common/flux.py`). A crop already larger is edited at its own size. A request without the field is edited at 768. The app sends nothing for 768, so a request at the default hashes as it did before the field existed.

The recipes and the sizes live twice and a test ties them: `SIZED_RECIPES` and `WORK_LONG_SIDES` in `crates/cleaner-core/src/engines/render.rs`, `SIZED_RECIPE_IDS` and `WORK_LONG_SIDES` in `deploy/cloud/common/contract.py`. Any other recipe or size is rejected on both sides, and a worker from before the field rejects any request that names it.

The size is a choice per request, not part of the deployment: deployment equality ignores it. The request digest, the durable attempt record and the patch's provenance carry it, and the cost estimate uses it. The app reads it from the `cloudWorkLongSide` setting.

## 9. The reference generation of the FLUX 3 recipes

The same recipes may carry `flux3_generation`, four booleans that each build one part of the edit the way the reference tool ("Manga Cleaner Reproduction", generation v2) builds it (`Flux3EditRunner` in `deploy/cloud/common/flux3.py`):

- `reference_prompt`: its prompt (`REFERENCE_FLUX3_PROMPT` in `deploy/cloud/common/manifest.py`), with its own words for where the lettering is ("upper centre") and a description as " Target description: ...".
- `marker`: the grown hint painted magenta (255, 0, 255) into the crop before it is resized, and its marker sentence added to the prompt. A result with more than max(16, 1 %) magenta pixels in the grown hint fails with `marker_remained`; another seed may pass.
- `reference_shift`: time shift 6.93 (its `mu` 1.93586) in place of 16.
- `megapixel_size`: one megapixel, each side the nearest multiple of 32, scaled up or down (`megapixel_size` in `deploy/cloud/common/flux.py` and `crates/cleaner-core/src/engines/render.rs`), in place of the working size. A request that sets it carries no `work_long_side`.

A record with every part false is refused: a request for this app's own way carries none, and hashes as it did before the field existed. Any other recipe, an unknown key or a non-boolean is refused on both sides, and a worker from before the field refuses any request that names it, so a profile must run the current cloud code first. Deployment equality ignores the field; the request digest, the attempt record, the patch's provenance and the cost estimate (`RenderRate::working_size`) carry it. The app reads it from the `cloudFlux3Generation` setting.

The reference tool's crop context (half the lettering's size per side, 64 to 512 px), its prompt upsampler and its INT8 weights are not part of the field: see "What has not been measured" in [findings.md](findings.md).

The client's finishing of the answer (`cloudFinishing`, `crates/cleaner-core/src/engines/postprocess.rs` `Finishing`) is never sent: it rides on the recipe, the attempt record and the patch's `postprocess` record, outside the request digest.

---

See also: [cloud-transport.md](cloud-transport.md), [cloud-journal.md](cloud-journal.md), [cloud-consent.md](cloud-consent.md), [cloud-security.md](cloud-security.md), [cloud-provisioning.md](cloud-provisioning.md), [cloud-frontend.md](cloud-frontend.md).
