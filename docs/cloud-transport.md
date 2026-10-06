> Current implementation note: both modules are registered. Orchestrator checks
> pass eight loopback HTTP tests and ten decoder tests. Client requests explicitly
> disable retries. Result reads cap encoded bytes at 64 MiB regardless of server
> limits; decoding caps pixels at 16 Mi pixels and checks all chunk framing/CRCs
> through a final IEND with no trailing bytes. These are host safety ceilings,
> not measured serving limits. The status-to-result helper requires the expected
> handle from the accepted attempt. No live provider verification is claimed.

# Cloud Transport and Bounded Result Decode (P3b1)

## Architecture Overview

This document specifies the security controls, invariants, and implementation status for the bounded **P3b1** milestone of Manga Cleaner's cloud diffusion inference integration.

The implementation consists of two core components:
1. **Hardened HTTP Control Transport (`src-tauri/src/inference/http.rs`)**: An immutable, read-only HTTP transport client connecting the desktop application to Modal and Beam cloud backends over the provider-neutral `/mc/v1` REST wire contract.
2. **Bounded Result PNG Decoder (`crates/cleaner-core/src/cloud_decode.rs`)**: A memory-safe, bounded PNG decompression pipeline validating raw bytes against declared wire metadata, latent stride, single-frame RGB8 format, `.finish()` stream completeness, and SHA-256 digests before producing [`GeneratedCrop`] for host-side compositing.

---

## Security Invariants and Hardening Controls

