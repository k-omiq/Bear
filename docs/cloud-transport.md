# Cloud transport and bounded result decode

This document specifies the security controls and invariants of the desktop's HTTP client for the `/mc/v1` wire contract, and of the bounded PNG decoder that turns a cloud result into pixels. The wire contract itself is in [cloud-contract.md](cloud-contract.md).

## Architecture

Two components carry the work:

1. **Hardened HTTP transport (`src-tauri/src/inference/http.rs`).** The client that connects the desktop app to Modal and Beam gateways over the provider-neutral REST contract.
2. **Bounded result PNG decoder (`crates/cleaner-core/src/cloud_decode.rs`).** A memory-safe decompression pipeline. It checks raw bytes against the declared wire metadata, the latent stride, the single-frame RGB8 format, stream completeness and the SHA-256 digest, then produces a [`GeneratedCrop`] for host-side compositing.

All limits below are host safety ceilings, not measured serving limits. Tests use loopback HTTP servers under `#[cfg(test)]`.

---

## Security invariants and hardening controls

### 1. Process-global single-worker DNS and anti-SSRF protection

- **One process-global worker (`resolve_and_validate_host`).** A single background thread takes requests over a bounded `sync_channel(32)` queue. If the DNS worker is stalled or the queue is full, new requests fail at once with `HttpTransportError::DnsResolverBusy`. No extra threads are spawned.
- **Strict public IP validation (`is_public_ip`).**
  - **IPv4:** rejects private, loopback, link-local, carrier-grade NAT (`100.64.0.0/10`), documentation (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`), benchmarking (`198.18.0.0/15`), broadcast and multicast ranges.
  - **IPv6:** allows only global unicast within `2000::/3`, and excludes the special, transition and documentation prefixes (`2001::/23`, `2002::/16`, `3ffe::/16`, `3fff::/16`). Every address outside `2000::/3` is rejected, including `::/128`, `::1/128`, `fc00::/7`, `fe80::/10`, `fec0::/10`, `ff00::/8`, `100::/64`, `64:ff9b::/96` and `::ffff:0:0/96`.
- **Connection IP pinning.** The validated public socket addresses are pinned to the `reqwest` client with `resolve_to_addrs`. This prevents DNS rebinding between the check and the connection.
- **Localhost only in tests.** Loopback injection is gated to `#[cfg(test)]`.

### 2. Transport client hardening

- **HTTPS only.** The client uses the default rustls root certificate validation. Plain `http://` endpoints fail closed.
- **No redirects.** `reqwest::redirect::Policy::none()` is set. Any redirect status (`300..=399`) or a `Location` header is rejected at once with `HttpTransportError::RedirectForbidden`. Modal answers a web request past 150 s with a 303, which is why long analysis and denoise work runs as submit-then-poll jobs ([cloud-provisioning.md](cloud-provisioning.md)).
- **No environment proxies.** `.no_proxy()` ignores `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY`.
- **No automatic retries.** No retry middleware is configured. A network failure goes straight to the caller's error handling.
- **Bounded timeouts.** `DEFAULT_CONNECT_TIMEOUT` is 10 s, `DEFAULT_REQUEST_TIMEOUT` is 30 s and `DEFAULT_DNS_TIMEOUT` is 5 s. Some operations pass their own bound, such as 90 s for analysis and denoise job requests.
- **Static, sanitised errors.** `HttpTransportError` variants are static. They never echo untrusted MIME types, bearer tokens, query parameters, raw URLs or server error bodies.

### 3. Typed runtime credential binding

- **Typed provider variants (`RuntimeCredential`).**
  - `RuntimeCredential::BeamBearer(SecretValue)` sends `Authorization: Bearer <token>`, marked sensitive.
  - `RuntimeCredential::ModalProxy { token_id, token_secret }` sends the official `Modal-Key` and `Modal-Secret` headers, marked sensitive.
- **No invented headers.** The client uses no vendor headers beyond those (no `x-beam-token`, no generic bearer claims).
- **Target binding (`BoundRuntimeCredential`).** A credential is bound to the target provider, profile ID and canonical origin fingerprint. Constructors accept no unbound secret.
- **Zeroisation.** Secret strings are wrapped in [`SecretValue`], which zeroises on drop and formats as `[REDACTED]` in `Debug` and `Display`.

### 4. Control surface

- **Control reads.** `GET /mc/v1/health` (typed health, provider identity must match), `GET /mc/v1/model-info` (advertised recipe, model revision and limits; checks the `native_mask_conditioning = false` honesty), `GET /mc/v1/capabilities` (deployment handshake), `GET /mc/v1/gpu`.
- **Job calls.** `POST /mc/v1/jobs` (multipart: `metadata`, `image`, `hint`), `GET /mc/v1/jobs/{handle}` (status, bound against the caller's [`JobRequestMetadata`]), `GET /mc/v1/jobs/{handle}/result` (raw PNG bytes from the verified endpoint origin, with resumable ranges) and `POST /mc/v1/jobs/{handle}/cancel`. A submission that fails in a way that could hide acceptance is classified so it is never retried blindly ([cloud-contract.md](cloud-contract.md) section 6).
- **GPU control.** `POST /mc/v1/gpu/stop`, including the idle-only release.
- **Analysis and denoise.** Submit-then-poll job routes under `/mc/analysis/v1/` and `/mc/denoise/v1/`.
- **Safe route construction.** Internal enums and `path_segments_mut` append fixed route segments, so an origin cannot be overwritten and a path cannot be traversed. Opaque handles are checked with `validate_ascii_id`. Dot segments, slashes, percent encoding and special characters are rejected.
- **Exact MIME essence.** The MIME type, ignoring parameters, must equal `application/json` for control endpoints and `image/png` for results.
- **Bounded streaming.** Every response body is read through `io::Read::take(max_bytes + 1)`, so streaming and chunked transfers hit a strict memory ceiling. Control JSON is capped at `MAX_CONTROL_JSON_BYTES` (1 MiB). Result reads cap the encoded bytes at the smaller of the advertised `max_png_bytes` and 64 MiB.

### 5. Bounded PNG result decoder

- **Wire preflight.** `cleaner_core::cloud_wire::validate_result_bytes` checks the `IHDR` CRC-32, dimension alignment with the latent stride (16), declared against actual byte length and the SHA-256 payload digest. The expected handle must come from the accepted attempt.
- **Separate encoded and decoded bounds.** The encoded stream is bounded by `ServiceLimits::max_png_bytes` (`png::Limits`), and the decoded pixel count by `ServiceLimits::max_pixels`. Host ceilings apply on top: 64 MiB encoded and 16 Mi pixels (`MAX_RESULT_ENCODED_BYTES`, `MAX_RESULT_PIXELS`).
- **Format check.** Interlacing, multi-frame APNG streams, non-RGB8 colour types (grayscale, RGBA, indexed, CMYK) and bit depths other than 8 are rejected.
- **Stream completeness (`reader.finish()`).** The decoder consumes every chunk through `IEND`, verifying chunk CRCs and rejecting trailing bytes or truncation.
- **Compositor hand-off.** The output is a [`DecodedResultCrop`]. Its `.as_generated_crop()` builds the [`GeneratedCrop`] that [`PreparedRender::composite`] evaluates.

---

## Not verified live

- Modal has run end to end on a real account ([findings.md](findings.md)). Beam has not.
- Vendor authentication headers follow the providers' documentation. The Beam bearer path has not run against a live account.

---

See also: [cloud-contract.md](cloud-contract.md), [cloud-security.md](cloud-security.md), [cloud-journal.md](cloud-journal.md), [cloud-provisioning.md](cloud-provisioning.md).
