//! Hardened cloud HTTP control transport and read-only client for `/mc/v1` wire protocol.
//!
//! Provides an immutable, security-hardened HTTP transport layer for querying health,
//! inspecting advertised model capabilities, checking job execution status, and safely
//! downloading bounded PNG inference results.
//!
//! ## Invariants
//!
//! 1. **Immutable Validated Target:** Constructed exclusively from validated profiles via
//!    [`crate::inference::config::validate_https_endpoint`]. Scheme MUST be `https://`, host
//!    must be a valid domain name, and query strings/fragments/credentials are forbidden.
//! 2. **Process-Global Single-Worker DNS & Anti-SSRF:** Uses a single process-global background
//!    worker with a bounded queue (`sync_channel(32)`). Resolves hostnames with explicit deadline,
//!    rejects ANY non-public/reserved IP (including private, loopback, link-local, CGNAT,
//!    documentation, and non-global IPv6 ranges), and pins validated addresses to the client
//!    connection. If the DNS worker queue is saturated, new requests fail closed immediately.
//! 3. **Strict IPv6 Allowlist:** Allows ONLY global unicast within `2000::/3` while explicitly
//!    excluding all known special and transition prefixes (`2001::/23`, `2002::/16`, `3ffe::/16`, `3fff::/16`).
//! 4. **Transport Hardening & No Retries:** Default rustls TLS certificate verification, redirects
//!    are strictly denied (`Policy::none()`), environment proxies are ignored (`no_proxy()`),
//!    automatic retries are disabled, and connect and request timeouts are strictly bounded.
//! 5. **Typed Runtime Credential Binding:** Credentials must be typed [`RuntimeCredential`] variants
//!    ([`RuntimeCredential::BeamBearer`] for Beam, [`RuntimeCredential::ModalProxy`] with `Modal-Key`
//!    and `Modal-Secret` headers for Modal) immutably bound to the target provider, profile ID,
//!    and canonical origin fingerprint via [`BoundRuntimeCredential`].
//! 6. **Static Sanitized Error Reporting:** Public errors never echo untrusted MIME types,
//!    bearer tokens, internal query parameters, raw URLs, or server error bodies.
//! 7. **Pure Read-Only Control Surface:** Dispatches only `GET /health`, `GET /model-info`,
//!    `GET /jobs/{handle}`, and `GET /jobs/{handle}/result`. Zero `POST` methods exist in this client.
//! 8. **Safe URL Construction & Exact MIME Essence:** Uses `path_segments_mut` to append fixed
//!    internal routes, preventing origin overwrites. Validates exact MIME type essence.
//! 9. **Pre-Return Binding on Result Downloads:** Result bytes are bounded by client-side
//!    [`ServiceLimits::max_png_bytes`] and verified via [`cleaner_core::cloud_wire::validate_result_bytes`]
//!    before returning.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use cleaner_core::cloud_wire::{
    validate_ascii_id, validate_health_response, validate_job_request_metadata,
    validate_model_info_response, validate_response_binding,
    validate_result_bytes, HealthResponse, JobAcceptedResponse, JobCancelResponse,
    JobRequestMetadata, JobStatusResponse, ModelInfoResponse, ResultMetadata, ServiceLimits,
};
use cleaner_core::engines::render::CloudProvider;
use cleaner_core::cloud_job_wire::{self as job_wire, JobEnvelope, JobKind, JobStatus};
use cleaner_core::cloud_analysis_wire::{
    AnalysisBatch, AnalysisBatchResult, AnalysisCapabilities, AnalysisError, AnalysisRequest, AnalysisResult,
    BATCH_MAX_BODY_BYTES, BATCH_MAX_RESPONSE_BYTES, MAX_RESPONSE_BYTES,
};
use cleaner_core::cloud_denoise_wire::{
    DenoiseCapabilities, DenoiseMetadata, DenoiseRejection, DenoiseResult,
    MAX_BODY_BYTES as DENOISE_MAX_BODY_BYTES, MAX_RESPONSE_BYTES as DENOISE_MAX_RESPONSE_BYTES,
};
use base64::Engine;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE};
use reqwest::redirect::Policy;
use reqwest::{StatusCode, Url};
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::inference::config::{
    compute_canonical_endpoint_fingerprint, validate_https_endpoint, validate_profile_id,
};
use crate::inference::gpu_jobs::{self, JobTransport, Polled, Schedule};
use crate::inference::secrets::SecretValue;

/// Default connection timeout for cloud inference endpoints (10 seconds).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default total request timeout for control and status endpoints (30 seconds).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default DNS resolution deadline (5 seconds).
pub const DEFAULT_DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// Result requests in a row that may add no byte before the download fails.
const RESULT_STALLED_TRIES: u32 = 3;

/// The longest one result download may take, every request for a part included.
/// Sized to a 16 MiB result on a link that carries a few kilobytes a second.
const RESULT_DOWNLOAD_BUDGET: Duration = Duration::from_secs(10 * 60);

/// Pause before asking for the rest of a result whose transfer stopped.
const RESULT_RETRY_PAUSE: Duration = Duration::from_millis(if cfg!(test) { 10 } else { 1000 });

/// Maximum allowable payload size for control JSON responses (1 MiB).
pub const MAX_CONTROL_JSON_BYTES: u64 = 1024 * 1024;

/// Maximum size of a GPU status or stop answer: two containers and a price list.
const MAX_GPU_JSON_BYTES: u64 = 16 * 1024;

/// Capacity of the global DNS resolver request queue.
const DNS_QUEUE_CAPACITY: usize = 32;

/// Number of concurrent DNS worker threads (>= 4) sharing the bounded queue.
const DNS_WORKER_COUNT: usize = 4;

/// Errors arising from the hardened cloud HTTP transport.
#[derive(Debug, Error, PartialEq)]
pub enum HttpTransportError {
    #[error("submission was not enqueued: {reason}")]
    NotEnqueued { reason: Box<HttpTransportError>, retryable: bool },
    #[error("cloud render weights need repair")]
    RenderWeightsUnavailable,
    #[error("invalid target URL configuration")]
    InvalidTarget,

    #[error("invalid profile id")]
    InvalidProfileId,

    #[error("target endpoint validation failed")]
    EndpointValidationFailed,

    #[error("runtime credential missing or invalid for target")]
    InvalidCredential,

    #[error("provider_paused: this provider is paused in this version")]
    ProviderPaused,

    #[error("credential provider does not match target provider")]
    CredentialProviderMismatch,

    #[error("DNS resolver queue busy")]
    DnsResolverBusy,

    #[error("DNS resolution failed for target host")]
    DnsResolutionFailed,

    #[error("DNS resolved to forbidden non-public, reserved, or loopback IP address")]
    NonPublicIpRejected,

    #[error("network connection error")]
    ConnectionError,

    #[error("request timeout exceeded")]
    Timeout,

    #[error("HTTP redirect rejected (redirects are strictly forbidden)")]
    RedirectForbidden,

    #[error("unexpected HTTP status code {status}")]
    UnexpectedStatus { status: u16 },

    #[error("response content-type mismatch")]
    ContentTypeMismatch,

    #[error("response body size exceeded maximum limit")]
    BodySizeLimitExceeded,

    #[error("cloud wire validation failed")]
    WireValidation,


    #[error("JSON deserialization error")]
    JsonDeserialization,

    #[error("provider mismatch in response payload")]
    ProviderMismatch,

    #[error("invalid or malicious job handle identifier")]
    InvalidHandle,

    /// A polled tile or page the gateway reports failed, with its typed code
    /// (`inference::gpu_jobs`). The GPU answered, so nothing is in doubt.
    #[error("cloud job failed: {code}")]
    JobFailed { code: String },

    /// A polled job cancelled, and the gateway confirmed it is over.
    #[error("cloud job cancelled")]
    JobCancelled,

    /// A polled job cancelled without the gateway confirming it: its remote
    /// state is unknown.
    #[error("cloud job cancel unconfirmed")]
    JobCancelUnconfirmed,

    /// A polled job that did not end within its schedule's deadline. It was
    /// asked to cancel; its remote state is unknown.
    #[error("cloud job did not finish before its deadline")]
    JobDeadline,
}

impl HttpTransportError {
    fn not_enqueued(self) -> Self {
        Self::NotEnqueued { reason: Box::new(self), retryable: false }
    }

    pub fn definitely_not_enqueued(&self) -> bool {
        matches!(self, Self::NotEnqueued { .. })
    }
}

fn submit_send_error(error: reqwest::Error) -> HttpTransportError {
    // No HTTP request was sent if connection establishment failed. A timeout
    // after connecting or a broken response can still hide remote acceptance.
    if error.is_connect() || error.is_builder() {
        HttpTransportError::ConnectionError.not_enqueued()
    } else {
        HttpTransportError::ConnectionError
    }
}

fn submit_rejection(status: u16, body: &[u8], request: &JobRequestMetadata) -> HttpTransportError {
    let error = HttpTransportError::UnexpectedStatus { status };
    // Gateway/edge authentication refuses the request before routing to jobs.
    if matches!(status, 401 | 403) { return error.not_enqueued(); }
    if let Ok(rejection) = serde_json::from_slice::<cleaner_core::cloud_wire::PreEnqueueRejectionResponse>(body) {
        let bound = rejection.job_id == request.job_id && rejection.attempt_id == request.attempt_id
            && rejection.request_digest == request.request_digest;
        let unparsed = rejection.job_id == "unknown" && rejection.attempt_id == "unknown"
            && rejection.request_digest == "0".repeat(64) && matches!(status, 400 | 413);
        if (bound || unparsed) && cleaner_core::cloud_wire::validate_pre_enqueue_rejection(&rejection).is_ok() {
            let reason = if bound && matches!(rejection.error_code.as_str(), "weights_missing" | "weights_corrupt") {
                HttpTransportError::RenderWeightsUnavailable
            } else { error };
            return HttpTransportError::NotEnqueued { reason: Box::new(reason), retryable: rejection.retryable };
        }
    }
    error
}

#[cfg(debug_assertions)]
fn exact_unsupported_recipe(status: u16, body: &[u8], request: &JobRequestMetadata) -> bool {
    if status != 400 { return false; }
    let Ok(rejection) = serde_json::from_slice::<cleaner_core::cloud_wire::PreEnqueueRejectionResponse>(body) else { return false; };
    rejection.job_id == request.job_id && rejection.attempt_id == request.attempt_id
        && rejection.request_digest == request.request_digest && !rejection.enqueued
        && !rejection.retryable && rejection.error_code == "unsupported_recipe"
        && cleaner_core::cloud_wire::validate_pre_enqueue_rejection(&rejection).is_ok()
}

#[cfg(debug_assertions)]
#[derive(Debug)]
pub struct ProbeSubmitError {
    pub error: HttpTransportError,
    pub exact_unsupported_recipe: bool,
}

fn analysis_rejection_error(status: u16, body: &[u8], expected_digest: &str) -> HttpTransportError {
    let Ok(rejection) = serde_json::from_slice::<AnalysisError>(body) else {
        return HttpTransportError::WireValidation;
    };
    if rejection.validate().is_err()
        || (rejection.request_digest.as_deref() != Some(expected_digest)
            && !(rejection.request_digest.is_none() && matches!(status, 401 | 413 | 503)))
    {
        return HttpTransportError::WireValidation;
    }
    HttpTransportError::UnexpectedStatus { status }
}

/// A denoise error answer, held to the same binding as an analysis one: it
/// must name this request's digest, or none where the gateway refuses before
/// reading the request.
fn denoise_rejection_error(status: u16, body: &[u8], expected_digest: &str) -> HttpTransportError {
    let Ok(rejection) = serde_json::from_slice::<DenoiseRejection>(body) else {
        return HttpTransportError::WireValidation;
    };
    if rejection.validate().is_err()
        || (rejection.request_digest.as_deref() != Some(expected_digest)
            && !(rejection.request_digest.is_none() && matches!(status, 401 | 413 | 503)))
    {
        return HttpTransportError::WireValidation;
    }
    HttpTransportError::UnexpectedStatus { status }
}

/// Room for a job answer's envelope around the synchronous answer it carries.
const JOB_ENVELOPE_BYTES: u64 = 4096;

/// A fresh attempt id for one tile or page: its job handle for as long as
/// this call submits and polls it (`cloud_job_wire::job_handle`).
fn attempt_id() -> Result<String, HttpTransportError> {
    let id = crate::inference::analysis::random_id().map_err(|_| HttpTransportError::WireValidation)?;
    debug_assert!(job_wire::valid_attempt_id(&id));
    Ok(id)
}

fn run_polled<R: DeserializeOwned>(
    job: &HttpGpuJob<'_, R>,
    schedule: &Schedule,
    cancel: &AtomicBool,
) -> Result<R, HttpTransportError> {
    gpu_jobs::run_job(job, schedule, cancel, &gpu_jobs::SystemClock::new())
}

/// One tile or page on the job routes (`cleaner_core::cloud_job_wire`).
struct HttpGpuJob<'a, R> {
    client: &'a CloudHttpClient,
    kind: JobKind,
    /// The synchronous body plus the attempt id, sent again unchanged by a
    /// repeated submit so it names the same job.
    body: Vec<u8>,
    request_digest: &'a str,
    handle: String,
    max_answer_bytes: u64,
    /// The synchronous route's own check of a completed answer.
    check: &'a dyn Fn(&R) -> Result<(), HttpTransportError>,
    /// The synchronous route's reading of a refusal, bound to the digest.
    reject: fn(u16, &[u8], &str) -> HttpTransportError,
}

impl<R: DeserializeOwned> HttpGpuJob<'_, R> {
    fn envelope<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<JobEnvelope<T>, HttpTransportError> {
        let envelope: JobEnvelope<T> = serde_json::from_slice(bytes).map_err(|_| HttpTransportError::JsonDeserialization)?;
        envelope.validate(self.kind, self.request_digest, &self.handle).map_err(|_| HttpTransportError::WireValidation)?;
        Ok(envelope)
    }
}

