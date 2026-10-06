# Manga Cleaner Cloud Wire API Specification (`/mc/v1`)

**Protocol Version:** `1.0.0` (offline contract)
**Status:** P0 architecture notes, superseded for exact schemas by [cloud-contract.md](cloud-contract.md). Gateways exist in `deploy/cloud/` and have not yet run live.

---

## 1. Core Principles & Boundary Invariants

1. **Crop-Only Transmission & Zero Coordinate Leakage:** Only cropped raster bounds grown by context padding are transmitted. Never send full pages, source/canvas coordinates (`x`, `y`, `on_page`, `strip_rect`), project archives, or local filesystem paths.
2. **Provider-Agnostic Gateway:** Future Modal and Beam wrappers must expose this uniform `/mc/v1` REST interface. Provider-native container lifecycle and queuing details are encapsulated behind the gateway.
3. **No Blind Deduplication Guarantee:** The client-computed `request_digest` provides content and metadata integrity only. The server does not maintain an idempotency index or offer a digest-lookup endpoint.
4. **Host-Side Composition & Bounded Validation:** The cloud runner executes the diffusion recipe and returns an RGB patch. Encoded byte limits and decoded raster bounds are enforced before allocation/decode, followed by exact geometry validation. Context alpha blending, edge replication, and tone matching remain strictly local host-side operations in Rust.
5. **Diffusion Recipe Realities:** Reference implementation is the FLUX SDNQ recipe from `sidecar/manga_cleaner_sidecar/backend/sdnq.py` (`WORK_LONG_SIDE = 768`, `LATENT_STRIDE = 16`). Current diffusion pipelines lack native masked diffusion; grayscale hints are advisory and ignored by the network. Fixed prompt and hyperparameter allowlists prevent arbitrary prompt/code execution.

---

## 2. Canonical Request Digest & Identifiers

The P0 digest proposal is superseded by the ordered JSON array in
[cloud-contract.md](cloud-contract.md). The current protocol is `1.0.0`;
`cloud_wire.rs`, `contract.py`, and shared fixtures define its exact bytes.
Do not implement the former JCS formula or delimiter concatenation.

The digest binds content and recipe metadata. It is neither authorization nor
an idempotency guarantee. Every dispatch still needs a backend grant, and an
ambiguous submission must never be retried automatically.

---

## 3. Wire Endpoints (All Authenticated)

Every request requires authenticated transport headers adapted by the Rust client to the target provider (e.g. Modal proxy token ID + secret via proxy headers, or Beam runtime authentication tokens). Common unverified bearer claims are prohibited.

### `GET /mc/v1/health`
Proves control-plane reachability and gateway authentication only. Does NOT verify GPU allocation, model residency, or container warmup. Returns gateway status and provider identifier.

### `GET /mc/v1/model-info`
Returns authenticated runtime metadata: supported protocol version, pinned symbolic model identifier `<pinned-model-id>`, pinned revision `<pinned-model-revision>`, pinned recipe `<pinned-recipe-id>`, and provisional service limits.
- **Provisional Limits (Unmeasured):**
  - Max dimensions: 2048×2048 px; Max megapixels: 4.19 MP.
  - Fixture max PNG: 16 MiB; fixture max multipart: 32 MiB; Default worker deadline: 120s.
  *(These limits are still provisional and unmeasured. Cloud execution now runs with them as safety ceilings.)*

### `POST /mc/v1/jobs`
Accepts `multipart/form-data` containing:
- `metadata` (canonical JSON): includes `protocol_version`, `job_id`, `attempt_id`, `request_digest`, crop dimensions, and allowlisted recipe settings.
- `image` (PNG, RGB8, exact snapped dimensions).
- `hint` (PNG, Gray8, exact snapped dimensions).
- **Response `202 Accepted`:** Returns `{ "handle": "<opaque-handle>", "status": "pending", "request_digest": "..." }`.

### `GET /mc/v1/jobs/{handle}`
Returns authoritative execution status: `status` (`pending` | `running` | `completed` | `failed` | `cancelled`), execution timestamps, and nullable reported cost (`reported_cost_usd: Option<f64>`).

### `GET /mc/v1/jobs/{handle}/result`
Downloads raw output PNG bytes (RGB8, exact input dimensions). Validated by response headers: `X-MC-Result-Digest` (SHA256 of payload) and `X-MC-Reported-Cost-Usd`.

### `POST /mc/v1/jobs/{handle}/cancel`
Requests cancellation. Response returns `{ "handle": "...", "status": "cancel_requested", "acknowledged": true }`. Does NOT immediately transition to terminal `cancelled`; client must reconcile terminal state via subsequent status inquiry.