### 1. Process-Global Single-Worker DNS & Anti-SSRF Protection
- **Single Process-Global Worker (`resolve_and_validate_host`)**: Uses a single background worker thread receiving requests over a bounded `sync_channel(32)` queue. If the DNS worker is stalled or the queue is full, new requests fail closed immediately with `HttpTransportError::DnsResolverBusy` without spawning additional threads.
- **Strict Public IP Validation (`is_public_ip`)**:
  - **IPv4**: Rejects all private, loopback, link-local, carrier-grade NAT (`100.64.0.0/10`), documentation (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`), benchmarking (`198.18.0.0/15`), broadcast, and multicast ranges.
  - **IPv6**: Allows **ONLY** global unicast within `2000::/3`, while explicitly excluding all special, transition, or documentation prefixes (`2001::/23`, `2002::/16`, `3ffe::/16`, `3fff::/16`). All addresses outside `2000::/3` (including `::/128`, `::1/128`, `fc00::/7`, `fe80::/10`, `fec0::/10`, `ff00::/8`, `100::/64`, `64:ff9b::/96`, `::ffff:0:0/96`) are rejected.
- **Connection IP Pinning**: Validated public socket addresses are pinned to the `reqwest` client via `resolve_to_addrs`, preventing time-of-check to time-of-use DNS rebinding attacks.
- **Localhost Test Gating**: Localhost/loopback injection is strictly gated to `#[cfg(test)]`.

### 2. Transport Client Hardening
- **HTTPS Enforced**: Enforces default rustls root certificate validation; plaintext `http://` endpoints fail closed.
- **All Redirects Denied**: `reqwest::redirect::Policy::none()` is enforced. Any redirect status code (`300..=399`) or presence of a `Location` header is rejected immediately with `HttpTransportError::RedirectForbidden`.
- **Zero Environment Proxy Leaks**: `.no_proxy()` ensures local environment proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`) are ignored.
- **Automatic Retries Disabled**: No automatic retry middleware is configured; network failures transition directly to caller error handling.
- **Timeouts**: Enforces `DEFAULT_CONNECT_TIMEOUT` (10s), `DEFAULT_REQUEST_TIMEOUT` (30s), and `DEFAULT_DNS_TIMEOUT` (5s).
- **Static Sanitized Error Reporting**: Public error enum variants (`HttpTransportError`) are static and leak-free; they never echo untrusted MIME types, bearer tokens, internal query parameters, raw URLs, or server error bodies.

### 3. Typed Runtime Credential Binding
- **Typed Provider Variants (`RuntimeCredential`)**:
  - `RuntimeCredential::BeamBearer(SecretValue)`: Sends `Authorization: Bearer <token>` with `val.set_sensitive(true)`.
  - `RuntimeCredential::ModalProxy { token_id, token_secret }`: Sends official `Modal-Key` and `Modal-Secret` headers with `set_sensitive(true)`.
- **No Invented Headers**: Does not invent vendor headers (e.g. `x-beam-token` or generic bearer claims).
- **Target Binding (`BoundRuntimeCredential`)**: Credentials must be explicitly bound to the target provider, profile ID, and canonical origin fingerprint. Arbitrary unbound secret parameters are prohibited in client constructors.
- **Memory Zeroization**: Secret strings are wrapped in [`SecretValue`], enforcing compiler-barrier zeroization on drop and formatting as `[REDACTED]` in `Debug` and `Display`.

### 4. Read-Only Control Surface
- **Implemented GET Endpoints**:
  - `GET /mc/v1/health`: Validates typed server health and asserts provider identity matching.
  - `GET /mc/v1/model-info`: Validates advertised recipe, model revision, service limits, and verifies `native_mask_conditioning = false` honesty.
  - `GET /mc/v1/jobs/{handle}`: Checks job status and validates response binding against the caller's [`JobRequestMetadata`].
  - `GET /mc/v1/jobs/{handle}/result`: Downloads raw PNG result buffers from the verified endpoint origin.
- **Zero POST Endpoints**: Job creation (`POST /jobs`) and cancellation (`POST /jobs/{handle}/cancel`) are intentionally omitted from this transport client.
- **Safe Route Construction**: Uses internal enums and `path_segments_mut` to append fixed route segments, preventing origin overwrites or path traversal. Opaque handles are strictly validated (`validate_ascii_id`) and reject dot segments, slashes, percent encoding, and special characters.
- **Exact MIME Essence Validation**: Verifies that the MIME type essence (ignoring parameters) exactly equals `"application/json"` for control endpoints and `"image/png"` for result endpoints.
- **Bounded Streaming**: All HTTP response bodies are read via `io::Read::take(max_bytes + 1)`, guaranteeing strict memory ceilings on streaming/chunked transfers.

### 5. Bounded PNG Result Decoder
- **Preflight Wire Check**: Calls `cleaner_core::cloud_wire::validate_result_bytes`, verifying IHDR header CRC-32, dimension alignment with latent stride (16), declared vs actual byte length, and SHA-256 payload digest.
- **Separate Encoded and Decoded Bounds**: Encoded stream byte limit is bounded by `ServiceLimits::max_png_bytes` (`png::Limits`), while decoded pixel count is bounded by `ServiceLimits::max_pixels`.
- **Format Verification**: Rejects interlacing, multi-frame APNG streams, non-RGB8 color types (e.g. Grayscale, RGBA, Indexed, CMYK), and bit depths other than 8.
- **Stream Completeness (`reader.finish()`)**: Decodes image data and consumes all remaining chunks to `IEND`, verifying chunk CRC and rejecting trailing corruption or garbage bytes.
- **Compositor Integration**: Outputs [`DecodedResultCrop`], which provides `.as_generated_crop()` to construct a [`GeneratedCrop`] for evaluation by [`PreparedRender::composite`].

---

## Status, Limitations, and Milestones

> [!IMPORTANT]
> **P3 Implementation Status**: P3 is **partially complete** (P3b1: hardened HTTP transport and result decode).
> Remote job submission (`POST /mc/v1/jobs`) is **intentionally unavailable** in this build.
> Vendor authentication headers are prepared based on P1 feasibility specifications and have not been executed against live cloud accounts.

| Milestone | Scope | Status | Notes |
|---|---|---|---|
| **P3a** | Cloud Wire Contract & Schemas | **Complete** | Defined in `crates/cleaner-core/src/cloud_wire.rs`. |
| **P3b1** | Hardened HTTP Transport & Result Decode | **Complete (Source + Tests)** | Defined in `src-tauri/src/inference/http.rs` and `crates/cleaner-core/src/cloud_decode.rs`. |
| **P3b2** | Polling Loop & Attempt State Machine | *Pending* | Implements bounded exponential backoff status polling and retry classification. |
| **P3c** | Orchestration & Failover Ladder | *Pending* | Integrates cloud render calls into the active cleaning pipeline with automatic local fallback. |
| **P4** | Persistence, Journaling & User Consent | *Pending* | Persisted job journal, cryptographic nonce consumption, and preflight cost dialogs. |

### Clear Remaining Limitations
1. **No Job Submission**: `POST /mc/v1/jobs` is not implemented in `http.rs`.
2. **No Status Polling Loop**: Automated polling with backoff and timeout handling remains P3b2 scope.
3. **No Automatic Pipeline Failover**: Failover between cloud execution and local inpainting remains P3c scope.
4. **No Live Vendor Account Verification**: Tests use mock HTTP loopback servers in `#[cfg(test)]`; no live cloud credentials or billable GPU workloads have been executed.

---

## Required Orchestrator Module & Dependency Changes

The following changes were intentionally left unedited in existing files (pursuant to ownership boundaries) and must be linked by the orchestrator:

### 1. `crates/cleaner-core/src/lib.rs`
Add the new module declaration:
```rust
pub mod cloud_decode;
```

### 2. `src-tauri/src/inference/mod.rs`
Add the new module declaration and re-exports:
```rust
pub mod http;

pub use http::{
    BoundRuntimeCredential, CloudEndpointTarget, CloudHttpClient, HttpTransportError,
    RuntimeCredential, DEFAULT_CONNECT_TIMEOUT, DEFAULT_DNS_TIMEOUT, DEFAULT_REQUEST_TIMEOUT,
    MAX_CONTROL_JSON_BYTES,
};
```

### 3. Dependencies
No new dependencies are required. The workspace already includes:
- `reqwest = { version = "0.13", features = ["blocking", "rustls"] }` (in `src-tauri/Cargo.toml`)
- `png = "0.18"` (in `crates/cleaner-core/Cargo.toml`)
- `sha2 = "0.10"`, `crc32fast = "1"`, `thiserror = "2"`, `serde = "1"`, `serde_json = "1"`, `zeroize = "1"`, `url = "2"`.