impl<R: DeserializeOwned> JobTransport for HttpGpuJob<'_, R> {
    type Output = R;

    fn submit(&self, timeout: Duration) -> Result<(), HttpTransportError> {
        let req = self.client.client.post(self.client.job_url(self.kind, None, false))
            .header(CONTENT_TYPE, "application/json").body(self.body.clone());
        let (status, bytes) = self.client.job_request(req, timeout, MAX_CONTROL_JSON_BYTES)?;
        if status.is_server_error() && bytes.is_empty() {
            // Not the gateway's answer (it refuses in JSON): the submit may or may
            // not have reached it, as when no answer came at all.
            return Err(HttpTransportError::ConnectionError);
        }
        if !status.is_success() {
            return Err((self.reject)(status.as_u16(), &bytes, self.request_digest));
        }
        if status != reqwest::StatusCode::ACCEPTED {
            return Err(HttpTransportError::UnexpectedStatus { status: status.as_u16() });
        }
        // The gateway's handle must be the one derived here: the idempotence of a
        // repeated submit and the cancel of an unanswered one both rest on it.
        self.envelope::<serde_json::Value>(&bytes).map(|_| ())
    }

    fn status(&self, timeout: Duration) -> Result<Polled<R>, HttpTransportError> {
        let req = self.client.client.get(self.client.job_url(self.kind, Some(&self.handle), false));
        let (status, bytes) = self.client.job_request(req, timeout, self.max_answer_bytes)?;
        if !status.is_success() {
            return Err(HttpTransportError::UnexpectedStatus { status: status.as_u16() });
        }
        let envelope = self.envelope::<R>(&bytes)?;
        Ok(match envelope.status {
            JobStatus::Pending | JobStatus::Running => Polled::Waiting,
            JobStatus::Completed => {
                let result = envelope.result.ok_or(HttpTransportError::WireValidation)?;
                (self.check)(&result)?;
                Polled::Completed(result)
            }
            JobStatus::Failed => Polled::Failed {
                code: envelope.error.map(|error| error.error_code).unwrap_or_default(),
            },
            JobStatus::Cancelled => Polled::Cancelled,
        })
    }

    fn cancel(&self, timeout: Duration) -> Result<bool, HttpTransportError> {
        let req = self.client.client.post(self.client.job_url(self.kind, Some(&self.handle), true));
        let (status, bytes) = self.client.job_request(req, timeout, MAX_CONTROL_JSON_BYTES)?;
        if !status.is_success() {
            return Err(HttpTransportError::UnexpectedStatus { status: status.as_u16() });
        }
        Ok(self.envelope::<serde_json::Value>(&bytes)?.status.is_terminal())
    }
}

/// Validate whether an IP address is a globally reachable public unicast address.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();

    // 0.0.0.0/8: "This network" (RFC 1122)
    if octets[0] == 0 {
        return false;
    }

    // 10.0.0.0/8: Private-Use (RFC 1918)
    if octets[0] == 10 {
        return false;
    }

    // 100.64.0.0/10: Shared Address Space / Carrier-Grade NAT (RFC 6598)
    if octets[0] == 100 && (octets[1] & 0xC0) == 64 {
        return false;
    }

    // 127.0.0.0/8: Loopback (RFC 1122)
    if octets[0] == 127 {
        return false;
    }

    // 169.254.0.0/16: Link Local (RFC 3927)
    if octets[0] == 169 && octets[1] == 254 {
        return false;
    }

    // 172.16.0.0/12: Private-Use (RFC 1918)
    if octets[0] == 172 && (octets[1] >= 16 && octets[1] <= 31) {
        return false;
    }

    // 192.0.0.0/24: IETF Protocol Assignments (RFC 6890)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        return false;
    }

    // 192.0.2.0/24: TEST-NET-1 (RFC 5737)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 2 {
        return false;
    }

    // 192.88.99.0/24: 6to4 Relay Anycast (RFC 3068 / RFC 7526)
    if octets[0] == 192 && octets[1] == 88 && octets[2] == 99 {
        return false;
    }

    // 192.168.0.0/16: Private-Use (RFC 1918)
    if octets[0] == 192 && octets[1] == 168 {
        return false;
    }

    // 198.18.0.0/15: Network Interconnect Benchmarking (RFC 2544)
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        return false;
    }

    // 198.51.100.0/24: TEST-NET-2 (RFC 5737)
    if octets[0] == 198 && octets[1] == 51 && octets[2] == 100 {
        return false;
    }

    // 203.0.113.0/24: TEST-NET-3 (RFC 5737)
    if octets[0] == 203 && octets[1] == 0 && octets[2] == 113 {
        return false;
    }

    // 224.0.0.0/4: Multicast (RFC 5771)
    if octets[0] >= 224 && octets[0] <= 239 {
        return false;
    }

    // 240.0.0.0/4: Reserved for Future Use / Broadcast (RFC 1112)
    if octets[0] >= 240 {
        return false;
    }

    true
}

/// Conservative IPv6 allowlist: permits ONLY global unicast in `2000::/3`, excluding special prefixes.
fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return false; // Documentation prefix, outside 2001::/23.
    }

    // Must be in 2000::/3 (first 3 bits = 001)
    if (segments[0] & 0xe000) != 0x2000 {
        return false;
    }

    // Exclude 2001::/23 (IETF Protocol Assignments, Teredo, Documentation, Benchmarking, ORCHID)
    if segments[0] == 0x2001 && (segments[1] & 0xfe00) == 0 {
        return false;
    }

    // Exclude 2002::/16 (6to4)
    if segments[0] == 0x2002 {
        return false;
    }

    // Exclude 3ffe::/16 and 3fff::/16 (6bone testbed and reserved)
    if segments[0] == 0x3ffe || segments[0] == 0x3fff {
        return false;
    }

    true
}

struct DnsJob {
    host: String,
    port: u16,
    reply: SyncSender<Result<Vec<SocketAddr>, ()>>,
}

static DNS_CHANNEL: OnceLock<SyncSender<DnsJob>> = OnceLock::new();

fn get_dns_sender() -> &'static SyncSender<DnsJob> {
    DNS_CHANNEL.get_or_init(|| {
        let (tx, rx): (SyncSender<DnsJob>, Receiver<DnsJob>) = sync_channel(DNS_QUEUE_CAPACITY);
        let rx_shared = std::sync::Arc::new(std::sync::Mutex::new(rx));
        for i in 0..DNS_WORKER_COUNT {
            let rx_clone = std::sync::Arc::clone(&rx_shared);
            thread::Builder::new()
                .name(format!("cloud-dns-worker-{i}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let rx_lock = match rx_clone.lock() {
                                Ok(guard) => guard,
                                Err(poisoned) => poisoned.into_inner(),
                            };
                            match rx_lock.recv() {
                                Ok(j) => j,
                                Err(_) => break,
                            }
                        };
                        let addrs = (job.host.as_str(), job.port)
                            .to_socket_addrs()
                            .map(|iter| iter.collect::<Vec<_>>())
                            .map_err(|_| ());
                        let _ = job.reply.send(addrs);
                    }
                })
                .expect("failed to spawn cloud DNS resolver worker");
        }
        tx
    })
}

/// Resolve host addresses with bounded queue dispatch, deadline, and strict public IP validation.
pub fn resolve_and_validate_host(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<Vec<SocketAddr>, HttpTransportError> {
    let sender = get_dns_sender();
    let (reply_tx, reply_rx) = sync_channel(1);

    let job = DnsJob {
        host: host.to_string(),
        port,
        reply: reply_tx,
    };

    // try_send fails immediately if the single worker queue is full
    sender
        .try_send(job)
        .map_err(|_| HttpTransportError::DnsResolverBusy)?;

    let addrs_res = reply_rx
        .recv_timeout(timeout)
        .map_err(|_| HttpTransportError::Timeout)?;

    let addrs = addrs_res.map_err(|_| HttpTransportError::DnsResolutionFailed)?;

    if addrs.is_empty() {
        return Err(HttpTransportError::DnsResolutionFailed);
    }

    for addr in &addrs {
        if !is_public_ip(addr.ip()) {
            return Err(HttpTransportError::NonPublicIpRejected);
        }
    }

    Ok(addrs)
}

/// Distinct vendor credential variants for cloud inference providers.
#[derive(Clone)]
pub enum RuntimeCredential {
    BeamBearer(SecretValue),
    ModalProxy {
        token_id: String,
        token_secret: SecretValue,
    },
}

/// An immutable runtime credential bound strictly to a target provider, profile ID, and origin fingerprint.
#[derive(Clone)]
pub struct BoundRuntimeCredential {
    provider: CloudProvider,
    profile_id: String,
    canonical_origin_fingerprint: String,
    credential: RuntimeCredential,
}

impl BoundRuntimeCredential {
    /// Bind a [`RuntimeCredential`] to a validated [`CloudEndpointTarget`].
    pub fn new(
        target: &CloudEndpointTarget,
        credential: RuntimeCredential,
    ) -> Result<Self, HttpTransportError> {
        match (&credential, target.provider()) {
            (RuntimeCredential::BeamBearer(sec), CloudProvider::Beam) => {
                if sec.is_empty() {
                    return Err(HttpTransportError::InvalidCredential);
                }
            }
            (
                RuntimeCredential::ModalProxy {
                    token_id,
                    token_secret,
                },
                CloudProvider::Modal,
            ) => {
                if token_id.trim().is_empty() || token_secret.is_empty() {
                    return Err(HttpTransportError::InvalidCredential);
                }
            }
            _ => {
                return Err(HttpTransportError::CredentialProviderMismatch);
            }
        }

        Ok(Self {
            provider: target.provider(),
            profile_id: target.profile_id().to_string(),
            canonical_origin_fingerprint: target.canonical_origin_fingerprint().to_string(),
            credential,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixedControlRoute {
    Health,
    ModelInfo,
    Gpu,
    GpuStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobSubResource {
    Status,
    Result,
    Cancel,
}

/// An immutable, validated cloud endpoint target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudEndpointTarget {
    provider: CloudProvider,
    profile_id: String,
    base_url: Url,
    canonical_origin: String,
    canonical_origin_fingerprint: String,
    canonical_endpoint_fingerprint: String,
}

impl CloudEndpointTarget {
    /// Construct and validate an immutable cloud endpoint target.
    pub fn new(
        provider: CloudProvider,
        profile_id: &str,
        raw_endpoint_url: &str,
    ) -> Result<Self, HttpTransportError> {
        validate_profile_id(profile_id)
            .map_err(|_| HttpTransportError::InvalidProfileId)?;

        let (canonical_origin, canonical_origin_fingerprint) =
            validate_https_endpoint(raw_endpoint_url)
                .map_err(|_| HttpTransportError::EndpointValidationFailed)?;

        let canonical_endpoint_fingerprint =
            compute_canonical_endpoint_fingerprint(raw_endpoint_url)
                .map_err(|_| HttpTransportError::EndpointValidationFailed)?;

        let mut parsed = Url::parse(raw_endpoint_url)
            .map_err(|_| HttpTransportError::EndpointValidationFailed)?;

        if !parsed.path().ends_with('/') {
            let mut new_path = parsed.path().to_string();
            new_path.push('/');
            parsed.set_path(&new_path);
        }

        Ok(Self {
            provider,
            profile_id: profile_id.to_string(),
            base_url: parsed,
            canonical_origin,
            canonical_origin_fingerprint,
            canonical_endpoint_fingerprint,
        })
    }

    #[cfg(test)]
    /// Internal constructor for unit testing with local mock HTTP servers.
    pub fn new_test_target(
        provider: CloudProvider,
        profile_id: &str,
        raw_endpoint_url: &str,
    ) -> Result<Self, HttpTransportError> {
        let mut parsed = Url::parse(raw_endpoint_url)
            .map_err(|_| HttpTransportError::EndpointValidationFailed)?;

        if !parsed.path().ends_with('/') {
            let mut new_path = parsed.path().to_string();
            new_path.push('/');
            parsed.set_path(&new_path);
        }

        let mock_https = match provider {
            CloudProvider::Beam => "https://api.beam.cloud/mc/v1",
            CloudProvider::Modal => "https://modal.run/mc/v1",
        };
        let (origin, origin_fp) = validate_https_endpoint(mock_https)
            .map_err(|_| HttpTransportError::EndpointValidationFailed)?;
        let endpoint_fp = compute_canonical_endpoint_fingerprint(mock_https)
            .map_err(|_| HttpTransportError::EndpointValidationFailed)?;

        Ok(Self {
            provider,
            profile_id: profile_id.to_string(),
            base_url: parsed,
            canonical_origin: origin,
            canonical_origin_fingerprint: origin_fp,
            canonical_endpoint_fingerprint: endpoint_fp,
        })
    }

    pub fn provider(&self) -> CloudProvider {
        self.provider
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn canonical_origin(&self) -> &str {
        &self.canonical_origin
    }

    pub fn canonical_origin_fingerprint(&self) -> &str {
        &self.canonical_origin_fingerprint
    }

    pub fn canonical_endpoint_fingerprint(&self) -> &str {
        &self.canonical_endpoint_fingerprint
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    fn build_fixed_route(&self, route: FixedControlRoute) -> Result<Url, HttpTransportError> {
        let mut url = self.base_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| HttpTransportError::InvalidTarget)?;
            segments.pop_if_empty();
            match route {
                FixedControlRoute::Health => {
                    segments.push("health");
                }
                FixedControlRoute::ModelInfo => {
                    segments.push("model-info");
                }
                FixedControlRoute::Gpu => {
                    segments.push("gpu");
                }
                FixedControlRoute::GpuStop => {
                    segments.push("gpu");
                    segments.push("stop");
                }
            }
        }
        Ok(url)
    }

    fn build_jobs_route(&self) -> Result<Url, HttpTransportError> {
        let mut url = self.base_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| HttpTransportError::InvalidTarget)?;
            segments.pop_if_empty();
            segments.push("jobs");
        }
        Ok(url)
    }

    fn build_job_route(
        &self,
        handle: &str,
        sub: JobSubResource,
    ) -> Result<Url, HttpTransportError> {
        validate_ascii_id(handle, "handle", 1, 128)
            .map_err(|_| HttpTransportError::WireValidation)?;

        if handle == "."
            || handle == ".."
            || handle.contains('/')
            || handle.contains('\\')
            || handle.contains('%')
            || handle.contains('?')
            || handle.contains('#')
            || handle.contains(':')
        {
            return Err(HttpTransportError::InvalidHandle);
        }

        let mut url = self.base_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| HttpTransportError::InvalidTarget)?;
            segments.pop_if_empty();
            segments.push("jobs");
            segments.push(handle);
            match sub {
                JobSubResource::Status => {}
                JobSubResource::Result => {
                    segments.push("result");
                }
                JobSubResource::Cancel => {
                    segments.push("cancel");
                }
            }
        }
        Ok(url)
    }
}

fn build_multipart_body(
    boundary: &str,
    metadata_json: &[u8],
    image_png: &[u8],
    hint_png: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(metadata_json.len() + image_png.len() + hint_png.len() + 512);

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"metadata\"\r\n");
    body.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
    body.extend_from_slice(metadata_json);
    body.extend_from_slice(b"\r\n");

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"image\"; filename=\"image.png\"\r\n");
    body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
    body.extend_from_slice(image_png);
    body.extend_from_slice(b"\r\n");

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"hint\"; filename=\"hint.png\"\r\n");
    body.extend_from_slice(b"Content-Type: image/png\r\n\r\n");
    body.extend_from_slice(hint_png);
    body.extend_from_slice(b"\r\n");

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn modal_attempt_handle(job_id: &str, attempt_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{job_id}\0{attempt_id}").as_bytes());
    format!("handle-modal-{}", digest[..16].iter()
        .map(|byte| format!("{byte:02x}")).collect::<String>())
}