### `POST /mc/v1/warmup`
Separate, explicit, potentially billable worker readiness probe. Triggers GPU allocation and model weight loading to memory.

### `GET /mc/v1/gpu`
Which GPU containers are up right now. Read from heartbeats the GPU containers write to the job store (`deploy/cloud/common/gpu.py`); this route runs on the CPU gateway and never starts a GPU. A heartbeat older than its container's scale-down window plus a grace is treated as gone, because a container can die without removing it.
```json
{ "provider": "modal", "supported": true, "now": 1790000100.0,
  "containers": [{ "role": "render", "gpu": "L4", "state": "idle",
                   "started_at": 1790000000.0, "last_active_at": 1790000060.0,
                   "idle_seconds": 120, "scaledown_estimate_at": 1790000180.0 }],
  "list_price_usd_per_hour": { "L4": 0.8 } }
```
- `role`: `render` | `analysis`, at most one entry each. `state`: `starting` | `idle` | `busy`. Times are Unix seconds on the gateway's clock; `now` is that clock at the answer, so a client measures time left against it rather than its own clock. `scaledown_estimate_at` is set only for `idle`.
- `list_price_usd_per_hour` is the provider's list price for the deployed GPU, an estimate for display only; it may be empty.
- `supported: false` (with no containers) means the deployment cannot report its GPU (Beam today). A deployment older than this route answers `404`; clients treat both as "unknown", never as "down".
- If the heartbeat store cannot be read, Modal also answers `supported: false`. The container state is unknown until the store recovers.

### `POST /mc/v1/gpu/stop`
Stops GPU containers. The body is optional: `{}` or no body stops every role, and `{"role": "render"}` or `{"role": "analysis"}` stops only that container and only its work. An optional `"idle_only": true` makes it an idle release instead (below); absent or `false` is the plain stop. Any other body (another role, `null`, an extra key, an `idle_only` that is not a boolean, a non-object, invalid JSON) is `400` with `error_code: "invalid_request"`.
- `render`: every pending or running job remains in the active index, including when more than 64 are queued. Each is first polled at the provider. One the GPU already finished keeps its `completed` or `failed` result. A running job becomes `cancelled` only after Modal confirms its call termination.
- `analysis`: every analysis tile call in flight (tracked in the job store, bounded, dropped after 10 minutes) is cancelled with its container, so its request fails. A cancel that fails stays tracked for the next stop. New analysis tiles are refused for 15 s.
- A container that was busy and had its call terminated is gone, and its heartbeat is removed. Any other container that is up is sent a `release` call, which drains it and lets it exit; its heartbeat stays until the container itself leaves, so `GET /mc/v1/gpu` keeps listing it (and it keeps billing) until then. A role is listed in `stopped` only when its call was terminated or its release was actually sent.
- A per-role marker makes a container of that role that cold-starts within 60 s of the stop skip its model load; the next submission for that role clears it. On Modal a render cold start refuses to start instead (section 5).
- A new render or analysis request during the first 15 s after a plain stop receives a pre-enqueue refusal. Render admission and submission are serialized with stop in the gateway. A marker write failure, unknown job index, or failed call termination answers `503` with `error_code: "stop_uncertain"`; the job and heartbeat remain tracked so Stop can be tried again.

Idempotent: with nothing up it returns `stopped: []`.
```json
{ "provider": "modal", "stopped": ["render"], "cancelled_jobs": 1 }
```
`cancelled_jobs` counts render jobs only.

If Modal spawn raises after the job claim is stored, the gateway answers `503` with `error_code: "submission_unknown"`, without a pre-enqueue rejection body. The claim stays in the job store and another request using that attempt does not spawn a second render. Its status may remain pending without a native call ID; the client must not automatically retry or create a new attempt from this answer.

**Idle release** (`{"role": "analysis", "idle_only": true}` or `{"role": "render", "idle_only": true}`). The app sends the analysis one once a Text cleanup run with cloud detection has analyzed its last page (or stopped early), so the analysis GPU leaves now instead of billing its idle window while the computer cleans, and the render one once a batch cloud clean (`docs/detect-clean.md` §3) has its last region back. It cancels nothing and writes no release marker, so no tile is refused afterwards and the next container loads its model as usual. A role's `release` is sent only when its heartbeat reads `idle` and no work of that role is tracked in flight: for `analysis` no tile call, for `render` no job pending or running in the gateway's active job index (a job another device queued behind an idle heartbeat). A count that cannot be read counts as busy. A job turns terminal in the index when it is polled, so one whose caller stopped polling holds the render release back until a plain stop settles it; the GPU then scales down on its idle timer, as before. A busy, starting or absent container is left exactly as it is. The response has the same shape, with `cancelled_jobs: 0` and the released role in `stopped`. Without the marker, a release that races the provider's own scale-down can cold-start one container that loads its model and then leaves. The app ignores any failure (an older gateway's `400`, Beam's `501`, the network) and the GPU then scales down on its idle timer.
A deployment that cannot stop its GPU answers `501` with `error_code: "unsupported"`.

