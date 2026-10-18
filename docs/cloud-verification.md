# Cloud design decisions and invariants

This document records the architecture decisions behind cloud rendering and the invariants every cloud path relies on: which entry points can reach the cloud, how geometry and masks stay local, and how provenance, secrets and the attempt journal are kept. The wire contract is in [cloud-contract.md](cloud-contract.md).

---

## 1. Entry points that can reach the cloud

| Operation | Frontend entry point | Backend command | Rule |
|---|---|---|---|
| **Auto Run** | `startRun()` in `src/lib/state/editor.svelte.js` | `start_run` (`src-tauri/src/run.rs`) | `Pipeline::clean_page`. Automatic work stops at `HIGHEST_AUTOMATIC = Engine::Lama`. It never reaches the cloud on its own. |
| **Apply Tool** | `applyTool()` in `src/lib/editor/toolapply.svelte.js` | `apply_tool` (`src-tauri/src/region.rs`) | Manual tools (Brush, Clone, Fill, LaMa, Flux, Cloud). A cloud choice needs a consent grant. |
| **Create Region** | `createRegion()` in `src/lib/editor/drawing.svelte.js` | `create_region` | User-drawn box or lasso. The region is created and rendered locally. A cloud render of it is a separate step on the stored region. |
| **Rerun Mask** | `rerunMask()` in `src/lib/editor/maskactions.svelte.js` | `rerun_mask` | Re-executes an existing patch. It validates the engine choice and needs a grant when it targets the cloud. |
| **Clean Anyway** | `cleanAnyway()` in `src/lib/editor/maskactions.svelte.js` | `clean_anyway` | Cleans a gate-skipped region with the engine the user picked. It is always local. |

Batch cloud cleaning and cloud detection have their own plan and consent flow ([detect-clean.md](detect-clean.md), [cloud-security.md](cloud-security.md)).

---

## 2. Geometry, masks and lifecycle invariants

1. **Source ink mask against applied blend mask.**
   - The *source ink mask* is the detected or drawn stroke contour (`Fitted::mask` in `crates/cleaner-core/src/fit/`). It decides what export preserves.
   - The *applied blend mask* is the dilated and feathered geometry (`applied_mask()` and `AlphaRamp` in `crates/cleaner-core/src/engines/model.rs`).
   - Pixels outside the applied blend mask are bit-identical to the original page. Remote diffusion output is blended locally, as `alpha * model + (1.0 - alpha) * original`, with dither.
2. **Longstrip coordinates.** Multi-segment longstrips map canvas rectangles (`strip_rect`) to page coordinates (`on_page`). Every remote crop bound is expressed in canonical page-relative coordinates on the host. Those coordinates never go on the wire.
3. **Undo and history.** In the frontend, undo restores historical snapshots through `applyRegionDelta` and `recordEdit`. In the backend, `restore_region` (`src-tauri/src/library.rs`) toggles mask visibility. Undo and redo restore cached local patch states and never send a network request.

---

## 3. Architecture decision records

### 3.1 Engine identity against execution target

- **Decision:** `Engine` (`Fill`, `Lama`, `Flux`, `Cloud`, `Paint`, `Clone`) is separate from `ExecutionTarget` (`Local`, `Beam { profile_id }`, `Modal { profile_id }`).
- **Rationale:** Algorithm choice stays independent of hosting. A cloud FLUX run records `engine: Engine::Flux` with the provider in the cloud provenance. `Engine::Cloud` survives only so legacy manifests still read. The old `denoise` rung reads as `Fill`. The ladder in `src/lib/model/ladder.js` is `fill`, `lama`, `flux`, with a safe stepping fallback for a legacy `cloud`.

### 3.2 Backend-issued scoped authorisation grant

- **Decision:** Cloud execution needs an authoritative, backend-issued opaque grant.
- **Scope:** The grant binds the provider and profile ID, the endpoint fingerprint, the SHA-256 of the source crop and mask, the context padding, the recipe and model revision, the region set, the maximum attempt count and the expiry.
- **Enforcement:** The grant is checked and consumed atomically before dispatch. The client never sends local file paths, page coordinates or internal region IDs to a gateway.
- **Deployment plans:** Provisioning approval hashes the canonical reviewed resource plan, so scope cannot widen silently ([cloud-provisioning.md](cloud-provisioning.md)).
- **Detail:** [cloud-consent.md](cloud-consent.md) and [cloud-security.md](cloud-security.md).

### 3.3 Nullable cost and project format

- **Decision:** `CloudRecord.cost` (`crates/cleaner-core/src/patch.rs`) is `Option<f64>`, and an unknown cost serialises as `null`. The interface shows an unknown cost as unknown and never coerces it to zero (`masks.value.cloudCostUnknown`).
- **Provenance fields:** `CloudRecord` also holds optional `profile_id`, `job_id`, `attempt_id`, `recipe_id`, `model_revision`, `tier` and `duration_ms`, all skipped when empty.
- **Format:** The project manifest format (`FORMAT_VERSION` in `crates/cleaner-core/src/project/mod.rs`) was bumped for these fields while `OLDEST_READABLE_VERSION = 1` stays. Older projects decode and are upgraded in memory without touching disk until a flush. A version above the current one is refused.

### 3.4 Secret storage

- **Decision:** Cloud setup credentials and runtime tokens never use the plaintext `settings.json` fallback that exists for the Hugging Face token (`src-tauri/src/settings.rs`).
- **Policy:** Secrets live in the OS keyring. Only when no keyring is available do they fall back to in-memory session storage. See [cloud-security.md](cloud-security.md) section 3.

### 3.5 Journal ownership and locking

- **Decision:** Inference attempts, handles and cached results live in a dedicated file-based journal (`src-tauri/src/inference/journal.rs`, [cloud-journal.md](cloud-journal.md)). It is not a database and not part of the project manifest.
- **Locking:** Each attempt has an OS file lock. The existing single-writer project discipline stays. Releasing the project lock around network work needs an immutable snapshot and a revision check before attachment, which the journal's attachment reconciler provides.

---

## 4. Render seam

Raster crop preparation and host-side composition are pure functions in `crates/cleaner-core/src/engines/render.rs`:

- `PreparedRender` holds private fields behind read-only getters (`bounds()`, `crop()`, `pad()`, `image_rgb8()`, `hint_gray8()`, `applied()`).
- `PreparedRender::prepare` enforces non-zero dimensions and engine eligibility, computes proportional context (`CONTEXT_MIN = 24`, `CONTEXT_MAX = 80`), snaps to `LATENT_STRIDE = 16`, edge-replicates pixels off the page (`EdgePad::Replicate`), builds the grayscale hint from `Fitted::ink`, and captures the unedited target patch and the `AlphaRamp`.
- `PreparedRender::composite` clones the captured patch and applies the captured `AlphaRamp`. It needs no `page` or `fitted` again, so the caller cannot desynchronise them. It checks the geometry and byte length of the `GeneratedCrop`, blends through the `AlphaRamp`, copies alpha unchanged and dithers 16-bit boundaries.
- The local sidecar `Inpainter::render` uses the same two functions, so local and cloud results composite identically.

The same file defines `CloudProvider` (`Beam`, `Modal`), `ExecutionTarget` and `RenderRecipe`. Recipes are pinned and carry no invented defaults.

---

See also: [cloud-contract.md](cloud-contract.md), [cloud-consent.md](cloud-consent.md), [cloud-security.md](cloud-security.md), [cloud-journal.md](cloud-journal.md), [cloud-frontend.md](cloud-frontend.md), [features.md](features.md).