pub const REQUIRED_GATEWAY_OPERATIONS: &[&str] = &[
    "jobs.submit", "jobs.status", "jobs.result", "jobs.cancel",
];
pub const MODAL_GPU_OPERATIONS: &[&str] = &["gpu.status", "gpu.stop", "gpu.release_idle"];

fn required_gateway_operations(provider: CloudProvider) -> impl Iterator<Item = &'static str> {
    REQUIRED_GATEWAY_OPERATIONS.iter().copied().chain(
        if provider == CloudProvider::Modal { MODAL_GPU_OPERATIONS } else { &[] }.iter().copied())
}

/// Hardened HTTP transport client for cloud inference read-only control and execution endpoints.
#[derive(Clone)]
pub struct CloudHttpClient {
    target: CloudEndpointTarget,
    client: Client,
    credential: BoundRuntimeCredential,
    /// The gateway's `/capabilities` operations, read once per client by
    /// [`Self::serves_gpu_jobs`]. Empty for a gateway without the route.
    operations: Arc<OnceLock<Vec<String>>>,
}

impl CloudHttpClient {
    fn analysis_url(&self, route: &str) -> Url {
        let mut url = self.target.base_url.clone();
        url.set_path(&format!("/mc/analysis/v1/{route}"));
        url
    }

    pub fn get_analysis_capabilities(&self) -> Result<AnalysisCapabilities, HttpTransportError> {
        let capabilities: AnalysisCapabilities = self.send_get_json(
            self.analysis_url("capabilities"), MAX_CONTROL_JSON_BYTES,
        )?;
        capabilities.validate().map_err(|_| HttpTransportError::WireValidation)?;
        Ok(capabilities)
    }