---

## 4. Client Lifecycle & Dispatch State Machine

```text
[Intent Persisted] -> [HTTP POST /jobs]
       |                      |
       | (Network Error)      +--> 202 Accepted -> [Handle Persisted] -> [Poll Status]
       v                                                                      |
[submission_unknown]                                                          +--> completed -> [Validate Result] -> [Durable Cache] -> [Project Commit]
(No auto-retry)                                                               +--> failed    -> [Log Error]       -> [Terminal Failed]
                                                                              +--> cancel    -> [cancel_requested]-> [Reconcile Cancel]
```

### Invariants:
1. **Intent-Before-Dispatch:** Client logs an intent record containing `{job_id, attempt_id, request_digest, grant_id}` before initiating network transmission.
2. **Ambiguous Transmission (`submission_unknown`):** If a network timeout, disconnect, or ambiguous 5xx/429 occurs prior to receiving a 202 handle, the attempt transitions locally to `submission_unknown`. Automatic retry or provider failover is strictly forbidden.
3. **Handle Durability:** The received `handle` must be durably committed before status polling starts.
4. **Result Validation & Durable Caching:** Before committing to the project, the downloaded raster is validated: encoded and decoded bounds are enforced before allocation/decode, followed by exact byte digest verification (`X-MC-Result-Digest`), RGB8 color mode, and post-decode pixel geometry identical to input bounds. Normal completed results MUST be validated and durably cached in the journal before project commit.
5. **Stale/Detached Results:** If a region is deleted or modified locally while a remote job is in flight, the completed patch is safely cached in the journal but detached from project commit.
6. **No Blind Retries:** Re-issuing `POST /jobs` is permitted only upon authoritative pre-enqueue rejection (confirmed structured error body from endpoint) or confirmed pre-transmission socket failure within the active grant. A raw HTTP 400 alone does NOT prove pre-enqueue rejection. Repeated reads of `/jobs/{handle}` or `/result` for an existing handle may retry with exponential backoff.

---

## 5. Modal GPU cold starts: memory snapshots

The analysis GPU class (`deploy/cloud/modal/app.py`) uses a Modal memory snapshot; the render `Worker` has its snapshot off. Modal runs every `@modal.enter(snap=True)` method on a cold start, snapshots the container, and starts later cold starts from that snapshot; `@modal.enter(snap=False)` methods run on every start, restores included. Wire behaviour does not change.

| Class | Options | Snapshot phase (`snap=True`, once per snapshot) | Every start (`snap=False`) |
|---|---|---|---|
| `Worker` (FLUX.2 Klein) | `enable_memory_snapshot=False` (no snapshot, so this phase runs on every start) | Load the pipeline from the weights volume onto the GPU and run one single-step warmup render. Raise `WorkerNotReady` instead of returning when the weights are not seeded, the load or warmup fails, or a stop's release marker is under 60 s old. | New heartbeat (new container id, `started_at` read now), release-marker check, `idle`. |
| `AnalysisGPU` (RT-DETR, SAM) | `enable_memory_snapshot=True` (CPU memory only) | Import torch and ONNX Runtime; build the runtime object. No CUDA context. | New heartbeat and release-marker check. ONNX Runtime CUDA sessions are created on the first tile, as before. |
| `gateway`, `seed_weights` | none | none | none |

