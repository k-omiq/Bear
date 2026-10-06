# Manga Cleaner: Cloud Security Architecture & Backend Foundation

**Milestone Status:** P3a Delivered (Backend Security, Profiles, Credentials, Policy & Grant Foundation)
**Remaining Milestone Scope (at P3a):** P3b (Backend Region Binding Integration & Consent Copy), P4 to P11 (Durable Transport, Adapters, Provisioning, Batching, Release Gates). Later work implemented P3b, P4 and P5 to P9 offline. P10 is not implemented, and no phase from P5 on has run live. See [cloud-work-log.md](cloud-work-log.md).

---

## 1. Threat Model & Security Invariants

The Manga Cleaner cloud inference architecture is governed by strict fail-closed security and data-minimization boundaries designed to protect user privacy, prevent unauthorized execution, and eliminate credential exposure:

1. **Crop-Only Transmission & Zero Coordinate Leakage:** Only tightly bounded raster crops snapped to `LATENT_STRIDE = 16` and context padding are transmitted. Full pages, page-relative coordinates (`x`, `y`, `page_x`, `page_y`, `strip_rect`, `on_page`), region IDs, and local filesystem paths are strictly forbidden in wire payloads (`/mc/v1`).
2. **Local Default Target:** The default execution target is always [`ExecutionTarget::Local`]. Remote cloud execution requires explicit user configuration and backend grant issuance.
3. **Simultaneous Versioned Public Profiles:** Both Beam and Modal profiles can be configured and persisted simultaneously in dedicated `inference.json`. Public configurations enforce `deny_unknown_fields` and never contain secrets.
4. **URL & Host Hygiene (`reqwest::Url`):** All remote endpoints must use HTTPS with valid hostnames. Embedded user credentials (`@`), query parameters (`?`), URL fragments (`#`), control characters, surrounding whitespace, and conservative IP literals / loopback domains (`localhost`, `127.0.0.1`, `[::1]`, `0.0.0.0`, `10.0.0.0/8`, `192.168.0.0/16`, `172.16.0.0/12`, `169.254.0.0/16`, WHATWG octal/hex/decimal IPs, IPv4-mapped IPv6, and `.local`/`.localhost`/`.internal`/`.arpa`) are strictly rejected following trailing-dot normalization. (DNS resolution and connect-time network verification remain P3b scope).
5. **Canonical Origin Fingerprinting:** Profile URLs are canonicalized (`https://<host>` or `https://<host>:<port>`) and hashed to generate a deterministic SHA-256 fingerprint for binding secrets and grants.
6. **Origin-Bound Credential Isolation:** Cloud API keys and tokens are stored in the OS credential manager or an explicit in-memory session-only store, bound directly to the profile's canonical origin fingerprint. Changing a profile's endpoint URL prevents reuse of old credentials.
7. **Role Separation:** Cloud credentials are partitioned into separate scopes (`Setup`, `Runtime`, `ModelDownload`) that cannot substitute for or overwrite one another.
8. **Compiler-Barrier Zeroization:** [`SecretValue`] uses the `zeroize` crate (`#[derive(Zeroize, ZeroizeOnDrop)]`) to prevent compiler dead-store elimination, never derives `Serialize`, and formats as `"[REDACTED]"`.
9. **Write-Only Credential Surface & Sanitized Errors:** Secret management commands are write-only and query endpoints return safe metadata summaries ([`SecretSummary`]) without ever returning secret strings or echoing raw backend errors to JavaScript.
10. **Generic Settings Guard:** The generic settings writer recursively rejects all cloud secret/token keys to prevent accidental leakage into `settings.json`.
11. **Backend Authorization Grants:** Remote diffusion requires an unexpired, unrevoked, attempt-limited grant issued by the backend over an immutable scope (`provider`, `profile_id`, `canonical_endpoint_fingerprint`, `source_hash`, `crop_sha256`, `crop_bounds`, `mask_hash`, `revision`, `recipe`, and `region_ids: Vec<String>`). Grants expire at `now >= deadline`, detect system clock rollback, and are automatically revoked when profiles or credentials are modified.

---

## 2. Public Profile Storage (`inference.json`)

Public configuration is isolated from generic application preferences and persisted atomically to `inference.json` under the app config directory.

```json
{
  "schemaVersion": 1,
  "selectedTarget": {
    "type": "local"
  },
  "beamProfiles": {
    "beam-prod": {
      "id": "beam-prod",
      "name": "Beam GPU Cluster",
      "endpointUrl": "https://api.beam.cloud/endpoint1",
      "canonicalOrigin": "https://api.beam.cloud",
      "canonicalOriginFingerprint": "...",
      "createdAtMs": 1726780000000,
      "updatedAtMs": 1726780000000
    }
  },
  "modalProfiles": {
    "modal-prod": {
      "id": "modal-prod",
      "name": "Modal FLUX Schnell",
      "endpointUrl": "https://user-app.run.modal.com",
      "canonicalOrigin": "https://user-app.run.modal.com",
      "canonicalOriginFingerprint": "...",
      "createdAtMs": 1726780000000,
      "updatedAtMs": 1726780000000
    }
  }
}
```

### Schema & ID Validation Rules

- `schema_version`: Must equal `CURRENT_SCHEMA_VERSION` (1). Version 0 or future versions fail closed.
- `profile_id`: Bounded length (1..=64 chars), ASCII alphanumeric, `-`, and `_`.
- `endpoint_url`: Validated by `validate_https_endpoint` using `reqwest::Url`.
- Bounds: Maximum 32 profiles per provider, maximum 128 characters per profile name.

---

## 3. Credential Isolation & Role Architecture