    /// One tile on the synchronous route (`POST /mc/analysis/v1/analyze`), for
    /// a gateway without analysis jobs ([`Self::analyze_tile`] chooses). On Modal
    /// a tile past 150 s gets a 303 here, refused as [`HttpTransportError::RedirectForbidden`].
    pub fn submit_analysis_tile(
        &self,
        request: &AnalysisRequest,
        tile_png: &[u8],
    ) -> Result<AnalysisResult, HttpTransportError> {
        request.validate(tile_png).map_err(|_| HttpTransportError::WireValidation)?;
        let envelope = serde_json::json!({
            "metadata": request,
            "tile_png_b64": base64::engine::general_purpose::STANDARD.encode(tile_png),
        });
        let body = serde_json::to_vec(&envelope).map_err(|_| HttpTransportError::WireValidation)?;
        if body.len() > 8_000_000 { return Err(HttpTransportError::BodySizeLimitExceeded); }
        let req = self.client.post(self.analysis_url("analyze"))
            // A scale-to-zero GPU may need a cold start before its first ONNX
            // tile. Keep the regular control/job calls at their 30 s bound.
            .timeout(Duration::from_secs(450))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body);
        let response = self.attach_auth(req)?.send().map_err(|_| HttpTransportError::ConnectionError)?;
        let status = response.status();
        if status.is_redirection() || response.headers().contains_key(LOCATION) {
            return Err(HttpTransportError::RedirectForbidden);
        }
        Self::verify_content_type(&response, "application/json")?;
        let bytes = Self::read_bounded_body(response, MAX_RESPONSE_BYTES as u64)?;
        if !status.is_success() {
            return Err(analysis_rejection_error(status.as_u16(), &bytes, &request.request_digest));
        }
        let result: AnalysisResult = serde_json::from_slice(&bytes)
            .map_err(|_| HttpTransportError::JsonDeserialization)?;
        result.validate(request).map_err(|_| HttpTransportError::WireValidation)?;
        Ok(result)
    }

    fn denoise_url(&self, route: &str) -> Url {
        let mut url = self.target.base_url.clone();
        url.set_path(&format!("/mc/denoise/v1/{route}"));
        url
    }

    /// Which denoise engines the deployment has models for (`GET /mc/denoise/v1/capabilities`).
    pub fn get_denoise_capabilities(&self) -> Result<DenoiseCapabilities, HttpTransportError> {
        let capabilities: DenoiseCapabilities = self.send_get_json(
            self.denoise_url("capabilities"), MAX_CONTROL_JSON_BYTES,
        )?;
        capabilities.validate().map_err(|_| HttpTransportError::WireValidation)?;
        Ok(capabilities)
    }

    /// One whole page through a recipe (`POST /mc/denoise/v1/page`). The answer
    /// is checked for its version and digest echo here; its PNG is checked by
    /// the caller, which knows the page's size ([`DenoiseResult::validate`]).
    /// The synchronous route, for a gateway without denoise jobs
    /// ([`Self::denoise_page`] chooses); on Modal a page past 150 s gets a 303.
    pub fn submit_denoise_page(
        &self,
        metadata: &DenoiseMetadata,
        page_png: &[u8],
    ) -> Result<DenoiseResult, HttpTransportError> {
        metadata.validate(page_png).map_err(|_| HttpTransportError::WireValidation)?;
        let envelope = serde_json::json!({
            "metadata": metadata,
            "page_png_b64": base64::engine::general_purpose::STANDARD.encode(page_png),
        });
        let body = serde_json::to_vec(&envelope).map_err(|_| HttpTransportError::WireValidation)?;
        if body.len() > DENOISE_MAX_BODY_BYTES { return Err(HttpTransportError::BodySizeLimitExceeded); }
        let req = self.client.post(self.denoise_url("page"))
            // The gateway waits up to 600 s for a denoise page: a cold start
            // plus a 4x sharpen of a large page.
            .timeout(Duration::from_secs(600))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body);
        let response = self.attach_auth(req)?.send().map_err(|_| HttpTransportError::ConnectionError)?;
        let status = response.status();
        if status.is_redirection() || response.headers().contains_key(LOCATION) {
            return Err(HttpTransportError::RedirectForbidden);
        }
        Self::verify_content_type(&response, "application/json")?;
        let bytes = Self::read_bounded_body(response, DENOISE_MAX_RESPONSE_BYTES as u64)?;
        if !status.is_success() {
            return Err(denoise_rejection_error(status.as_u16(), &bytes, &metadata.request_digest));
        }
        let result: DenoiseResult = serde_json::from_slice(&bytes)
            .map_err(|_| HttpTransportError::JsonDeserialization)?;
        if result.protocol_version != metadata.protocol_version || result.request_digest != metadata.request_digest {
            return Err(HttpTransportError::WireValidation);
        }
        Ok(result)
    }

    /// Whether the gateway serves `kind` as submit-then-poll jobs: it lists
    /// [`JobKind::operation`] in `GET /mc/v1/capabilities`. Read once per
    /// client. A gateway without the route (404/501) serves none; any other
    /// failure is returned and not remembered.
    pub fn serves_gpu_jobs(&self, kind: JobKind) -> Result<bool, HttpTransportError> {
        let operations = match self.operations.get() {
            Some(operations) => operations,
            None => {
                let mut url = self.target.base_url.clone();
                url.set_path(&format!("{}/capabilities", url.path().trim_end_matches('/')));
                let listed = match self.send_get_json::<serde_json::Value>(url, MAX_CONTROL_JSON_BYTES) {
                    Ok(response) => response["operations"].as_array().map(|ops| ops.iter()
                        .filter_map(|op| op.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
                    Err(HttpTransportError::UnexpectedStatus { status: 404 | 501 }) => Vec::new(),
                    Err(error) => return Err(error),
                };
                self.operations.get_or_init(|| listed)
            }
        };
        Ok(operations.iter().any(|op| op == kind.operation()))
    }

    /// One analysis tile. Submitted and polled ([`gpu_jobs::run_job`]) when the
    /// gateway serves analysis jobs, so no request waits on the GPU and `cancel`
    /// stops the poll and cancels the tile remotely. An older gateway gets the
    /// synchronous [`Self::submit_analysis_tile`], which `cancel` cannot interrupt.
    pub fn analyze_tile(
        &self,
        request: &AnalysisRequest,
        tile_png: &[u8],
        cancel: &AtomicBool,
    ) -> Result<AnalysisResult, HttpTransportError> {
        if !self.serves_gpu_jobs(JobKind::Analysis)? {
            return self.submit_analysis_tile(request, tile_png);
        }
        request.validate(tile_png).map_err(|_| HttpTransportError::WireValidation)?;
        let attempt_id = attempt_id()?;
        let body = serde_json::to_vec(&serde_json::json!({
            "metadata": request,
            "tile_png_b64": base64::engine::general_purpose::STANDARD.encode(tile_png),
            "attempt_id": attempt_id,
        })).map_err(|_| HttpTransportError::WireValidation)?;
        if body.len() > 8_000_000 { return Err(HttpTransportError::BodySizeLimitExceeded); }
        let check = |result: &AnalysisResult| result.validate(request).map_err(|_| HttpTransportError::WireValidation);
        let job = HttpGpuJob {
            client: self, kind: JobKind::Analysis, body,
            request_digest: &request.request_digest,
            handle: job_wire::job_handle(JobKind::Analysis, &request.request_digest, &attempt_id),
            max_answer_bytes: MAX_RESPONSE_BYTES as u64 + JOB_ENVELOPE_BYTES,
            check: &check, reject: analysis_rejection_error,
        };
        run_polled(&job, &gpu_jobs::ANALYSIS_SCHEDULE, cancel)
    }

    /// One batch of a page's analysis requests (`POST /mc/analysis/v1/batches`),
    /// submitted and polled as a tile is. `tiles` are the batch's distinct PNGs,
    /// each sent once. Only for a gateway that lists the operation
    /// ([`Self::serves_gpu_jobs`] with [`JobKind::AnalysisBatch`]).
    pub fn submit_analysis_batch(
        &self,
        batch: &AnalysisBatch,
        tiles: &[&[u8]],
        cancel: &AtomicBool,
    ) -> Result<AnalysisBatchResult, HttpTransportError> {
        let attempt_id = attempt_id()?;
        let body = serde_json::to_vec(&serde_json::json!({
            "metadata": batch,
            "tiles_png_b64": tiles.iter().map(|png| base64::engine::general_purpose::STANDARD.encode(png))
                .collect::<Vec<_>>(),
            "attempt_id": attempt_id,
        })).map_err(|_| HttpTransportError::WireValidation)?;
        if body.len() > BATCH_MAX_BODY_BYTES { return Err(HttpTransportError::BodySizeLimitExceeded); }
        let check = |result: &AnalysisBatchResult| result.validate(batch).map_err(|_| HttpTransportError::WireValidation);
        let job = HttpGpuJob {
            client: self, kind: JobKind::AnalysisBatch, body,
            request_digest: &batch.request_digest,
            handle: job_wire::job_handle(JobKind::AnalysisBatch, &batch.request_digest, &attempt_id),
            max_answer_bytes: BATCH_MAX_RESPONSE_BYTES as u64 + JOB_ENVELOPE_BYTES,
            check: &check, reject: analysis_rejection_error,
        };
        run_polled(&job, &gpu_jobs::ANALYSIS_BATCH_SCHEDULE, cancel)
    }

    /// One denoise page, as [`Self::analyze_tile`] sends a tile: submitted and
    /// polled when the gateway serves denoise jobs, else the synchronous
    /// [`Self::submit_denoise_page`].
    pub fn denoise_page(
        &self,
        metadata: &DenoiseMetadata,
        page_png: &[u8],
        cancel: &AtomicBool,
    ) -> Result<DenoiseResult, HttpTransportError> {
        if !self.serves_gpu_jobs(JobKind::Denoise)? {
            return self.submit_denoise_page(metadata, page_png);
        }
        metadata.validate(page_png).map_err(|_| HttpTransportError::WireValidation)?;
        let attempt_id = attempt_id()?;
        let body = serde_json::to_vec(&serde_json::json!({
            "metadata": metadata,
            "page_png_b64": base64::engine::general_purpose::STANDARD.encode(page_png),
            "attempt_id": attempt_id,
        })).map_err(|_| HttpTransportError::WireValidation)?;
        if body.len() > DENOISE_MAX_BODY_BYTES { return Err(HttpTransportError::BodySizeLimitExceeded); }
        let check = |result: &DenoiseResult| {
            if result.protocol_version != metadata.protocol_version || result.request_digest != metadata.request_digest {
                return Err(HttpTransportError::WireValidation);
            }
            Ok(())
        };
        let job = HttpGpuJob {
            client: self, kind: JobKind::Denoise, body,
            request_digest: &metadata.request_digest,
            handle: job_wire::job_handle(JobKind::Denoise, &metadata.request_digest, &attempt_id),
            max_answer_bytes: DENOISE_MAX_RESPONSE_BYTES as u64 + JOB_ENVELOPE_BYTES,
            check: &check, reject: denoise_rejection_error,
        };
        run_polled(&job, &gpu_jobs::DENOISE_SCHEDULE, cancel)
    }

    fn job_url(&self, kind: JobKind, handle: Option<&str>, cancel: bool) -> Url {
        let mut url = self.target.base_url.clone();
        let mut path = kind.route().to_owned();
        if let Some(handle) = handle {
            path.push('/');
            path.push_str(handle);
            if cancel { path.push_str("/cancel"); }
        }
        url.set_path(&path);
        url
    }

    /// Send one job request under `timeout` and read its bounded JSON answer.
    /// A redirect is refused as everywhere else in this client.
    fn job_request(&self, req: RequestBuilder, timeout: Duration, max_bytes: u64)
        -> Result<(reqwest::StatusCode, Vec<u8>), HttpTransportError> {
        let req = req.timeout(timeout).header(ACCEPT, "application/json");
        let response = self.attach_auth(req)?.send().map_err(|error| if error.is_timeout() {
            HttpTransportError::Timeout
        } else {
            HttpTransportError::ConnectionError
        })?;
        let status = response.status();
        if status.is_redirection() || response.headers().contains_key(LOCATION) {
            return Err(HttpTransportError::RedirectForbidden);
        }
        if !status.is_success() {
            // The gateway refuses in JSON; anything else (an edge error page) is
            // only its status, with an empty body.
            if Self::verify_content_type(&response, "application/json").is_err() {
                return Ok((status, Vec::new()));
            }
            return Ok((status, Self::read_bounded_body(response, MAX_CONTROL_JSON_BYTES)?));
        }
        Self::verify_content_type(&response, "application/json")?;
        let bytes = Self::read_bounded_body(response, max_bytes)?;
        Ok((status, bytes))
    }

    /// Create a new hardened cloud HTTP client with a bound runtime credential.
    pub fn new(
        target: CloudEndpointTarget,
        credential: BoundRuntimeCredential,
    ) -> Result<Self, HttpTransportError> {
        if crate::inference::provider_paused(target.provider) {
            return Err(HttpTransportError::ProviderPaused);
        }
        if credential.provider != target.provider
            || credential.profile_id != target.profile_id
            || credential.canonical_origin_fingerprint != target.canonical_origin_fingerprint
        {
            return Err(HttpTransportError::InvalidCredential);
        }

        let host = target
            .base_url
            .host_str()
            .ok_or(HttpTransportError::InvalidTarget)?;
        let port = target
            .base_url
            .port_or_known_default()
            .unwrap_or(443);

        let pinned_addrs = resolve_and_validate_host(host, port, DEFAULT_DNS_TIMEOUT)?;

        let mut builder = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .https_only(true);

        builder = builder.resolve_to_addrs(host, &pinned_addrs);

        let client = builder
            .build()
            .map_err(|_| HttpTransportError::ConnectionError)?;

        Ok(Self {
            target,
            client,
            credential,
            operations: Arc::default(),
        })
    }

    #[cfg(test)]
    /// Create test client with injected reqwest client (for mock loopback testing).
    pub fn new_test_client(
        target: CloudEndpointTarget,
        credential: BoundRuntimeCredential,
        client: Client,
    ) -> Self {
        Self {
            target,
            client,
            credential,
            operations: Arc::default(),
        }
    }

    pub fn target(&self) -> &CloudEndpointTarget {
        &self.target
    }

    fn read_bounded_body(
        mut response: Response,
        max_bytes: u64,
    ) -> Result<Vec<u8>, HttpTransportError> {
        let mut reader = (&mut response).take(max_bytes.saturating_add(1));
        let mut buf = Vec::new();
        reader
            .read_to_end(&mut buf)
            .map_err(|_| HttpTransportError::ConnectionError)?;

        if buf.len() as u64 > max_bytes {
            return Err(HttpTransportError::BodySizeLimitExceeded);
        }

        Ok(buf)
    }

    fn verify_content_type(
        response: &Response,
        expected_essence: &str,
    ) -> Result<(), HttpTransportError> {
        let raw_ct = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let essence = raw_ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        if essence != expected_essence {
            return Err(HttpTransportError::ContentTypeMismatch);
        }

        Ok(())
    }

    fn attach_auth(&self, mut builder: RequestBuilder) -> Result<RequestBuilder, HttpTransportError> {
        match &self.credential.credential {
            RuntimeCredential::BeamBearer(secret) => {
                let token_str = secret
                    .expose_str()
                    .map_err(|_| HttpTransportError::InvalidCredential)?;
                let mut val = HeaderValue::from_str(&format!("Bearer {token_str}"))
                    .map_err(|_| HttpTransportError::InvalidCredential)?;
                val.set_sensitive(true);
                builder = builder.header(AUTHORIZATION, val);
            }
            RuntimeCredential::ModalProxy {
                token_id,
                token_secret,
            } => {
                let secret_str = token_secret
                    .expose_str()
                    .map_err(|_| HttpTransportError::InvalidCredential)?;
                let mut key_val = HeaderValue::from_str(token_id)
                    .map_err(|_| HttpTransportError::InvalidCredential)?;
                key_val.set_sensitive(true);
                let mut secret_val = HeaderValue::from_str(secret_str)
                    .map_err(|_| HttpTransportError::InvalidCredential)?;
                secret_val.set_sensitive(true);

                builder = builder.header("Modal-Key", key_val);
                builder = builder.header("Modal-Secret", secret_val);
            }
        }
        Ok(builder)
    }

    fn send_get_json<T: DeserializeOwned>(
        &self,
        url: Url,
        max_bytes: u64,
    ) -> Result<T, HttpTransportError> {
        let req = self.client.get(url).header(ACCEPT, "application/json");
        let prepared = self.attach_auth(req)?;

        let resp = prepared
            .send()
            .map_err(|_| HttpTransportError::ConnectionError)?;

        let status = resp.status();
        if status.is_redirection() || resp.headers().contains_key(LOCATION) {
            return Err(HttpTransportError::RedirectForbidden);
        }

        if !status.is_success() {
            return Err(HttpTransportError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }

        Self::verify_content_type(&resp, "application/json")?;
        let body_bytes = Self::read_bounded_body(resp, max_bytes)?;

        serde_json::from_slice::<T>(&body_bytes)
            .map_err(|_| HttpTransportError::JsonDeserialization)
    }

    /// Authenticated health check (`GET /mc/v1/health`).
    pub fn get_health(&self) -> Result<HealthResponse, HttpTransportError> {
        let url = self.target.build_fixed_route(FixedControlRoute::Health)?;
        let resp: HealthResponse = self.send_get_json(url, MAX_CONTROL_JSON_BYTES)?;

        validate_health_response(&resp).map_err(|_| HttpTransportError::WireValidation)?;

        if resp.provider != self.target.provider {
            return Err(HttpTransportError::ProviderMismatch);
        }

        Ok(resp)
    }

    /// Authenticated model-info runtime capability check (`GET /mc/v1/model-info`).
    pub fn get_model_info(&self) -> Result<ModelInfoResponse, HttpTransportError> {
        let url = self.target.build_fixed_route(FixedControlRoute::ModelInfo)?;
        let resp: ModelInfoResponse = self.send_get_json(url, MAX_CONTROL_JSON_BYTES)?;

        validate_model_info_response(&resp).map_err(|_| HttpTransportError::WireValidation)?;

        if resp.provider != self.target.provider {
            return Err(HttpTransportError::ProviderMismatch);
        }

        Ok(resp)
    }

    /// Which GPU containers are up (`GET /mc/v1/gpu`). The gateway reads heartbeats and
    /// never starts a GPU. `Ok(None)`: the deployment predates the route.
    pub fn get_gpu_status(&self) -> Result<Option<crate::inference::gpu::GpuStatusWire>, HttpTransportError> {
        let url = self.target.build_fixed_route(FixedControlRoute::Gpu)?;
        match self.send_gpu_request(self.client.get(url))? {
            Some(bytes) => crate::inference::gpu::parse_gpu_status(&bytes, self.target.provider).map(Some),
            None => Ok(None),
        }
    }

    /// Stop GPU containers (`POST /mc/v1/gpu/stop`): one role's, or every role's when
    /// `role` is `None`. Idempotent on the gateway. `Ok(None)`: the deployment predates
    /// the route or cannot stop its GPU.
    pub fn stop_gpu(
        &self,
        role: Option<crate::inference::gpu::GpuRole>,
    ) -> Result<Option<crate::inference::gpu::GpuStopWire>, HttpTransportError> {
        self.post_gpu_stop(role, false)
    }

    /// The same route with `idle_only`: the gateway releases `role`'s container only
    /// if it is idle with nothing in flight, and cancels nothing.
    pub fn release_idle_gpu(
        &self,
        role: crate::inference::gpu::GpuRole,
    ) -> Result<Option<crate::inference::gpu::GpuStopWire>, HttpTransportError> {
        self.post_gpu_stop(Some(role), true)
    }

    fn post_gpu_stop(
        &self,
        role: Option<crate::inference::gpu::GpuRole>,
        idle_only: bool,
    ) -> Result<Option<crate::inference::gpu::GpuStopWire>, HttpTransportError> {
        let url = self.target.build_fixed_route(FixedControlRoute::GpuStop)?;
        let body = crate::inference::gpu::stop_request_body(role, idle_only);
        let req = self.client.post(url).header(CONTENT_TYPE, "application/json").body(body);
        match self.send_gpu_request(req)? {
            Some(bytes) => crate::inference::gpu::parse_gpu_stop(&bytes, self.target.provider).map(Some),
            None => Ok(None),
        }
    }

    /// One GPU route: 404 (older deployment) and 501 (provider cannot) are `None`,
    /// any other failure an error, a success its bounded JSON body.
    fn send_gpu_request(&self, req: RequestBuilder) -> Result<Option<Vec<u8>>, HttpTransportError> {
        let prepared = self.attach_auth(req.header(ACCEPT, "application/json"))?;
        let resp = prepared.send().map_err(|_| HttpTransportError::ConnectionError)?;
        let status = resp.status();
        if status.is_redirection() || resp.headers().contains_key(LOCATION) {
            return Err(HttpTransportError::RedirectForbidden);
        }
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::NOT_IMPLEMENTED {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(HttpTransportError::UnexpectedStatus { status: status.as_u16() });
        }
        Self::verify_content_type(&resp, "application/json")?;
        Self::read_bounded_body(resp, MAX_GPU_JSON_BYTES).map(Some)
    }

    /// Modal's durable dispatch backend owns this handle scheme. Other providers
    /// remain unresolved until they offer an authenticated attempt lookup contract.
    pub fn lookup_attempt(
        &self, request: &JobRequestMetadata,
    ) -> Result<Option<JobStatusResponse>, HttpTransportError> {
        match self.target.provider {
            CloudProvider::Modal => {
                let handle = modal_attempt_handle(&request.job_id, &request.attempt_id);
                self.get_job_status(&handle, request).map(Some)
            }
            CloudProvider::Beam => Ok(None),
        }
    }

    /// CPU-only deployment handshake. Missing capabilities require a redeploy.
    pub fn gateway_is_current(&self) -> Result<bool, HttpTransportError> {
        let mut url = self.target.base_url.clone();
        url.set_path(&format!("{}/capabilities", url.path().trim_end_matches('/')));
        let response: serde_json::Value = match self.send_get_json(url, MAX_CONTROL_JSON_BYTES) {
            Ok(response) => response,
            Err(HttpTransportError::UnexpectedStatus { status: 404 | 501 }) => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(response["protocol_version"] == cleaner_core::cloud_wire::PROTOCOL_VERSION
            && response["provider"] == match self.target.provider {
                CloudProvider::Modal => "modal", CloudProvider::Beam => "beam",
            }
            && required_gateway_operations(self.target.provider).all(|required| {
                response["operations"].as_array().is_some_and(|ops| ops.iter().any(|op| op == required))
            }))
    }

    /// Which cloud code the deployment runs, from `code_digest` in `/capabilities`
    /// (`deploy/cloud/common/release.py`). `None`: a gateway too old to say.
    pub fn gateway_code_digest(&self) -> Result<Option<String>, HttpTransportError> {
        let mut url = self.target.base_url.clone();
        url.set_path(&format!("{}/capabilities", url.path().trim_end_matches('/')));
        let response: serde_json::Value = match self.send_get_json(url, MAX_CONTROL_JSON_BYTES) {
            Ok(response) => response,
            Err(HttpTransportError::UnexpectedStatus { status: 404 | 501 }) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(response["code_digest"].as_str()
            .filter(|digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
            .map(str::to_string))
    }

    /// Authenticated job status check bound to expected handle and request metadata (`GET /mc/v1/jobs/{handle}`).
    pub fn get_job_status(
        &self,
        handle: &str,
        request_meta: &JobRequestMetadata,
    ) -> Result<JobStatusResponse, HttpTransportError> {
        validate_job_request_metadata(request_meta).map_err(|_| HttpTransportError::WireValidation)?;

        let url = self.target.build_job_route(handle, JobSubResource::Status)?;
        let resp: JobStatusResponse = self.send_get_json(url, MAX_CONTROL_JSON_BYTES)?;

        validate_response_binding(request_meta, &resp, Some(handle))
            .map_err(|_| HttpTransportError::WireValidation)?;

        Ok(resp)
    }

    /// Fetch raw result PNG bytes from protected result endpoint (`GET /mc/v1/jobs/{handle}/result`).
    ///
    /// Download bytes are capped by client [`ServiceLimits::max_png_bytes`] and verified
    /// against [`validate_result_bytes`] before returning.
    pub fn fetch_result_bytes(
        &self,
        handle: &str,
        result_meta: &ResultMetadata,
        request_meta: &JobRequestMetadata,
        limits: &ServiceLimits,
    ) -> Result<Vec<u8>, HttpTransportError> {
        self.fetch_result_bytes_while(handle, result_meta, request_meta, limits, &|| true)
    }

    /// [`Self::fetch_result_bytes`] for as long as `keep_going` answers true.
    ///
    /// A result is the one large body this client reads, and a slow link can
    /// stall it past the read timeout. A transfer that stops is asked again for
    /// the bytes still missing (`Range`), so what arrived is kept. It fails
    /// after [`RESULT_STALLED_TRIES`] requests in a row that add nothing, or
    /// once [`RESULT_DOWNLOAD_BUDGET`] is spent. The digest check on the whole
    /// result covers the joined parts.
    pub fn fetch_result_bytes_while(
        &self,
        handle: &str,
        result_meta: &ResultMetadata,
        request_meta: &JobRequestMetadata,
        limits: &ServiceLimits,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, HttpTransportError> {
        let url = self.target.build_job_route(handle, JobSubResource::Result)?;
        let max_bytes = limits.max_png_bytes.min(cleaner_core::cloud_decode::MAX_RESULT_ENCODED_BYTES);
        let started = Instant::now();
        let mut bytes = Vec::new();
        let mut stalled = 0;
        loop {
            let had = bytes.len();
            match self.fetch_result_part(url.clone(), &mut bytes, max_bytes, keep_going) {
                Ok(()) => break,
                Err(error @ (HttpTransportError::ConnectionError | HttpTransportError::Timeout)) => {
                    stalled = if bytes.len() > had { 0 } else { stalled + 1 };
                    if stalled >= RESULT_STALLED_TRIES
                        || started.elapsed() >= RESULT_DOWNLOAD_BUDGET
                        || !keep_going()
                    {
                        return Err(error);
                    }
                    thread::sleep(RESULT_RETRY_PAUSE);
                }
                Err(error) => return Err(error),
            }
        }

        validate_result_bytes(&bytes, result_meta, request_meta, limits, handle)
            .map_err(|_| HttpTransportError::WireValidation)?;

        Ok(bytes)
    }

    /// One result request: the whole body, or the rest of it when `bytes`
    /// holds a part. Bytes read before a fault stay in `bytes`. A gateway that
    /// does not serve ranges answers 200 with the whole result, which replaces
    /// the part.
    fn fetch_result_part(
        &self,
        url: Url,
        bytes: &mut Vec<u8>,
        max_bytes: u64,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<(), HttpTransportError> {
        let mut req = self.client.get(url).header(ACCEPT, "image/png");
        if !bytes.is_empty() {
            req = req.header(RANGE, format!("bytes={}-", bytes.len()));
        }
        let mut resp = self.attach_auth(req)?
            .send()
            .map_err(|_| HttpTransportError::ConnectionError)?;

        let status = resp.status();
        if status.is_redirection() || resp.headers().contains_key(LOCATION) {
            return Err(HttpTransportError::RedirectForbidden);
        }

        if !status.is_success() {
            return Err(HttpTransportError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }

        Self::verify_content_type(&resp, "image/png")?;
        if status == StatusCode::PARTIAL_CONTENT {
            let first = resp.headers().get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("bytes "))
                .and_then(|value| value.split('-').next())
                .and_then(|value| value.parse::<usize>().ok());
            if first != Some(bytes.len()) {
                return Err(HttpTransportError::WireValidation);
            }
        } else {
            bytes.clear();
        }

        let mut chunk = [0u8; 64 * 1024];
        loop {
            if !keep_going() {
                return Err(HttpTransportError::ConnectionError);
            }
            match resp.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.len() as u64 > max_bytes {
                        return Err(HttpTransportError::BodySizeLimitExceeded);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(HttpTransportError::ConnectionError),
            }
        }
    }

    /// Submit a crop rendering job (`POST /mc/v1/jobs`).
    pub fn submit_job(
        &self,
        request_meta: &JobRequestMetadata,
        image_bytes: &[u8],
        hint_bytes: &[u8],
        limits: &ServiceLimits,
    ) -> Result<JobAcceptedResponse, HttpTransportError> {
        self.submit_job_inner(request_meta, image_bytes, hint_bytes, limits)
            .map_err(|(error, _)| error)
    }

    /// Debug probe uses the production submit path and exposes only whether a
    /// bounded, request-bound pre-enqueue rejection named the old recipe.
    #[cfg(debug_assertions)]
    pub fn submit_job_probe(
        &self,
        request_meta: &JobRequestMetadata,
        image_bytes: &[u8],
        hint_bytes: &[u8],
        limits: &ServiceLimits,
    ) -> Result<JobAcceptedResponse, ProbeSubmitError> {
        self.submit_job_inner(request_meta, image_bytes, hint_bytes, limits)
            .map_err(|(error, exact_unsupported_recipe)| ProbeSubmitError { error, exact_unsupported_recipe })
    }

    fn submit_job_inner(
        &self,
        request_meta: &JobRequestMetadata,
        image_bytes: &[u8],
        hint_bytes: &[u8],
        limits: &ServiceLimits,
    ) -> Result<JobAcceptedResponse, (HttpTransportError, bool)> {
        cleaner_core::cloud_wire::validate_crop_payload(
            request_meta,
            image_bytes,
            hint_bytes,
            limits,
        )
        .map_err(|_| (HttpTransportError::WireValidation.not_enqueued(), false))?;

        let url = self.target.build_jobs_route().map_err(|error| (error.not_enqueued(), false))?;
        let meta_json = serde_json::to_vec(request_meta)
            .map_err(|_| (HttpTransportError::WireValidation.not_enqueued(), false))?;

        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let boundary = format!("------------------------manga_cleaner_{}_{}", nanos, count);
        let body = build_multipart_body(&boundary, &meta_json, image_bytes, hint_bytes);

        let ct = format!("multipart/form-data; boundary={boundary}");
        let req = self
            .client
            .post(url)
            .header(CONTENT_TYPE, ct)
            .header(ACCEPT, "application/json")
            .body(body);

        let prepared = self.attach_auth(req).map_err(|error| (error.not_enqueued(), false))?;
        let resp = prepared
            .send()
            .map_err(|error| (submit_send_error(error), false))?;

        let status = resp.status();
        if status.is_redirection() || resp.headers().contains_key(LOCATION) {
            return Err((HttpTransportError::RedirectForbidden, false));
        }

        if status == reqwest::StatusCode::ACCEPTED {
            Self::verify_content_type(&resp, "application/json").map_err(|error| (error, false))?;
            let body_bytes = Self::read_bounded_body(resp, MAX_CONTROL_JSON_BYTES).map_err(|error| (error, false))?;
            let accepted: JobAcceptedResponse = serde_json::from_slice(&body_bytes)
                .map_err(|_| (HttpTransportError::JsonDeserialization, false))?;
            validate_response_binding(request_meta, &accepted, None)
                .map_err(|_| (HttpTransportError::WireValidation, false))?;
            return Ok(accepted);
        }

        let status = status.as_u16();
        let body = Self::read_bounded_body(resp, MAX_CONTROL_JSON_BYTES).unwrap_or_default();
        #[cfg(debug_assertions)]
        let exact = exact_unsupported_recipe(status, &body, request_meta);
        #[cfg(not(debug_assertions))]
        let exact = false;
        Err((submit_rejection(status, &body, request_meta), exact))
    }

    /// Request job cancellation (`POST /mc/v1/jobs/{handle}/cancel`).
    pub fn cancel_job(
        &self,
        handle: &str,
        request_meta: &JobRequestMetadata,
    ) -> Result<JobCancelResponse, HttpTransportError> {
        let url = self.target.build_job_route(handle, JobSubResource::Cancel)?;
        let req = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body("{}");

        let prepared = self.attach_auth(req)?;
        let resp = prepared
            .send()
            .map_err(|_| HttpTransportError::ConnectionError)?;

        let status = resp.status();
        if status.is_redirection() || resp.headers().contains_key(LOCATION) {
            return Err(HttpTransportError::RedirectForbidden);
        }

        if !status.is_success() {
            return Err(HttpTransportError::UnexpectedStatus {
                status: status.as_u16(),
            });
        }

        Self::verify_content_type(&resp, "application/json")?;
        let body_bytes = Self::read_bounded_body(resp, MAX_CONTROL_JSON_BYTES)?;
        let cancel_resp: JobCancelResponse = serde_json::from_slice(&body_bytes)
            .map_err(|_| HttpTransportError::JsonDeserialization)?;

        cleaner_core::cloud_wire::validate_cancel_response(request_meta, &cancel_resp, handle)
            .map_err(|_| HttpTransportError::WireValidation)?;

        Ok(cancel_resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn gateway_required_operations_are_provider_specific() {
        let beam: Vec<_> = required_gateway_operations(CloudProvider::Beam).collect();
        assert_eq!(beam, REQUIRED_GATEWAY_OPERATIONS);
        assert!(!beam.iter().any(|operation| operation.starts_with("gpu.")));
        let modal: Vec<_> = required_gateway_operations(CloudProvider::Modal).collect();
        assert_eq!(modal.len(), beam.len() + MODAL_GPU_OPERATIONS.len());
        assert!(modal.contains(&"gpu.release_idle"));
    }

    #[test]
    fn test_loopback_presend_validation_auth_and_connection_refused_are_not_enqueued() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let request: JobRequestMetadata = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/job_request_metadata.json")).unwrap();
        let image = include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");
        let hint = include_bytes!("../../../deploy/cloud/fixtures/tiny_hint.png");
        for case in ["validation", "auth", "refused"] {
            let target = CloudEndpointTarget::new_test_target(CloudProvider::Beam, "beam-test", &format!("http://{address}/mc/v1")).unwrap();
            let credential = BoundRuntimeCredential::new(&target, RuntimeCredential::BeamBearer(SecretValue::new("token"))).unwrap();
            let mut client = CloudHttpClient::new_test_client(target, credential, Client::new());
            if case == "auth" { client.credential.credential = RuntimeCredential::BeamBearer(SecretValue::new("invalid\nheader")); }
            let crop = if case == "validation" { &b"invalid"[..] } else { image.as_slice() };
            let error = client.submit_job(&request, crop, hint, &cleaner_core::cloud_wire::provisional_fixture_limits()).unwrap_err();
            assert!(error.definitely_not_enqueued(), "{case}: {error}");
            let HttpTransportError::NotEnqueued { reason, .. } = error else { unreachable!() };
            assert!(matches!((case, *reason), ("validation", HttpTransportError::WireValidation)
                | ("auth", HttpTransportError::InvalidCredential) | ("refused", HttpTransportError::ConnectionError)));
        }
    }

    #[test]
    fn missing_weights_rejection_is_bound_and_definitely_not_enqueued() {
        let request: JobRequestMetadata = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/job_request_metadata.json")).unwrap();
        let mut body = serde_json::json!({ "enqueued": false, "error_code": "weights_missing", "message": "Repair the installation",
            "job_id": request.job_id, "attempt_id": request.attempt_id, "request_digest": request.request_digest, "retryable": false });
        for code in ["weights_missing", "weights_corrupt"] {
            body["error_code"] = code.into();
            let error = submit_rejection(503, &serde_json::to_vec(&body).unwrap(), &request);
            assert!(matches!(error, HttpTransportError::NotEnqueued { reason, retryable: false } if *reason == HttpTransportError::RenderWeightsUnavailable));
        }
        body["request_digest"] = "0".repeat(64).into();
        assert!(!submit_rejection(503, &serde_json::to_vec(&body).unwrap(), &request).definitely_not_enqueued());
    }

    #[test]
    fn a_paused_provider_gets_no_client() {
        let target = CloudEndpointTarget::new(CloudProvider::Beam, "beam-prod", "https://mc-ab12cd-gateway.app.beam.cloud/mc/v1").expect("valid target");
        let credential = BoundRuntimeCredential::new(&target, RuntimeCredential::BeamBearer(SecretValue::new("token"))).unwrap();
        assert!(matches!(CloudHttpClient::new(target, credential), Err(HttpTransportError::ProviderPaused)));
    }

    #[test]
    fn submission_rejections_distinguish_no_enqueue_from_uncertainty() {
        let request: JobRequestMetadata = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/job_request_metadata.json")).unwrap();
        for (status, retryable) in [(400, false), (401, false), (503, true)] {
            let mut body = serde_json::json!({ "enqueued": false, "error_code": if retryable { "service_unavailable_pre_queue" } else { "unsupported_recipe" },
                "message": "GPU was just stopped", "job_id": request.job_id,
                "attempt_id": request.attempt_id, "request_digest": request.request_digest, "retryable": retryable });
            assert!(matches!(submit_rejection(status, &serde_json::to_vec(&body).unwrap(), &request),
                HttpTransportError::NotEnqueued { retryable: actual, .. } if actual == retryable));
            body["enqueued"] = true.into();
            if status != 401 {
                assert!(!submit_rejection(status, &serde_json::to_vec(&body).unwrap(), &request).definitely_not_enqueued());
            }
        }
        for status in [400, 503] {
            assert!(!submit_rejection(status, b"broken response", &request).definitely_not_enqueued());
        }
        assert!(HttpTransportError::WireValidation.not_enqueued().definitely_not_enqueued());
        assert!(!HttpTransportError::ConnectionError.definitely_not_enqueued());
    }

    #[test]
    fn old_recipe_probe_reaches_http_and_requires_exact_bound_refusal() {
        let mut request: JobRequestMetadata = serde_json::from_str(
            include_str!("../../../deploy/cloud/fixtures/job_request_metadata.json")
        ).unwrap();
        let image = include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");
        let hint = include_bytes!("../../../deploy/cloud/fixtures/tiny_hint.png");
        request.recipe.preprocessing_version = "1.0.0".into();
        request.request_digest = cleaner_core::cloud_wire::compute_request_digest(&request);
        let limits = cleaner_core::cloud_wire::provisional_fixture_limits();
        cleaner_core::cloud_wire::validate_crop_payload(&request, image, hint, &limits).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bound = request.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut body = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                body.extend_from_slice(&chunk[..n]);
                if let Some(header_end) = body.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&body[..header_end]);
                    let content_length = headers.lines().find_map(|line| {
                        line.to_ascii_lowercase().strip_prefix("content-length: ")
                            .and_then(|length| length.parse::<usize>().ok())
                    }).unwrap();
                    if body.len() >= header_end + 4 + content_length { break; }
                }
            }
            let sent = String::from_utf8_lossy(&body);
            assert!(sent.starts_with("POST /mc/v1/jobs HTTP/1.1"));
            assert!(sent.contains("\"preprocessing_version\":\"1.0.0\""));
            assert!(sent.contains(&bound.request_digest));
            let rejection = serde_json::json!({
                "enqueued": false, "error_code": "unsupported_recipe", "message": "old preprocessing",
                "job_id": bound.job_id, "attempt_id": bound.attempt_id,
                "request_digest": bound.request_digest, "retryable": false
            }).to_string();
            let response = format!("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", rejection.len(), rejection);
            stream.write_all(response.as_bytes()).unwrap();
        });
        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Beam, "beam-probe", &format!("http://127.0.0.1:{port}/mc/v1")
        ).unwrap();
        let credential = BoundRuntimeCredential::new(
            &target, RuntimeCredential::BeamBearer(SecretValue::new("test-token"))
        ).unwrap();
        let client = CloudHttpClient::new_test_client(target, credential, Client::new());
        let error = client.submit_job_probe(&request, image, hint, &limits).unwrap_err();
        assert!(error.exact_unsupported_recipe);
        assert!(error.error.definitely_not_enqueued());
        server.join().unwrap();

        let mut wrong = request.clone();
        wrong.request_digest = "a".repeat(64);
        let rejection = serde_json::json!({
            "enqueued": false, "error_code": "unsupported_recipe", "message": "old preprocessing",
            "job_id": request.job_id, "attempt_id": request.attempt_id,
            "request_digest": request.request_digest, "retryable": false
        });
        assert!(!exact_unsupported_recipe(400, rejection.to_string().as_bytes(), &wrong));
        assert!(!exact_unsupported_recipe(503, rejection.to_string().as_bytes(), &request));
    }

    #[test]
    fn modal_attempt_handle_matches_gateway_contract() {
        assert_eq!(modal_attempt_handle("job-test", "att-test"),
            "handle-modal-32f9dc0aafefdec6245d2cf8723c7b70");
    }

    #[test]
    fn gateway_handshake_requires_protocol_provider_and_idle_release() {
        for case in ["current", "beam", "missing_idle", "old_protocol", "wrong_provider", "old_gateway"] {
            let provider = if case == "beam" { CloudProvider::Beam } else { CloudProvider::Modal };
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let size = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..size]).to_lowercase();
                assert!(request.starts_with("get /mc/v1/capabilities "));
                if case == "beam" { assert!(request.contains("authorization: bearer beam-token")); }
                else {
                    assert!(request.contains("modal-key: tid"));
                    assert!(request.contains("modal-secret: tsec"));
                }
                let mut operations: Vec<_> = required_gateway_operations(provider).collect();
                if case == "missing_idle" { operations.retain(|op| *op != "gpu.release_idle"); }
                let body = serde_json::json!({
                    "protocol_version": if case == "old_protocol" { "old" } else { cleaner_core::cloud_wire::PROTOCOL_VERSION },
                    "provider": if case == "wrong_provider" || case == "beam" { "beam" } else { "modal" },
                    "operations": operations,
                }).to_string();
                let status = if case == "old_gateway" { "404 Not Found" } else { "200 OK" };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let target = CloudEndpointTarget::new_test_target(provider, "modal-1",
                &format!("http://{address}/mc/v1")).unwrap();
            let credential = BoundRuntimeCredential::new(&target, if case == "beam" {
                RuntimeCredential::BeamBearer(SecretValue::new("beam-token"))
            } else { RuntimeCredential::ModalProxy {
                token_id: "tid".into(), token_secret: SecretValue::new("tsec"),
            } }).unwrap();
            let client = CloudHttpClient::new_test_client(target, credential, reqwest::blocking::Client::new());
            assert_eq!(client.gateway_is_current().unwrap(), matches!(case, "current" | "beam"), "{case}");
            server.join().unwrap();
        }
    }

    #[test]
    fn gateway_code_digest_is_read_from_capabilities_or_absent() {
        let digest = "0123456789abcdef".repeat(4);
        for (case, expected) in [
            ("current", Some(digest.clone())), ("none", None), ("bad", None), ("old_gateway", None),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let advertised = digest.clone();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let size = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..size]).to_lowercase().starts_with("get /mc/v1/capabilities "));
                let mut body = serde_json::json!({"protocol_version": "1.0.0", "provider": "modal", "operations": []});
                match case {
                    "current" => body["code_digest"] = advertised.into(),
                    "bad" => body["code_digest"] = "not-a-digest".into(),
                    _ => {}
                }
                let body = body.to_string();
                let status = if case == "old_gateway" { "404 Not Found" } else { "200 OK" };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let target = CloudEndpointTarget::new_test_target(CloudProvider::Modal, "modal-1",
                &format!("http://{address}/mc/v1")).unwrap();
            let credential = BoundRuntimeCredential::new(&target, RuntimeCredential::ModalProxy {
                token_id: "tid".into(), token_secret: SecretValue::new("tsec"),
            }).unwrap();
            let client = CloudHttpClient::new_test_client(target, credential, reqwest::blocking::Client::new());
            assert_eq!(client.gateway_code_digest().unwrap(), expected, "{case}");
            server.join().unwrap();
        }
    }

    #[test]
    fn analysis_unauthorized_rejection_preserves_status() {
        let body = include_bytes!("../../../deploy/cloud/fixtures/analysis_v1/unauthorized_error.json");
        assert_eq!(
            analysis_rejection_error(401, body, &"a".repeat(64)),
            HttpTransportError::UnexpectedStatus { status: 401 },
        );
        assert_eq!(
            analysis_rejection_error(400, body, &"a".repeat(64)),
            HttpTransportError::WireValidation,
        );
        let unavailable = include_bytes!("../../../deploy/cloud/fixtures/analysis_v1/capability_unavailable_error.json");
        assert_eq!(
            analysis_rejection_error(503, unavailable, &"a".repeat(64)),
            HttpTransportError::UnexpectedStatus { status: 503 },
        );
        let failed = include_bytes!("../../../deploy/cloud/fixtures/analysis_v1/inference_failed_error.json");
        let digest = "6b05d6efca8c347fe3ebb2ac8021c1c0584c13c1fab60559590e12045311d393";
        assert_eq!(
            analysis_rejection_error(500, failed, digest),
            HttpTransportError::UnexpectedStatus { status: 500 },
        );
        assert_eq!(
            analysis_rejection_error(500, failed, &"a".repeat(64)),
            HttpTransportError::WireValidation,
        );
    }

    #[test]
    fn denoise_rejection_is_bound_to_the_request_digest() {
        let digest = "b".repeat(64);
        let answer = |code: &str, digest: Option<&str>| serde_json::to_vec(&serde_json::json!({
            "protocol_version": "1.0.0", "error_code": code, "message": "refused", "request_digest": digest,
        })).unwrap();
        assert_eq!(denoise_rejection_error(422, &answer("unsupported_page", Some(&digest)), &digest),
            HttpTransportError::UnexpectedStatus { status: 422 });
        assert_eq!(denoise_rejection_error(401, &answer("unauthorized", None), &digest),
            HttpTransportError::UnexpectedStatus { status: 401 });
        assert_eq!(denoise_rejection_error(500, &answer("inference_failed", Some(&"c".repeat(64))), &digest),
            HttpTransportError::WireValidation, "another request's digest");
        assert_eq!(denoise_rejection_error(500, &answer("inference_failed", None), &digest),
            HttpTransportError::WireValidation);
        assert_eq!(denoise_rejection_error(400, &answer("teapot", Some(&digest)), &digest),
            HttpTransportError::WireValidation);
        assert_eq!(denoise_rejection_error(502, b"<html>bad gateway</html>", &digest),
            HttpTransportError::WireValidation);
    }
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn gpu_price_reads_fresh_http_state_in_both_directions() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let priced = serde_json::json!({
                "provider": "modal", "supported": true, "now": 1000.0,
                "containers": [], "list_price_usd_per_hour": { "A100": 2.0 }
            }).to_string();
            for body in [None, Some(&priced), Some(&priced), None] {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /mc/v1/gpu "));
                let response = match body {
                    Some(body) => format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                };
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal, "modal-test", &format!("http://127.0.0.1:{port}/mc/v1"),
        ).unwrap();
        let credential = BoundRuntimeCredential::new(&target, RuntimeCredential::ModalProxy {
            token_id: "test-id".into(), token_secret: SecretValue::new("test-secret"),
        }).unwrap();
        let client = CloudHttpClient::new_test_client(target, credential, Client::new());
        let price = || client.get_gpu_status().unwrap().and_then(|status| {
            let mut entries = status.list_price_usd_per_hour.into_iter();
            let one = entries.next()?;
            entries.next().is_none().then_some(one)
        });
        assert_eq!(price(), None);
        assert_eq!(price(), Some(("A100".into(), 2.0)));
        assert_eq!(price(), Some(("A100".into(), 2.0)));
        assert_eq!(price(), None);
        server.join().unwrap();
    }

    #[test]
    fn test_public_ip_classifier_ipv4() {
        // Forbidden: Loopback
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(127, 255, 255, 255))));

        // Forbidden: Private ranges (RFC 1918)
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 254))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));

        // Forbidden: Link local / Cloud metadata
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));

        // Forbidden: CGNAT (100.64.0.0/10)
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(100, 127, 255, 255))));

        // Forbidden: Documentation & Benchmarking
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1))));

        // Forbidden: Multicast & Future Reserved
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1))));
        assert!(!is_public_ip(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255))));

        // Allowed: Public IPs
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))));
    }

    #[test]
    fn test_public_ip_classifier_ipv6_conservative() {
        // Forbidden: Outside 2000::/3
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!is_public_ip(IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
        assert!(!is_public_ip(IpAddr::V6("fe80::1".parse().unwrap())));
        assert!(!is_public_ip(IpAddr::V6("fec0::1".parse().unwrap())));
        assert!(!is_public_ip(IpAddr::V6("fc00::1".parse().unwrap())));
        assert!(!is_public_ip(IpAddr::V6("ff02::1".parse().unwrap())));
        assert!(!is_public_ip(IpAddr::V6("100::1".parse().unwrap())));

        // Forbidden: Special ranges within 2000::/3
        assert!(!is_public_ip(IpAddr::V6("2001:db8::1".parse().unwrap()))); // Documentation
        assert!(!is_public_ip(IpAddr::V6("2001:2::1".parse().unwrap()))); // Benchmarking
        assert!(!is_public_ip(IpAddr::V6("2001::1".parse().unwrap()))); // Teredo
        assert!(!is_public_ip(IpAddr::V6("2002:c0a8:101::1".parse().unwrap()))); // 6to4
        assert!(!is_public_ip(IpAddr::V6("3ffe::1".parse().unwrap()))); // 6bone

        // Allowed: Global 2000::/3
        assert!(is_public_ip(IpAddr::V6("2606:4700:4700::1111".parse().unwrap())));
        assert!(is_public_ip(IpAddr::V6("2a00:1450:4009:81f::200e".parse().unwrap())));
    }

    #[test]
    fn test_cloud_endpoint_target_fixed_routes_and_handle_rejection() {
        let target = CloudEndpointTarget::new(
            CloudProvider::Modal,
            "modal-prod",
            "https://modal-cleaner.run.modal.com/mc/v1",
        )
        .expect("valid target");

        assert_eq!(
            target.build_fixed_route(FixedControlRoute::Health).unwrap().as_str(),
            "https://modal-cleaner.run.modal.com/mc/v1/health"
        );
        assert_eq!(
            target.build_fixed_route(FixedControlRoute::ModelInfo).unwrap().as_str(),
            "https://modal-cleaner.run.modal.com/mc/v1/model-info"
        );
        assert_eq!(
            target.build_job_route("handle-123", JobSubResource::Status).unwrap().as_str(),
            "https://modal-cleaner.run.modal.com/mc/v1/jobs/handle-123"
        );
        assert_eq!(
            target.build_job_route("handle-123", JobSubResource::Result).unwrap().as_str(),
            "https://modal-cleaner.run.modal.com/mc/v1/jobs/handle-123/result"
        );

        // Dotsegments and URL injection rejected
        assert_eq!(
            target.build_job_route("..", JobSubResource::Status),
            Err(HttpTransportError::InvalidHandle)
        );
        assert_eq!(
            target.build_job_route(".", JobSubResource::Status),
            Err(HttpTransportError::InvalidHandle)
        );
        assert_eq!(
            target.build_job_route("handle/with/slash", JobSubResource::Status),
            Err(HttpTransportError::InvalidHandle)
        );
        assert_eq!(
            target.build_job_route("handle%20space", JobSubResource::Status),
            Err(HttpTransportError::InvalidHandle)
        );
    }

    #[test]
    fn test_credential_provider_binding() {
        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-1",
            "https://localhost.test/mc/v1",
        )
        .unwrap();

        // Mismatched provider variant
        let beam_cred = RuntimeCredential::BeamBearer(SecretValue::new("beam-token"));
        let err = BoundRuntimeCredential::new(&target, beam_cred).err().unwrap();
        assert_eq!(err, HttpTransportError::CredentialProviderMismatch);

        // Valid ModalProxy variant
        let modal_cred = RuntimeCredential::ModalProxy {
            token_id: "tid-123".to_string(),
            token_secret: SecretValue::new("tsec-456"),
        };
        let bound = BoundRuntimeCredential::new(&target, modal_cred).unwrap();
        assert_eq!(bound.provider, CloudProvider::Modal);
    }

    #[test]
    fn test_loopback_mock_redirect_forbidden() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Read the request before answering: closing with it unread
                // resets the connection on Windows and the client never sees
                // the response this test is about.
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let response = "HTTP/1.1 302 Found\r\nLocation: https://evil.com/leak\r\nContent-Length: 0\r\n\r\n";
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Beam,
            "beam-1",
            &format!("http://127.0.0.1:{port}/mc/v1"),
        )
        .unwrap();

        let cred = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::BeamBearer(SecretValue::new("token")),
        )
        .unwrap();

        let client = CloudHttpClient::new_test_client(target, cred, Client::builder().redirect(Policy::none())
            .retry(reqwest::retry::never()).build().unwrap());
        let err = client.get_health().unwrap_err();
        assert_eq!(err, HttpTransportError::RedirectForbidden);
    }

    #[test]
    fn test_loopback_mock_chunked_over_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the request before closing the response socket: unread request
                // bytes can cause a TCP reset and hide the intended body-limit error.
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n";
                let _ = stream.write_all(header.as_bytes());
                // Emit huge chunk > 1MB
                let chunk_size = MAX_CONTROL_JSON_BYTES + 100;
                let chunk_header = format!("{:x}\r\n", chunk_size);
                let _ = stream.write_all(chunk_header.as_bytes());
                let payload = vec![b'a'; chunk_size as usize];
                let _ = stream.write_all(&payload);
                let _ = stream.write_all(b"\r\n0\r\n\r\n");
            }
        });

        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Beam,
            "beam-1",
            &format!("http://127.0.0.1:{port}/mc/v1"),
        )
        .unwrap();

        let cred = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::BeamBearer(SecretValue::new("token")),
        )
        .unwrap();

        let client = CloudHttpClient::new_test_client(target, cred, Client::new());
        let err = client.get_health().unwrap_err();
        assert_eq!(err, HttpTransportError::BodySizeLimitExceeded);
    }

    #[test]
    fn test_loopback_mock_mime_essence_mismatch() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Read the request first, as above, or Windows resets the connection.
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                // Return invalid MIME extension like application/json-evil or text/html
                let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json-evil; charset=utf-8\r\nContent-Length: 2\r\n\r\n{}";
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Beam,
            "beam-1",
            &format!("http://127.0.0.1:{port}/mc/v1"),
        )
        .unwrap();

        let cred = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::BeamBearer(SecretValue::new("token")),
        )
        .unwrap();

        let client = CloudHttpClient::new_test_client(target, cred, Client::new());
        let err = client.get_health().unwrap_err();
        assert_eq!(err, HttpTransportError::ContentTypeMismatch);
    }

    #[test]
    fn test_loopback_mock_auth_headers_and_status_binding() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let req_str = String::from_utf8_lossy(&buf);

                // Verify Modal-Key and Modal-Secret headers arrived
                assert!(req_str.contains("modal-key: test-modal-key"));
                assert!(req_str.contains("modal-secret: test-modal-sec"));

                // Return status response with wrong job_id
                let json = r#"{
                    "handle": "h-123",
                    "job_id": "wrong-job-id",
                    "attempt_id": "att-1",
                    "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
                    "recipe_id": "test-sdnq-v1",
                    "preprocessing_version": "1.0.0",
                    "model_id": "test-flux-schnell",
                    "model_revision": "0123456789abcdef0123456789abcdef01234567",
                    "native_mask_conditioning": false,
                    "status": "running"
                }"#;
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", json.len(), json);
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-1",
            &format!("http://127.0.0.1:{port}/mc/v1"),
        )
        .unwrap();

        let cred = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::ModalProxy {
                token_id: "test-modal-key".to_string(),
                token_secret: SecretValue::new("test-modal-sec"),
            },
        )
        .unwrap();

        let req_meta = JobRequestMetadata {
            protocol_version: "1.0.0".to_string(),
            job_id: "expected-job-id".to_string(),
            attempt_id: "att-1".to_string(),
            recipe: cleaner_core::cloud_wire::WireRenderRecipe {
                qwen_edit: None,
                recipe_id: "test-sdnq-v1".to_string(),
                preprocessing_version: "1.0.0".to_string(),
                model_id: "test-flux-schnell".to_string(),
                model_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
                native_mask_conditioning: false,
            },
            width: 16,
            height: 16,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            image_sha256: "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f".to_string(),
            hint_sha256: "3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238".to_string(),
            request_digest: "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0".to_string(),
        };

        let client = CloudHttpClient::new_test_client(target, cred, Client::new());
        let err = client.get_job_status("h-123", &req_meta).unwrap_err();
        assert!(matches!(err, HttpTransportError::WireValidation));
    }

    #[test]
    fn test_loopback_mock_submit_job_accepted() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);

                let req_str = String::from_utf8_lossy(&buf);
                let digest = if let Some(idx) = req_str.find("\"request_digest\":\"") {
                    let rest = &req_str[idx + 18..];
                    if let Some(end) = rest.find('"') {
                        &rest[..end]
                    } else {
                        "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0"
                    }
                } else {
                    "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0"
                };

                let json = format!(r#"{{
                    "handle": "h-test-456",
                    "status": "pending",
                    "job_id": "job-sub-1",
                    "attempt_id": "att-sub-1",
                    "request_digest": "{digest}",
                    "recipe_id": "test-sdnq-v1",
                    "preprocessing_version": "1.0.0",
                    "model_id": "test-flux-schnell",
                    "model_revision": "0123456789abcdef0123456789abcdef01234567",
                    "native_mask_conditioning": false
                }}"#);
                let response = format!("HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", json.len(), json);
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Beam,
            "beam-1",
            &format!("http://127.0.0.1:{port}/mc/v1"),
        )
        .unwrap();

        let cred = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::BeamBearer(SecretValue::new("test-token")),
        )
        .unwrap();

        let req_meta = JobRequestMetadata {
            protocol_version: "1.0.0".to_string(),
            job_id: "job-sub-1".to_string(),
            attempt_id: "att-sub-1".to_string(),
            recipe: cleaner_core::cloud_wire::WireRenderRecipe {
                qwen_edit: None,
                recipe_id: "test-sdnq-v1".to_string(),
                preprocessing_version: "1.0.0".to_string(),
                model_id: "test-flux-schnell".to_string(),
                model_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
                native_mask_conditioning: false,
            },
            width: 16,
            height: 16,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            image_sha256: "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f".to_string(),
            hint_sha256: "3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238".to_string(),
            request_digest: "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0".to_string(),
        };

        let limits = cleaner_core::cloud_wire::provisional_fixture_limits();
        let client = CloudHttpClient::new_test_client(target, cred, Client::new());

        // Dummy PNGs matching hashes:
        // Notice: to pass validate_crop_payload, the PNG headers and digests must match!
        let raster_rgb = cleaner_core::image::Raster {
            width: 16,
            height: 16,
            mode: cleaner_core::image::ColorMode::Rgb,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: vec![128; 16 * 16 * 3],
        };
        let img_bytes = cleaner_core::image::encode(&raster_rgb, cleaner_core::image::Format::Png).unwrap();
        let raster_gray = cleaner_core::image::Raster {
            width: 16,
            height: 16,
            mode: cleaner_core::image::ColorMode::Gray,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: vec![255; 16 * 16],
        };
        let hint_bytes = cleaner_core::image::encode(&raster_gray, cleaner_core::image::Format::Png).unwrap();

        let mut req_meta_valid = req_meta.clone();
        req_meta_valid.image_sha256 = cleaner_core::ingest::sha256_hex(&img_bytes);
        req_meta_valid.hint_sha256 = cleaner_core::ingest::sha256_hex(&hint_bytes);
        req_meta_valid.request_digest = cleaner_core::cloud_wire::compute_request_digest(&req_meta_valid);

        let accepted = client.submit_job(&req_meta_valid, &img_bytes, &hint_bytes, &limits).unwrap();
        assert_eq!(accepted.handle, "h-test-456");
        assert_eq!(accepted.status, cleaner_core::cloud_wire::JobExecutionStatus::Pending);
    }

    #[test]
    fn test_loopback_mock_cancel_job() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);

                let json = r#"{
                    "handle": "h-cancel-1",
                    "job_id": "job-can-1",
                    "attempt_id": "att-can-1",
                    "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
                    "status": "cancel_requested",
                    "acknowledged": true
                }"#;
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", json.len(), json);
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Beam,
            "beam-1",
            &format!("http://127.0.0.1:{port}/mc/v1"),
        )
        .unwrap();

        let cred = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::BeamBearer(SecretValue::new("test-token")),
        )
        .unwrap();

        let req_meta = JobRequestMetadata {
            protocol_version: "1.0.0".to_string(),
            job_id: "job-can-1".to_string(),
            attempt_id: "att-can-1".to_string(),
            recipe: cleaner_core::cloud_wire::WireRenderRecipe {
                qwen_edit: None,
                recipe_id: "test-sdnq-v1".to_string(),
                preprocessing_version: "1.0.0".to_string(),
                model_id: "test-flux-schnell".to_string(),
                model_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
                native_mask_conditioning: false,
            },
            width: 16,
            height: 16,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            image_sha256: "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f".to_string(),
            hint_sha256: "3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238".to_string(),
            request_digest: "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0".to_string(),
        };

        let client = CloudHttpClient::new_test_client(target, cred, Client::new());
        let cancel_resp = client.cancel_job("h-cancel-1", &req_meta).unwrap();
        assert_eq!(cancel_resp.handle, "h-cancel-1");
        assert_eq!(cancel_resp.status, "cancel_requested");
        assert!(cancel_resp.acknowledged);
    }

    #[test]
    fn test_dns_worker_pool_concurrent_resolutions_and_ssrf_rejection() {
        // Loopback / non-public rejected fail-closed
        let err = resolve_and_validate_host("127.0.0.1", 443, Duration::from_secs(2)).unwrap_err();
        assert_eq!(err, HttpTransportError::NonPublicIpRejected);

        // Multiple concurrent resolutions across workers
        let mut handles = Vec::new();
        for _ in 0..8 {
            handles.push(thread::spawn(|| {
                // Loopback IP resolution fails closed consistently
                let res = resolve_and_validate_host("127.0.0.1", 443, Duration::from_secs(2));
                assert_eq!(res, Err(HttpTransportError::NonPublicIpRejected));
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    /// Manual live transport probe. The caller supplies secrets only in the
    /// child process environment; this test prints no credential material.
    #[test]
    #[ignore = "requires MC_LIVE_MODAL_ENDPOINT, MC_LIVE_MODAL_TOKEN_ID and MC_LIVE_MODAL_TOKEN_SECRET"]
    fn live_modal_native_control_and_analysis_capabilities() {
        let endpoint = std::env::var("MC_LIVE_MODAL_ENDPOINT").expect("live endpoint");
        let token_id = std::env::var("MC_LIVE_MODAL_TOKEN_ID").expect("live token id");
        let token_secret = std::env::var("MC_LIVE_MODAL_TOKEN_SECRET").expect("live token secret");
        let target = CloudEndpointTarget::new(CloudProvider::Modal, "mc-live-probe", &endpoint)
            .expect("valid live Modal endpoint");
        let credential = BoundRuntimeCredential::new(
            &target,
            RuntimeCredential::ModalProxy {
                token_id,
                token_secret: SecretValue::new(&token_secret),
            },
        ).expect("bound Modal credential");
        let client = CloudHttpClient::new(target, credential).expect("native transport");
        let health = client.get_health().expect("authenticated health");
        assert_eq!(health.status, "ok");
        let model = client.get_model_info().expect("pinned model info");
        assert!(model.model_id.contains("FLUX.2-klein"));
        let analysis = client.get_analysis_capabilities().expect("analysis capabilities");
        assert!(analysis.capabilities.iter().any(|item| item.capability == "text_mask_sam_ts@1"));
        assert!(analysis.capabilities.iter().any(|item| item.capability == "text_regions_rt@1"));
        let rt = analysis.capabilities.iter().find(|item| item.capability == "text_regions_rt@1")
            .expect("RT capability");
        let tile_png = include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");
        let mut request = AnalysisRequest {
            protocol_version: cleaner_core::cloud_analysis_wire::VERSION.into(),
            capability: rt.capability.clone(),
            graph_sha256s: rt.graph_sha256s.clone(),
            model_revision: rt.model_revision.clone(),
            tile_id: "native-rt-probe".into(),
            tile_rect: cleaner_core::cloud_analysis_wire::TileRect {
                x: 0, y: 0, width: 16, height: 16,
            },
            tile_png_sha256: format!("{:x}", sha2::Sha256::digest(tile_png)),
            source_page_sha256: format!("{:x}", sha2::Sha256::digest(tile_png)),
            page_width: 16, page_height: 16,
            tile_core: cleaner_core::cloud_analysis_wire::TileRect { x: 0, y: 0, width: 16, height: 16 },
            request_digest: String::new(),
        };
        request.request_digest = request.digest().expect("request digest");
        let result = client.submit_analysis_tile(&request, tile_png).expect("native RT tile");
        assert_eq!(result.capability, rt.capability);
        println!("native Modal control passed: {} and {} analysis capabilities",
            model.model_id, analysis.capabilities.len());
    }

    /// A loopback gateway for the job routes. Each connection carries one
    /// request, answered by `answer(method, path, body)` with a status and a
    /// JSON body, or dropped unanswered on `None`. Every request is logged.
    type RequestLog = std::sync::Arc<std::sync::Mutex<Vec<(String, String, Vec<u8>)>>>;

    fn job_gateway(
        answer: impl Fn(&str, &str, &[u8]) -> Option<(u16, String)> + Send + 'static,
    ) -> (String, RequestLog) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&log);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut raw = Vec::new();
                let mut chunk = [0u8; 65536];
                let (head, length) = loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 { break (String::new(), 0) }
                    raw.extend_from_slice(&chunk[..read]);
                    if let Some(end) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..end]).to_string();
                        let length = head.lines().find_map(|line| line.to_ascii_lowercase()
                            .strip_prefix("content-length:").map(|value| value.trim().parse::<usize>().unwrap()))
                            .unwrap_or(0);
                        raw.drain(..end + 4);
                        break (head, length);
                    }
                };
                while raw.len() < length {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 { break }
                    raw.extend_from_slice(&chunk[..read]);
                }
                let mut words = head.split_whitespace();
                let (method, path) = (words.next().unwrap_or("").to_string(), words.next().unwrap_or("").to_string());
                seen.lock().unwrap().push((method.clone(), path.clone(), raw.clone()));
                if let Some((status, body)) = answer(&method, &path, &raw) {
                    let location = if status == 303 { "Location: https://example.invalid/result\r\n" } else { "" };
                    let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                }
            }
        });
        (format!("http://{address}/mc/v1"), log)
    }

    fn modal_client(base: &str) -> CloudHttpClient {
        let target = CloudEndpointTarget::new_test_target(CloudProvider::Modal, "modal-1", base).unwrap();
        let credential = BoundRuntimeCredential::new(&target, RuntimeCredential::ModalProxy {
            token_id: "tid".into(), token_secret: SecretValue::new("tsec"),
        }).unwrap();
        CloudHttpClient::new_test_client(target, credential,
            Client::builder().redirect(Policy::none()).build().unwrap())
    }

    fn denoise_request() -> (DenoiseMetadata, &'static [u8]) {
        let page: &'static [u8] = include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");
        let recipe = cleaner_core::cloud_denoise_wire::preset("realcugan-2x-conservative").unwrap().recipe();
        (DenoiseMetadata::new(recipe, page).unwrap(), page)
    }

    fn capabilities_with(operations: &[&str]) -> String {
        let listed: Vec<&str> = ["jobs.submit", "jobs.status", "jobs.result", "jobs.cancel"].iter()
            .chain(operations).copied().collect();
        serde_json::json!({"protocol_version": "1.0.0", "provider": "modal", "operations": listed}).to_string()
    }

    fn job_answer(digest: &str, handle: &str, status: &str, result: serde_json::Value) -> String {
        serde_json::json!({"protocol_version": "1.0.0", "request_digest": digest, "handle": handle,
            "status": status, "error": null, "result": result}).to_string()
    }

    /// A page through the job routes: the first submit's answer is lost, the
    /// repeat carries the same attempt (so the gateway names the same job), the
    /// page is polled while it runs and collected once it completes.
    #[test]
    fn a_denoise_page_is_submitted_again_after_a_lost_answer_then_polled() {
        let (metadata, page) = denoise_request();
        let digest = metadata.request_digest.clone();
        let submits = std::sync::atomic::AtomicUsize::new(0);
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let answer_digest = digest.clone();
        let (base, log) = job_gateway(move |method, path, body| {
            let digest = answer_digest.as_str();
            match (method, path) {
                ("GET", "/mc/v1/capabilities") => Some((200, capabilities_with(&["denoise.jobs"]))),
                ("POST", "/mc/denoise/v1/jobs") => {
                    if submits.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 { return None; }
                    let sent: serde_json::Value = serde_json::from_slice(body).unwrap();
                    let handle = job_wire::job_handle(JobKind::Denoise, digest, sent["attempt_id"].as_str().unwrap());
                    Some((202, job_answer(digest, &handle, "running", serde_json::Value::Null)))
                }
                ("GET", path) if path.starts_with("/mc/denoise/v1/jobs/denoise-") => {
                    let handle = path.rsplit('/').next().unwrap();
                    if polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        return Some((200, job_answer(digest, handle, "running", serde_json::Value::Null)));
                    }
                    Some((200, job_answer(digest, handle, "completed", serde_json::json!({
                        "protocol_version": "1.0.0", "request_digest": digest, "page_png_b64": "",
                        "info": {"width": 16, "height": 16, "gray": false, "steps": []}}))))
                }
                _ => Some((404, "{}".into())),
            }
        });
        let client = modal_client(&base);
        let result = client.denoise_page(&metadata, page, &AtomicBool::new(false)).unwrap();
        assert_eq!(result.request_digest, digest);
        let log = log.lock().unwrap();
        let submitted: Vec<&Vec<u8>> = log.iter().filter(|(m, p, _)| m == "POST" && p == "/mc/denoise/v1/jobs")
            .map(|(_, _, body)| body).collect();
        assert_eq!(submitted.len(), 2);
        assert_eq!(submitted[0], submitted[1], "the repeated submit must name the same attempt");
        assert_eq!(log.iter().filter(|(m, _, _)| m == "GET").count(), 3, "capabilities once, two polls");
        assert!(!log.iter().any(|(_, path, _)| path.ends_with("/cancel") || path == "/mc/denoise/v1/page"));
    }

    /// A batch goes to its own route with each tile PNG once, is polled on the
    /// job envelope and comes back as one validated result per request.
    #[test]
    fn an_analysis_batch_sends_each_tile_once_and_answers_every_request() {
        use cleaner_core::cloud_analysis_wire::{AnalysisBatch, TileRect, RT, SAM, VERSION};
        let tile: &'static [u8] = include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");
        let rect = TileRect { x: 0, y: 0, width: 16, height: 16 };
        let request = |capability: &str| {
            let mut request = AnalysisRequest {
                protocol_version: VERSION.into(), capability: capability.into(),
                graph_sha256s: vec!["a".repeat(64); if capability == SAM { 2 } else { 1 }],
                model_revision: "c".repeat(40), tile_id: "tile_00".into(),
                tile_rect: rect, tile_png_sha256: format!("{:x}", sha2::Sha256::digest(tile)),
                source_page_sha256: "e".repeat(64), page_width: 16, page_height: 16, tile_core: rect,
                request_digest: String::new(),
            };
            request.request_digest = request.digest().unwrap();
            request
        };
        let batch = AnalysisBatch::new(vec![request(SAM), request(RT)]);
        let mut mask = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut mask, 16, 16);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.write_header().unwrap().write_image_data(&[255; 256]).unwrap();
        }
        let answer = |request: &AnalysisRequest, mask: Option<&[u8]>| serde_json::json!({
            "protocol_version": VERSION, "capability": request.capability, "request_digest": request.request_digest,
            "tile_id": request.tile_id, "tile_rect": request.tile_rect, "graph_sha256s": request.graph_sha256s,
            "model_revision": request.model_revision,
            "mask_png_b64": mask.map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
            "components": [], "boxes": [],
            "timings": {"load_ms": 0, "preprocess_ms": 0, "inference_ms": 0, "postprocess_ms": 0},
            "reported_cost_usd": null});
        let results = serde_json::json!({"protocol_version": VERSION, "request_digest": batch.request_digest,
            "results": [answer(&batch.requests[0], Some(&mask)), answer(&batch.requests[1], None)]});
        let digest = batch.request_digest.clone();
        let job = |digest: &str, handle: &str, status: &str, result: serde_json::Value| serde_json::json!({
            "protocol_version": VERSION, "request_digest": digest, "handle": handle, "status": status,
            "error": null, "result": result}).to_string();
        let (base, log) = job_gateway(move |method, path, body| match (method, path) {
            ("GET", "/mc/v1/capabilities") => Some((200, capabilities_with(&["analysis.jobs", "analysis.batches"]))),
            ("POST", "/mc/analysis/v1/batches") => {
                let sent: serde_json::Value = serde_json::from_slice(body).unwrap();
                let handle = job_wire::job_handle(JobKind::AnalysisBatch, &digest, sent["attempt_id"].as_str().unwrap());
                Some((202, job(&digest, &handle, "running", serde_json::Value::Null)))
            }
            ("GET", path) if path.starts_with("/mc/analysis/v1/batches/analysis_batch-") => {
                Some((200, job(&digest, path.rsplit('/').next().unwrap(), "completed", results.clone())))
            }
            _ => Some((404, "{}".into())),
        });
        let client = modal_client(&base);
        assert!(client.serves_gpu_jobs(JobKind::AnalysisBatch).unwrap());
        let result = client.submit_analysis_batch(&batch, &[tile], &AtomicBool::new(false)).unwrap();
        assert_eq!(result.results.len(), 2);
        let log = log.lock().unwrap();
        let sent: Vec<serde_json::Value> = log.iter()
            .filter(|(method, path, _)| method == "POST" && path == "/mc/analysis/v1/batches")
            .map(|(_, _, body)| serde_json::from_slice(body).unwrap()).collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["tiles_png_b64"].as_array().unwrap().len(), 1, "one PNG for both models");
        assert_eq!(sent[0]["metadata"]["requests"].as_array().unwrap().len(), 2);
        assert!(!log.iter().any(|(_, path, _)| path.starts_with("/mc/analysis/v1/jobs")));
    }

    /// A redirect on a status read is refused as everywhere in this client, and
    /// the job is cancelled on the gateway rather than left running.
    #[test]
    fn a_redirected_poll_is_refused_and_the_job_cancelled() {
        let (metadata, page) = denoise_request();
        let digest = metadata.request_digest.clone();
        let (base, log) = job_gateway(move |method, path, body| match (method, path) {
            ("GET", "/mc/v1/capabilities") => Some((200, capabilities_with(&["denoise.jobs"]))),
            ("POST", "/mc/denoise/v1/jobs") => {
                let sent: serde_json::Value = serde_json::from_slice(body).unwrap();
                let handle = job_wire::job_handle(JobKind::Denoise, &digest, sent["attempt_id"].as_str().unwrap());
                Some((202, job_answer(&digest, &handle, "running", serde_json::Value::Null)))
            }
            ("GET", _) => Some((303, "{}".into())),
            ("POST", path) if path.ends_with("/cancel") => {
                let handle = path.split('/').nth(5).unwrap().to_string();
                Some((200, job_answer(&digest, &handle, "cancelled", serde_json::Value::Null)))
            }
            _ => Some((404, "{}".into())),
        });
        let client = modal_client(&base);
        assert_eq!(client.denoise_page(&metadata, page, &AtomicBool::new(false)).unwrap_err(),
            HttpTransportError::RedirectForbidden);
        assert!(log.lock().unwrap().iter().any(|(m, p, _)| m == "POST" && p.ends_with("/cancel")));
    }

    /// A gateway that does not list the job operation gets the synchronous
    /// route, as before, and the capability read is made once per client.
    #[test]
    fn an_older_gateway_keeps_the_synchronous_route() {
        let (metadata, page) = denoise_request();
        let digest = metadata.request_digest.clone();
        let (base, log) = job_gateway(move |method, path, _| match (method, path) {
            ("GET", "/mc/v1/capabilities") => Some((200, capabilities_with(&[]))),
            ("POST", "/mc/denoise/v1/page") => Some((200, serde_json::json!({
                "protocol_version": "1.0.0", "request_digest": digest, "page_png_b64": "",
                "info": {"width": 16, "height": 16, "gray": false, "steps": []}}).to_string())),
            _ => Some((404, "{}".into())),
        });
        let client = modal_client(&base);
        client.denoise_page(&metadata, page, &AtomicBool::new(false)).unwrap();
        client.denoise_page(&metadata, page, &AtomicBool::new(false)).unwrap();
        let paths: Vec<String> = log.lock().unwrap().iter().map(|(_, p, _)| p.clone()).collect();
        assert_eq!(paths, ["/mc/v1/capabilities", "/mc/denoise/v1/page", "/mc/denoise/v1/page"]);
        assert!(!client.serves_gpu_jobs(JobKind::Analysis).unwrap());
    }

    /// A result server that answers each connection in turn with one scripted
    /// reply, and reports the `Range` header each request carried.
    fn result_server(replies: Vec<Vec<u8>>) -> (u16, thread::JoinHandle<Vec<Option<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || replies.into_iter().map(|reply| {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
            }
            stream.write_all(&reply).unwrap();
            String::from_utf8_lossy(&request).lines().find_map(|line| {
                line.to_ascii_lowercase().strip_prefix("range: ").map(str::to_owned)
            })
        }).collect());
        (port, server)
    }

    #[test]
    fn result_download_resumes_where_a_dropped_transfer_stopped() {
        let request: JobRequestMetadata = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/job_request_metadata.json")).unwrap();
        let png: &[u8] = include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");
        let handle = "handle-test-modal-999";
        let meta = ResultMetadata {
            handle: handle.into(),
            job_id: request.job_id.clone(),
            attempt_id: request.attempt_id.clone(),
            request_digest: request.request_digest.clone(),
            recipe_id: request.recipe.recipe_id.clone(),
            preprocessing_version: request.recipe.preprocessing_version.clone(),
            model_id: request.recipe.model_id.clone(),
            model_revision: request.recipe.model_revision.clone(),
            native_mask_conditioning: request.recipe.native_mask_conditioning,
            result_digest: request.image_sha256.clone(),
            reported_cost_usd: None,
            width: request.width,
            height: request.height,
            byte_length: png.len() as u64,
        };
        let limits = cleaner_core::cloud_wire::provisional_fixture_limits();
        let cut = png.len() / 2;
        let head = |status: &str, extra: &str| format!(
            "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nConnection: close\r\n{extra}\r\n").into_bytes();
        // Promises the whole result, then closes after a part of it.
        let dropped = [head("200 OK", &format!("Content-Length: {}\r\n", png.len())), png[..cut].to_vec()].concat();
        let rest = [head("206 Partial Content", &format!("Content-Length: {}\r\nContent-Range: bytes {cut}-{}/{}\r\n",
            png.len() - cut, png.len() - 1, png.len())), png[cut..].to_vec()].concat();
        let whole = [head("200 OK", &format!("Content-Length: {}\r\n", png.len())), png.to_vec()].concat();
        let fetch = |replies: Vec<Vec<u8>>, keep_going: &dyn Fn() -> bool| {
            let (port, server) = result_server(replies);
            let target = CloudEndpointTarget::new_test_target(
                CloudProvider::Beam, "beam-result", &format!("http://127.0.0.1:{port}/mc/v1")).unwrap();
            let credential = BoundRuntimeCredential::new(
                &target, RuntimeCredential::BeamBearer(SecretValue::new("test-token"))).unwrap();
            let client = CloudHttpClient::new_test_client(target, credential, Client::new());
            let fetched = client.fetch_result_bytes_while(handle, &meta, &request, &limits, keep_going);
            (fetched, server)
        };

        // The second request asks for the missing bytes only, and the parts join.
        let (fetched, server) = fetch(vec![dropped.clone(), rest.clone()], &|| true);
        assert_eq!(fetched.unwrap(), png);
        assert_eq!(server.join().unwrap(), [None, Some(format!("bytes={cut}-"))]);

        // A gateway without ranges sends the whole result again, which replaces the part.
        let (fetched, server) = fetch(vec![dropped.clone(), whole], &|| true);
        assert_eq!(fetched.unwrap(), png);
        server.join().unwrap();

        // A part that does not start where the download stopped is refused.
        let wrong = [head("206 Partial Content", &format!("Content-Length: {}\r\nContent-Range: bytes 0-{}/{}\r\n",
            png.len(), png.len() - 1, png.len())), png.to_vec()].concat();
        let (fetched, server) = fetch(vec![dropped.clone(), wrong], &|| true);
        assert_eq!(fetched.unwrap_err(), HttpTransportError::WireValidation);
        server.join().unwrap();

        // Requests that add nothing end the download; a cancel ends it at once.
        let empty = head("200 OK", &format!("Content-Length: {}\r\n", png.len()));
        let (fetched, server) = fetch(vec![empty.clone(); RESULT_STALLED_TRIES as usize], &|| true);
        assert_eq!(fetched.unwrap_err(), HttpTransportError::ConnectionError);
        assert_eq!(server.join().unwrap().len(), RESULT_STALLED_TRIES as usize);
        let (fetched, server) = fetch(vec![dropped], &|| false);
        assert_eq!(fetched.unwrap_err(), HttpTransportError::ConnectionError);
        server.join().unwrap();
    }
}
