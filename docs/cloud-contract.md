# Manga Cleaner Provider-Neutral Cloud Wire Contract (`/mc/v1`)

**Protocol Version:** `1.0.0`
**Status:** Canonical P2c Wire Contract (Offline Shared Schema & Validation Only). Gateway implementations now exist in `deploy/cloud/` and follow this contract. They have not run live.

---

## 1. Scope & System Boundary

This specification defines the provider-neutral wire contract schemas, canonical digest, bounded validation rules, and lifecycle state machines shared between Manga Cleaner client services and future cloud gateways (Modal and Beam).

> [!IMPORTANT]
> **Offline Contract Only:** Deployed cloud gateways, remote containers, and provider HTTP endpoints are absent in P2c. All structures and validators operate entirely in-memory and on shared offline JSON test fixtures.

### Key Invariants

1. **Crop-Only Transmission & Zero Coordinate Leakage:** Only tightly cropped raster bounds (snapped to multiples of `LATENT_STRIDE = 16` with context padding) are transmitted. Full pages, page-relative coordinates (`x`, `y`, `page_x`, `page_y`, `strip_rect`, `on_page`), region IDs, and local filesystem paths are strictly forbidden in request payloads. Deserialization denies unknown fields.
2. **Deterministic Collision-Free Canonical Digest (`MC-REQ-V1`):** Content and metadata integrity is computed via a compact, versioned JSON ordered array of restricted ASCII strings, integers, and booleans. This eliminates delimiter collisions (e.g., `job_id="a|b", attempt_id="c"` vs `job_id="a", attempt_id="b|c"`) and floating-point JSON canonicalization variances across Rust and Python.
3. **Exact Identity & Response Binding:** Server responses (`202 Accepted`, status polling, result download, and cancellation) must bind `handle`, `job_id`, `attempt_id`, `request_digest`, and all recipe identity fields (`recipe_id`, `preprocessing_version`, `model_id`, `model_revision`, `native_mask_conditioning`).
4. **Honest Capability Reporting:** `GET /mc/v1/model-info` honestly advertises `native_mask_conditioning: false` for current FLUX SDNQ recipes. Advisory grayscale hints are accepted on the wire, but requests claiming native masked diffusion are rejected pre-submit.
5. **Fail-Closed Validation:** Unknown versions, unexpected JSON fields, mutable revision tags (`main`, `master`, `latest`, `head`, `dev`, `staging`), non-hex revisions, unaligned dimensions, non-finite costs, or out-of-bound limits fail closed before enqueueing.
6. **Structured Pre-Enqueue Rejection vs Ambiguous Acceptance:** A structured `PreEnqueueRejectionResponse` confirming `enqueued: false` with bound identity permits safe retry on allowlisted error codes. Network disconnects during submission transition locally to `submission_unknown` without blind re-submission.
7. **Bounded PNG Header & Mandatory P3 Decode Gate:** Wire validators verify PNG 8-byte signatures, `IHDR` chunk length 13, geometry, bit depth 8, color modes, and `IHDR` CRC-32 checksums before allocation. This header gate does not decompress IDAT rasters; full bounded decompression and alpha blend compositing remain a mandatory P3 host-side decode gate.
8. **Payload Limits vs Transport Multipart Framing:** P2c checks individual PNG buffer limits and payload pixel arithmetic. Full HTTP transport multipart wire framing/overhead limits are enforced in the P3 network transport layer.

---

## 2. Canonical Request Digest (`MC-REQ-V1`)

The canonical metadata is encoded as a compact JSON ordered array without whitespace:

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

The request digest is the lowercase hexadecimal SHA-256 hash of the UTF-8 bytes:
$$\text{request\_digest} = \text{hex}(\text{SHA256}(\text{CanonicalJSONArray}))$$

### P2c Test Vectors (Synthetic 40-Hex Revision)

- **Base Vector (`model_revision="0123456789abcdef0123456789abcdef01234567"`):**
  - Canonical JSON: `["MC-REQ-V1","1.0.0","job-test-001","attempt-test-001","test-sdnq-v1","1.0.0","test-flux-schnell","0123456789abcdef0123456789abcdef01234567",false,16,16,42,4,350,"2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f","3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238"]`
  - SHA-256: `42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0`
- **Collision Case A (`job_id="a|b", attempt_id="c"`):**
  - SHA-256: `88647a2f2f1351530c85b444cedde98d01ca9e9548242bc1aa4ac84f5e74dd2c`
- **Collision Case B (`job_id="a", attempt_id="b|c"`):**
  - SHA-256: `6298eeb660626079ffba0d848451d9184ee0f416691e52a7972dc9b9bc9c007c`

---

## 3. Endpoints & Shared Schemas

### 3.1 `GET /mc/v1/health`
Proves control-plane reachability and gateway authentication.
- **Response `200 OK`:**
  ```json
  {
    "status": "ok",
    "provider": "modal",
    "protocol_version": "1.0.0"
  }
  ```

### 3.2 `GET /mc/v1/model-info`
Returns worker capabilities, pinned model/recipe versions, and provisional service limits.
- **Response `200 OK`:**
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

### 3.3 `POST /mc/v1/jobs`
Enqueues a crop rendering job. Accepts `multipart/form-data` with `metadata`, `image` (PNG RGB8), and `hint` (PNG Gray8).
- **Request Metadata:**
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
- **Response `202 Accepted`:**
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

### 3.4 `GET /mc/v1/jobs/{handle}`
Authoritative execution status. Statuses: `pending`, `running`, `completed`, `failed`, `cancelled`.
- **Response `200 OK` (completed):**
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

### 3.5 `POST /mc/v1/jobs/{handle}/cancel`
Requests cancellation. Returns non-terminal `cancel_requested` status with bound request digest.
- **Response `200 OK`:**
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

### 3.6 Structured Pre-Enqueue Rejection
Confirms request was rejected before enqueueing.
- **Response `400 Bad Request` / `422 Unprocessable`:**
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