Credentials are stored in the OS credential store under service `com.mangacleaner.studio.cloud` with account user format `<provider>:<profile_id>:<canonical_origin_fingerprint>:<role>`.

| Role | Intended Scope | Storage Lifetime |
|---|---|---|
| `Setup` | Workspace/account token for deployment helper inspection and resource creation | OS Keyring or ephemeral session |
| `Runtime` | Least-privilege proxy token for `/mc/v1` crop inference | OS Keyring or ephemeral session |
| `ModelDownload` | Token for weight snapshot downloads | OS Keyring or ephemeral session |

### Surviving app updates

A finished setup lives outside the app bundle, so an update replaces nothing it needs: the token in the OS credential store, the profile in `app_config_dir/inference.json`, the helper journal in `app_data_dir/cloud/journal`, and the permission in `settings.json`.

On macOS the login keychain guards an item's data with an access list that names the build that wrote it. An ad-hoc signed build is named by its code hash, so each update is a new program to the keychain and the first read of the token asks for the login password. Three things keep that from looking like a lost setup:

- The summary the interface reads (`get_cloud_secret_summary`) asks whether the item exists without reading its data (`macos_item_exists` in `secrets.rs`), which never prompts. After an update the setup still reads as ready.
- The first read of each credential in a launch is kept in process memory (`SecretManager::unlocked`, zeroized on drop). A successful save seeds the cache with the new value, so setup verification and requests do not read it back from Keychain. A delete or failed save clears it. Concurrent mutations are serialized and stale in-flight reads cannot repopulate it.
- macOS saves and deletes use attribute-based `SecItemUpdate`/`SecItemDelete` queries in the same login keychain. They do not read the old password first, unlike the `keyring` macOS mutation paths. The OS still authorizes the mutation; existing access controls are preserved. Hugging Face saves and clears likewise update their process cache without a readback.
- A store that will not answer is reported as `secretLocked` ("your system password store is locked or blocked this app"), never as a missing token, so nobody is told to set up again.

Prompt counts depend on keychain lock state, item access controls, and the app signature; there is no unconditional one-prompt guarantee across separately protected credentials. A stable code-signing identity prevents each update from becoming a new program to the keychain. The release workflow supports one when the `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD` and `APPLE_SIGNING_IDENTITY` secrets are set.

Release builds ignore the `MANGA_CLEANER_PROVISIONER_BIN` and `MANGA_CLEANER_JOURNAL_ROOT` overrides: the first would hand pasted provider keys to whatever program it names, and the second would lose the record of what setup created. A resume that moves an installation to a new origin deletes the old runtime token and moves the billing (setup) token to the new origin.

---

## 4. Backend Authorization Grant Service

The [`GrantService`] provides backend-enforced admission control to guarantee that no paid cloud inference request can occur without validated authorization.

```text
Backend Workflow:
User Action -> Backend Region Preparation -> Validate Eligibility -> Mint Grant (opaque 256-bit nonce)
                                                                           |
                                                                    Atomic Mutex
                                                                           |
Execute Remote HTTP -> Consume Grant (validate scope + increment attempt) -> Dispatch /mc/v1
```

### Scope Binding Invariants

Grants bind the exact immutable execution context:
- `provider`, `profile_id` & `canonical_endpoint_fingerprint`
- `source_hash`, `crop_sha256` & `mask_hash`
- `crop_bounds` ([`cleaner_core::mask::Rect`])
- `revision` & `recipe` ([`cleaner_core::engines::render::RenderRecipe`])
- `region_ids: Vec<String>`
- `max_attempts` & `expires_at_ms`

Any mismatch between the grant's scope and the execution payload fails consumption immediately with [`GrantError::ScopeMismatch`].

Cloud detection runs have a separate chapter grant. Its proposal records each page index, source index and local SHA-256 hash. The confirmed grant carries that list through the run, and a changed page fails with `cloud_run_source_changed` before its first tile. The run checks the selected profile and its mutation epoch again before each tile request. A cloud clean render rechecks those conditions after consuming its per-region grant and immediately before its HTTP submit. Cloud clean proposals and confirmed grants each hold at most 16 live entries.

---

## 5. Tauri Application Permissions & Capabilities

Custom commands are governed by explicit application manifest declarations in `build.rs` and capability declarations under `src-tauri/permissions/cloud.toml` and `src-tauri/capabilities/cloud.json`.

Exposed Command Surface:
- `read_inference_config`: Read public configuration from `inference.json`.
- `write_inference_config`: Validate and write public configuration to `inference.json`.
- `store_cloud_secret`: Write-only credential storage into OS keyring or session store.
- `delete_cloud_secret`: Delete credentials from OS keyring and session store.
- `get_cloud_secret_summary`: Safe summary of credential presence and backend.

---

## 6. Milestone Register & Remaining Work

| Phase | Description | Status |
|---|---|---|
| **P3a** | Backend security foundation, public profiles, secrets, policy, grants, and settings guards | **Completed** |
| **P3b** | Backend region binding integration, frontend consent dialogs, i18n copy updates | Implemented offline |
| **P4** | Durable attempt journal, local result caching, crash recovery, and cancellation | Implemented offline |
| **P5** | First real cloud crop via Modal staging adapter | Implemented offline, not yet run live |
| **P6** | Beam runtime parity and token permission verification | Implemented offline, not yet run live |
| **P7 to P8** | Automatic provisioning helpers for Modal and Beam | Implemented offline, not yet run live |
| **P9** | Complete account connection and lifecycle UX | Implemented offline |
| **P10** | Cloud-assisted chapter workflows and conservative routing | Not implemented: chapter runs stay local only |
| **P11** | End-to-end integration and release gating | Partial: CPU-only CI and the release install step; no live end-to-end run |
