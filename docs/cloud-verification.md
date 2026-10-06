# Manga Cleaner Cloud Verification Baseline & Architecture Decisions (P0 / P2a / P2b)

**Commit:** `03fcaba` | **Branch:** `codex/cloud-integration` | **Role:** P2b IMPLEMENTER
**Status:** Phase record of partial implementation. Current acceptance and actual test logs are tracked in [cloud-work-log.md](cloud-work-log.md); the architecture decisions it lists are now implemented offline (see [features.md](features.md) Cloud section, [cloud-frontend.md](cloud-frontend.md), [cloud-provisioning.md](cloud-provisioning.md), and [findings.md](findings.md)).

---

## 1. Test Baseline & Failure Diagnosis

### 1.1 Test Execution Evidence
- **Prior Review Results:** Core unit tests (`cargo test -p cleaner-core -- --test-threads=1`) passed 527 tests. Frontend tests (`npm test`) passed 913 tests (48 files) and `npm run build` succeeded (exit 0; logs at `/tmp/manga-cloud-p0-frontend.log`).
- **Targeted Baseline Reproduction (P0 Turn):**
  - Executed: `cargo test --target-dir /Users/caved/dev/manga-cleaner/target -p manga-cleaner --lib run::tests::the_shipped_picks_keep_bubble_text_off_the_inpainter -- --nocapture`.
  - Outcome: FAILED (exit 101; duration: 26.98s). Panicked at [`src-tauri/src/run/tests.rs:2456:13`](../src-tauri/src/run/tests.rs#L2456): `c1-p001-r8 is bubble text and ran at Lama`.
  - Evidence: Full terminal output preserved at `/tmp/manga-cloud-p0-baseline.log`.
  - Temporary Test Setup: Symlinks `models` and `runtimes` in the worktree pointing to local dev directories were used solely to resolve model weights without network downloads.
- **Limitation & Root Cause Analysis:**
  - The root cause remains **undiagnosed** on `03fcaba`. Two distinct hypotheses exist:
    1. Divergence between detector-only classification (`detected.inside()` at [`src-tauri/src/run.rs:1715`](../src-tauri/src/run.rs#L1715)) and script gate interior settlement (`gate/mod.rs:328`), routing region `r8` to `outside: EnginePick::Lama`.
    2. Region `r8` starts at `Fill` but fails the quality metric in `quality::assess` ([`src-tauri/src/run.rs:1989`](../src-tauri/src/run.rs#L1989)), escalating up `ladder_from` to `Engine::Lama`.
  - In accordance with P0 instructions, no production code or assertions are modified to force a pass; this remains an open baseline issue.

---

## 2. Source-Linked Entry-Point Inventory

| Operation | Frontend Entry Point | Backend Command | Processing Seam & Invariant |
|---|---|---|---|
| **Auto Run** | [`src/lib/state/editor.svelte.js:1310`](../src/lib/state/editor.svelte.js#L1310)<br>`startRun()` | [`src-tauri/src/run.rs:3086`](../src-tauri/src/run.rs#L3086)<br>`start_run` | `Pipeline::clean_page`. Preserves `HIGHEST_AUTOMATIC = Engine::Lama`. Never triggers remote inference. |
| **Apply Tool** | [`src/lib/editor/toolapply.svelte.js:93`](../src/lib/editor/toolapply.svelte.js#L93)<br>`applyTool()` | [`src-tauri/src/region.rs:2254`](../src-tauri/src/region.rs#L2254)<br>`apply_tool` | Manual editing tools (Brush/Clone/Fill/Lama/Flux). Legacy Cloud returns unavailable notice; future cloud routes to `RegionRenderer`. |
| **Create Region** | [`src/lib/editor/drawing.svelte.js:258`](../src/lib/editor/drawing.svelte.js#L258)<br>`createRegion()` | [`src-tauri/src/region.rs:2460`](../src-tauri/src/region.rs#L2460)<br>`create_region` | User-drawn box/lasso. Invokes `Bench::render_plan`. Backend authorization required before remote dispatch. |
| **Rerun Mask** | [`src/lib/editor/maskactions.svelte.js:220`](../src/lib/editor/maskactions.svelte.js#L220)<br>`rerunMask()` | [`src-tauri/src/region.rs:2565`](../src-tauri/src/region.rs#L2565)<br>`rerun_mask` | Re-executes existing patch. Validates engine choice; must verify backend grant if targeting remote cloud. |
| **Clean Anyway** | [`src/lib/editor/maskactions.svelte.js:268`](../src/lib/editor/maskactions.svelte.js#L268)<br>`cleanAnyway()` | [`src-tauri/src/region.rs:2786`](../src-tauri/src/region.rs#L2786)<br>`clean_anyway` | Cleans gate-skipped regions using user-chosen engine pick; requires backend authorization if cloud target. |

---

## 3. Geometry, Masks, and Lifecycle Invariants

1. **Source Ink Mask vs. Applied Blend Mask:**
   - *Source Ink / Export Mask:* The detected or drawn stroke contour (`Fitted::mask` in [`crates/cleaner-core/src/fit.rs`](../crates/cleaner-core/src/fit.rs)). Dictates export preservation.
   - *Applied Blend Mask:* The dilated and feathered geometry (`applied_mask()` and `AlphaRamp` in [`crates/cleaner-core/src/engines/flux.rs:348-380`](../crates/cleaner-core/src/engines/flux.rs#L348-L380)).
   - *Invariant:* Pixels outside the applied blend mask are bit-identical to the original page. Remote diffusion output is blended strictly locally via `alpha * model + (1.0 - alpha) * original` with dither.
2. **Longstrip Coordinates:**
   - Multi-segment longstrips map canvas rectangles (`strip_rect`) to page coordinates (`on_page` in [`src-tauri/src/run.rs:1711`](../src-tauri/src/run.rs#L1711)). All remote crop bounds must be expressed in canonical page-relative coordinates.
3. **Undo & History Lifecycle:**
   - In frontend, undo restores historical snapshots via `applyRegionDelta` / `recordEdit`. In backend, `restore_region` ([`src-tauri/src/library.rs:2488`](../src-tauri/src/library.rs#L2488)) toggles mask visibility (`set_mask_visible`).
   - *Invariant:* Undo and redo restore cached local patch states and never trigger remote network requests.

---

## 4. Architecture Decision Records (ADRs)

### 4.1 Engine Identity vs. Execution Target
- **Decision:** Separate `Engine` (`Fill | Denoise | Lama | Flux | Paint | Clone`) from `ExecutionTarget` (`Local | Beam(profile_id) | Modal(profile_id)`).
- **Rationale:** Decouples algorithm selection from provider hosting. A cloud FLUX run yields `engine: Engine::Flux` with provider recorded in cloud provenance.

### 4.2 Backend-Issued Scoped Authorization Grant
- **Decision:** Cloud execution requires an authoritative, backend-issued opaque grant.
- **Scope:** Binds provider/profile ID, endpoint fingerprint, SHA256 of source crop and mask, context padding, recipe/model revision, region set, maximum attempt count, and expiry timestamp.
- **Enforcement:** Atomically checked and consumed prior to dispatch. Client never sends local file paths, page coordinates, or internal region IDs to remote gateways.
- **Deployment Plans:** Automatic deployment approval hashes the canonical reviewed resource plan to prevent silent scope expansion.

### 4.3 Nullable Cost Semantics & Project Format v3
- **Decision:** Update `CloudRecord.cost` ([`crates/cleaner-core/src/patch.rs:74`](../crates/cleaner-core/src/patch.rs#L74)) from mandatory `f64` to `Option<f64>`.
- **Migration Policy:** Bump [`crates/cleaner-core/src/project/mod.rs`](../crates/cleaner-core/src/project/mod.rs) to `FORMAT_VERSION = 3` while retaining `OLDEST_READABLE_VERSION = 1`. Old v1/v2 projects decode gracefully; unknown future versions (v4+) are rejected. Format migration will be accompanied by roundtrip and refusal test suites in P2.

### 4.4 Secret Storage vs. Existing Insecure Fallback
- **Decision:** Cloud setup credentials and runtime tokens must NEVER use the insecure plaintext `settings.json` fallback that exists for HuggingFace tokens ([`src-tauri/src/settings.rs:87-148`](../src-tauri/src/settings.rs#L87-L148)).
- **Policy:** Secrets are stored strictly in OS keyrings, falling back only to session-in-memory storage when OS keychains are unavailable.

### 4.5 Journal Ownership & Single-Instance Locking
- **Decision:** Inference attempts, handles, and cached results are tracked in a dedicated auxiliary transactional journal (SQLite is a proposed candidate architecture, not an existing repo dependency).
- **Locking Policy:** Single-instance application lease policy. Preserve the existing single-writer project discipline initially. Releasing a project lock around network work requires an immutable snapshot and revision validation before attachment; this is not implemented yet.

---

## 5. P2a Core Seam Implementation Evidence

### 5.1 Pure Render Extraction & Contract Separation
- Extracted pure raster crop preparation and host-side composition into [`crates/cleaner-core/src/engines/render.rs`](../crates/cleaner-core/src/engines/render.rs).
- Implemented [`PreparedRender`] and [`GeneratedCrop`] contracts:
  - `PreparedRender` encapsulates private fields with read-only getters (`bounds()`, `crop()`, `pad()`, `image_rgb8()`, `hint_gray8()`, `applied()`).
  - `PreparedRender::prepare`: enforces non-zero dimensions and legitimate engine eligibility, computes proportional context (`CONTEXT_MIN = 24`, `CONTEXT_MAX = 80`), snaps to `LATENT_STRIDE = 16`, edge-replicates pixels off-page (`EdgePad::Replicate`), isolates the grayscale hint mask from `Fitted::ink`, and captures the target unedited patch and `AlphaRamp`.
  - `PreparedRender::composite`: clones the captured bounded patch and evaluates the captured `AlphaRamp` without requiring `page` or `fitted` to be passed again, preventing caller desynchronization by construction. Performs explicit checked geometry/byte length verification on `GeneratedCrop` before compositing, blends via `AlphaRamp`, copies alpha unmodified, and dithers 16-bit boundaries.
- Local sidecar [`Inpainter::render`](../crates/cleaner-core/src/engines/flux.rs) delegates directly to `PreparedRender::prepare` and `PreparedRender::composite`, preserving existing sidecar protocol and memory observation guards without behavioral divergence.

---

## 6. P2b Domain, Provenance & Vocabulary Implementation Evidence

### 6.1 Domain Types & Provenance Structure
- Defined typed [`CloudProvider`] (`Beam`, `Modal`), [`ExecutionTarget`] (`Local`, `Beam { profile_id }`, `Modal { profile_id }`), and [`RenderRecipe`] in [`crates/cleaner-core/src/engines/render.rs`](../crates/cleaner-core/src/engines/render.rs) and exported them via [`cleaner_core::engines`](../crates/cleaner-core/src/engines/mod.rs).
- Extended [`CloudRecord`] in [`crates/cleaner-core/src/patch.rs`](../crates/cleaner-core/src/patch.rs):
  - `cost` evolved to `Option<f64>` with honest null semantics.
  - Added optional `profile_id`, `job_id`, `attempt_id`, `recipe_id`, `model_revision`, `tier`, and `duration_ms` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - Preserved `Engine::Cloud` for reading legacy manifests without inferring or fabricating models.
  - New remote records represent `Engine::Flux` with `provenance.cloud` populated.

### 6.2 Project Manifest Format v3 Migration
- Bumped `FORMAT_VERSION = 3` in [`crates/cleaner-core/src/project/mod.rs`](../crates/cleaner-core/src/project/mod.rs) while keeping `OLDEST_READABLE_VERSION = 1`.
- Verified non-mutating reads: opening a v1 or v2 manifest upgrades in-memory version to 3 without modifying disk until explicit flush.
- Verified version gate: future versions (e.g. v4) are refused with `StoreError::Version { expected: 3, found: 4 }`.
- Verified undo/export serialization on patches and manifests without invoking render models or network operations.

### 6.3 Frontend Vocabulary & UI Policy Reconciliation
- Reconciled [`src/lib/model/ladder.js`](../src/lib/model/ladder.js): `RUNGS = ['fill', 'denoise', 'lama', 'flux']`, with safe stepping fallback for legacy `cloud`.
- Updated [`src/lib/model/masks.js`](../src/lib/model/masks.js) `reRunnable(mask)` policy: returns `false` for legacy `cloud` and remote FLUX masks with cloud provenance, preventing unconfirmed paid executions from Layers row menus.
- Honest cost display: updated [`src/lib/model/masks.js`](../src/lib/model/masks.js), [`src/lib/editor/maskrows.js`](../src/lib/editor/maskrows.js), and [`src/lib/editor/cloudflow.svelte.js`](../src/lib/editor/cloudflow.svelte.js) so null costs are not coerced to fake zero.
- Updated typedefs in [`src/lib/model/types.js`](../src/lib/model/types.js).

### 6.4 Verification Evidence & Test Ownership
- **P2b Source Fixes Completed:**
  - `CloudCostDialog.svelte` & `en.js`: updated cost evaluation (`isCostKnown = rawCost >= 0`), added explicit unknown cost copy (`modal.body.cloudCostUnknown`), removed false promises ("billed on return", "rejected request is not billed") from both strings.
  - `src-tauri/src/library.rs`: implemented conservative bounded symbolic allowlist (`[a-zA-Z0-9._\-/]`, len <= 128, no URLs/userinfo/control/whitespace) in `sanitize_identifier`, documented lexical boundary guarantee (non-cryptographic secret detection), removed duplicate `ApiProvenance` docblock, and verified safe `"unknown"` fallback.
  - `crates/cleaner-core/src/project/mod.rs`: restored from HEAD to eliminate whole-file formatting churn; transplanted `FORMAT_VERSION = 3` / `OLDEST_READABLE_VERSION = 1` documentation, raw JSON v1 and v2 migration fixtures, v3 roundtrip tests, and non-inferring undo serialization tests.
  - `crates/cleaner-core/src/engines/render.rs` & `patch.rs`: explicit pinned `RenderRecipe` without fabricated defaults, serde-derived `ExecutionTarget`, and honest `cost: Option<f64>` serializing explicit `null`.
  - Frontend Routing & UI: `cleanRegionAutomatically` capped at `lama` for all automatic paths, `masks.provenance.cloudCost` retained with a `null` value, shown as an unknown cost (`masks.value.cloudCostUnknown`), and DOM tests added for `CloudCostDialog`.
- **Test Execution Ownership:** Full test execution (`cargo check`, `cargo test -p cleaner-core`, `cargo test -p manga-cleaner`, `cargo clippy`, `npm test`) is handed over to orchestrator. Source files have been pruned of EOF whitespace and prepared for verification.