- **No unloaded snapshot.** Modal snapshots only after every `snap=True` method has returned, and an exception there fails the container before any snapshot is taken (modal 1.5.5 `_runtime/user_code_imports.py`: `lifecycle_context` calls `maybe_snapshot` after `lifecycle_presnapshot`; `call_lifecycle_functions` runs under `handle_task_lifecycle_exception`, which raises `UserException`). The render snapshot phase therefore raises rather than returning without a loaded pipeline, so a missing seed cannot become the state every restore starts from. The next cold start tries again. Before seeding, a render fails with Modal's container startup error, and the gateway maps it back to the typed code (`weights_missing` and so on) from the `WorkerNotReady` it carries. A Modal render container therefore no longer stays up without weights to load them on a later job; the Beam worker still does.
- **Per start.** Heartbeat documents, their container ids and timestamps, and release-marker reads all happen in `snap=False`. The snapshot holds no heartbeat, clock reading or HTTP client. Module-level `modal.Dict` and `modal.Volume` handles are rehydrated by the SDK on first use after a restore (`_object.py`, `hydrate`). Render seeds come from each job (`torch.Generator(device="cpu").manual_seed`), so no RNG state carries over.
- **Invalidation.** A redeploy that changes the code or the class config (GPU type, memory, image) makes Modal build a new snapshot on the next cold start. Changing the volume does not. The render snapshot references weight files on the volume, so deleting or moving the pinned snapshot directory needs a redeploy afterwards. The first cold start after each deploy still pays the full load, plus the time to write the snapshot.
- **Why the analysis worker keeps CPU snapshots only.** GPU snapshots checkpoint every CUDA context in the process with NVIDIA `cuda-checkpoint` (`_runtime/gpu_memory_snapshot.py`). Neither the SDK nor Modal's docs say whether ONNX Runtime CUDA sessions survive a restore, so the sessions are created after it.
- **Why the render worker has no snapshot.** With `experimental_options={"enable_gpu_snapshot": True}` the one live run (2026-09-26) built a new GPU snapshot on every cold start, including a plain cold start on an unchanged deployment, and never restored one. Each build added about 1 min 45 s to the 55 s load, so a cold render took about 3 min against 52 s without. The cause is not known; docs/findings.md records it as open. Turning it back on is `WORKER_SNAPSHOT` in app.py.
- **Status.** GPU memory snapshots are experimental in Modal. The analysis CPU snapshot was written once live; its reuse and gain are not measured (docs/findings.md).

### Deployment compatibility and interrupted submissions

`GET /mc/v1/capabilities` uses the same authentication as health and model-info,
performs no GPU work, and returns `protocol_version`, `provider`, and `operations`.
Both providers require `jobs.submit`, `jobs.status`, `jobs.result`, and `jobs.cancel`.
Modal additionally requires `gpu.status`, `gpu.stop`, and `gpu.release_idle`.
Beam uses provider-managed GPU lifecycle and does not advertise those operations. Idle release is
`POST /mc/v1/gpu/stop` with `idle_only: true`. Missing operations or an older
endpoint produce "Gateway out of date. Redeploy the gateway" at connection check.
The model-info and analysis capability contracts continue to bind the exact recipe,
preprocessing, model revision, and graph pins. Redeploy the CPU gateway and GPU
classes together; do not clear durable job documents when upgrading.

Unknown Modal submissions use authenticated `GET /mc/v1/jobs/{handle}`. Only the
Modal dispatch backend has the desktop lookup contract: `handle-modal-` followed
by the first 32 hex characters of SHA-256 of UTF-8 `job_id + NUL + attempt_id`.
This is not a universal provider handle scheme. The desktop verifies the original
profile endpoint fingerprint and validates every response identity and recipe
field against the saved request. Pending, missing, unavailable, and mismatched
answers remain unresolved, with no replacement POST. A resolved answer is saved
as `lookup_evidence` before the journal transitions from Unknown to Accepted.
Existing journals without this optional field remain readable. Beam uncertainty
remains unresolved until it has an explicit lookup contract.

Recovery never uploads images. Cached results retain their original input and
predecessor bindings. Startup audits committed entries against manifest patch
identities and buffers; missing patches or changed inputs request repair and
retain journals, result PNGs, and revision buffers. New intents are serialized by
region and refused while another attempt for that chapter/page/region remains
unresolved, even if a fresh grant was issued.

A proven pre-send failure or identity-bound `enqueued:false` rejection closes the
attempt as Cancelled without a handle. A fresh consent grant may retry that region.
Only transport failures that could hide acceptance become Unknown. Recovery
lookups and accepted-job waits run independently in background recovery workers.

Cloud settings offers recovery on demand and an explicit Abandon action for stuck
attempts. `reconcileCloudRecovery({action: "abandon", attemptId,
acceptDuplicateRisk: true})` records an Abandoned phase containing the previous
phase and the confirmation timestamp. The confirmation warns that a remote job
may still complete and that a later render may duplicate paid work. No remote
cancellation or replacement POST is implied. All evidence stays on disk.

A durable scope index tracks unresolved attempts, with one migration scan for old
journals. Unreadable journals block only their recoverable chapter/page/region
scope. Committed audits cache a marker keyed by manifest, undo history, and source
file metadata. Source hashing is shared per chapter snapshot outside the chapter
writer lock. A changed region revision, recorded undo/replacement, deleted region,
or deleted chapter supersedes the old commit. A plain predecessor detection at
its original revision with no recorded action still requests repair. Library load
failures are load errors. Dismissing a repair writes `repair-acknowledged.json`;
it never deletes journal, result, or revision evidence.
