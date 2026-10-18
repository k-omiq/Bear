//! Coherent cloud inference service coordinating consent, credentials, HTTP transport,
//! attempt durability, and project attachment (P3/P4).
//!
//! Provides the top-level orchestration layer for user-owned cloud diffusion rendering,
//! ensuring strict backend gating, honest nullable cost provenance, tamper-evident authorization,
//! non-terminal cancellation, and transactional crash recovery.
//!
//! ## Invariants
//!
//! 1. **Strict Backend Gating:** No network dispatch or GPU inference is initiated unless
//!    `cloudEngines == "allowed"` in application settings and a valid backend [`Grant`] is consumed.
//! 2. **Durable Intent & Dispatching:** Every remote operation persists its intent and dispatching
//!    phase to the transactional journal before issuing an HTTP request.
//! 3. **Ambiguous Acceptance Never Auto-Retried:** Transport failures or process interruptions
//!    during submission recover strictly as `Unknown` and forbid automatic resubmission.
//! 4. **Known Handle Reuse:** Polling or download network faults preserve the durable remote handle
//!    for resume and never spawn a duplicate billable job.
//! 5. **Non-Terminal Cancellation Acknowledgement:** `CancelRequested` remains non-terminal until
//!    confirmed as cancelled or completed by authoritative gateway status.
//! 6. **Mandatory Result Decode Validation:** Output bytes are validated via full
//!    [`cleaner_core::cloud_decode::decode_result_crop`] before caching or project attachment.
//! 7. **Crash Discovery & Offline Recovery:** Cached outputs recover without resubmission.
//! 8. **Stale Revision Attachment Rejection:** Drifts in source pixels, region geometry, or mask
//!    hashes reject project attachment (`StaleAttachment`) while preserving cached results for inspection.
//!
//! ## Progress and cancellation
//!
//! A render reports its phases to an optional [`ProgressSink`] (the Tauri layer turns them into
//! `cloud://attempt` events) and ends with exactly one of `committed`, `failed`, `cancelled` or
//! `unknown`, the last three carrying [`InferenceServiceError::code`]. A render in progress is
//! registered by attempt id, so [`request_render_cancel`] reaches it before the journal has a
//! remote handle to cancel; once it has one, a `CancelRequested` written by another command
//! stops the wait as well.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use cleaner_core::cloud_decode::{DecodedResultCrop, ResultDecodeError};
use cleaner_core::cloud_wire::{
    compute_request_digest, JobExecutionStatus, JobRequestMetadata, JobStatusResponse,
    ResultMetadata, ServiceLimits, TypedError, WireRenderRecipe, WireValidationError,
    PROTOCOL_VERSION,
};
#[cfg(test)]
use cleaner_core::engines::flux::wire_sampling;
#[cfg(test)]
use cleaner_core::engines::render::PreparedRender;
use cleaner_core::engines::render::{CloudProvider, ExecutionTarget, Preprocessing, RenderRecipe};
#[cfg(test)]
use cleaner_core::fit::{self, EdgeMap};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::patch::{CloudRecord, Engine, Patch, Provenance};
use cleaner_core::project::Job;
use serde::Serialize;
use thiserror::Error;

use crate::inference::config::{compute_canonical_endpoint_fingerprint, InferenceConfig};
use crate::inference::consent::{
    compute_operation_digest, encode_gray8_png, encode_rgb8_png, load_cloud_region,
    CloudRegionKind, ConfirmProposalOptions, ConsentError, ConsentProposal, ConsentService,
    OperationIntent, PrepareProposalRequest,
};
use crate::inference::http::{
    BoundRuntimeCredential, CloudEndpointTarget, CloudHttpClient, HttpTransportError,
    RuntimeCredential, RETRY_SEED_OPERATION,
};
use crate::inference::journal::{
    AttemptJournal, AttemptLockGuard, AttemptPhase, AttemptRecord, CreateAttemptIntent,
    JournalError, ProjectRegionSnapshot, RecoveryDecision,
};
use crate::inference::policy::{Grant, GrantError, GrantScope, GrantService};
use crate::inference::secrets::{
    decode_modal_runtime_secret, SecretKey, SecretManager, SecretRole,
};
use crate::library::{ApiRegion, Library};
use crate::run;

/// Errors arising from inference service operations.
#[derive(Debug, Error)]
pub enum InferenceServiceError {
    #[error("cloud execution is disabled in application settings")]
    CloudDisabled,

    #[error("selected cloud profile changed before dispatch")]
    ProfileChanged,

    #[error(
        "local execution target requested; cloud inference service only handles remote providers"
    )]
    LocalTargetNotAllowed,

    #[error("profile '{0}' not found for provider {1:?}")]
    ProfileNotFound(String, CloudProvider),

    #[error("cloud credential missing for profile '{0}'")]
    CredentialMissing(String),

    #[error("grant error: {0}")]
    Grant(#[from] GrantError),

    #[error("consent error: {0}")]
    Consent(#[from] ConsentError),

    #[error("attempt journal error: {0}")]
    Journal(#[from] JournalError),

    #[error("HTTP transport error: {0}")]
    Http(#[from] HttpTransportError),

    /// The submit request failed after the attempt was marked dispatching, so
    /// the gateway may or may not hold the job. The journal says `Unknown`.
    #[error("job submission outcome unknown: {0}")]
    AmbiguousSubmission(HttpTransportError),

    #[error("wire contract validation error: {0}")]
    Wire(#[from] WireValidationError),

    #[error("result decode error: {0}")]
    Decode(#[from] ResultDecodeError),

    #[error("remote job failed with error '{error_code}': {message}")]
    RemoteJobFailed { error_code: String, message: String },

    #[error("remote job was cancelled (handle '{0}')")]
    RemoteJobCancelled(String),

    /// The user cancelled the render and it stopped waiting.
    #[error("cloud render was cancelled")]
    Cancelled,

    #[error("polling timeout exceeded for job handle '{0}'")]
    PollingTimeout(String),

    #[error("stale project attachment: {0}")]
    StaleAttachment(String),

    /// A run on this computer is working on the page the result is for
    /// ([`run::page_run`]). The result stays cached, as after any attachment
    /// that did not happen.
    #[error("a run is working on the page this result is for")]
    PageBusy,

    #[error("job manifest error: {0}")]
    JobManifest(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl InferenceServiceError {
    /// The stable snake_case code the interface maps to a sentence.
    ///
    /// The error's own text never crosses to the webview: it can carry a
    /// path, a remote handle or words a gateway chose.
    pub fn code(&self) -> &'static str {
        match self {
            Self::CloudDisabled => "cloud_disabled",
            Self::ProfileChanged => "cloud_run_profile_changed",
            Self::LocalTargetNotAllowed => "target_invalid",
            Self::ProfileNotFound(..) => "profile_missing",
            Self::CredentialMissing(_) => "credential_missing",
            Self::Grant(GrantError::ScopeMismatch { field: "input_sha256" | "predecessors_sha256" }) => "region_changed",
            Self::Grant(_) => "consent_invalid",
            Self::Consent(err) => consent_code(err),
            Self::Journal(err) => journal_code(err),
            Self::Http(err) => http_code(err),
            // A gateway that answered with a status said why; one that never
            // answered leaves only the fact that nobody knows.
            Self::AmbiguousSubmission(
                HttpTransportError::ConnectionError | HttpTransportError::Timeout,
            ) => "submission_unknown",
            Self::AmbiguousSubmission(err) => http_code(err),
            Self::Wire(_) => "gateway_protocol",
            Self::Decode(_) => "result_invalid",
            Self::RemoteJobFailed { error_code, .. } if matches!(error_code.as_str(), "weights_missing" | "weights_corrupt") => "weights_unavailable",
            Self::RemoteJobFailed { error_code, .. } if error_code == "edit_not_applied" => "edit_not_applied",
            Self::RemoteJobFailed { .. } => "remote_failed",
            Self::RemoteJobCancelled(_) => "remote_cancelled",
            Self::Cancelled => "cancelled",
            Self::PollingTimeout(_) => "poll_timeout",
            Self::StaleAttachment(_) => "region_changed",
            Self::PageBusy => "page_busy",
            Self::JobManifest(_) | Self::Io(_) => "project_error",
        }
    }

    /// The phase a render that failed with this error ends in.
    pub fn phase(&self) -> RenderPhase {
        match self {
            Self::Cancelled | Self::RemoteJobCancelled(_) => RenderPhase::Cancelled,
            Self::AmbiguousSubmission(_) => RenderPhase::Unknown,
            _ => RenderPhase::Failed,
        }
    }
}

fn consent_code(err: &ConsentError) -> &'static str {
    match err {
        ConsentError::CloudDisabled => "cloud_disabled",
        ConsentError::LocalTargetNotAllowed => "target_invalid",
        ConsentError::ProfileNotFound(..) => "profile_missing",
        ConsentError::InvalidEndpoint(_) => "endpoint_invalid",
        ConsentError::RegionNotFound(_)
        | ConsentError::PageNotFound(..)
        | ConsentError::PageMismatch => "region_not_found",
        ConsentError::SourceImageError
        | ConsentError::RenderPrepareFailed(_)
        | ConsentError::CropTooLarge
        | ConsentError::UnsupportedNewRegion(_) => "region_unsupported",
        ConsentError::RevisionMismatch
        | ConsentError::SourceMismatch
        | ConsentError::MaskMismatch
        | ConsentError::CropDigestMismatch => "region_changed",
        // The profile or the endpoint moved on between consent and dispatch.
        ConsentError::ConfigMismatch | ConsentError::ProfileMutated => "target_changed",
        _ => "consent_invalid",
    }
}

fn journal_code(err: &JournalError) -> &'static str {
    match err {
        JournalError::AttemptLocked { .. } => "attempt_busy",
        JournalError::RegionUnresolved { .. } => "recovery_required",
        JournalError::AttemptAlreadyExists { .. } | JournalError::DuplicateCommit { .. } => {
            "attempt_exists"
        }
        JournalError::StaleAttachment { .. } => "region_changed",
        JournalError::Wire(_)
        | JournalError::Decode(_)
        | JournalError::ResultDigestMismatch { .. }
        | JournalError::LimitExceeded { .. } => "result_invalid",
        _ => "journal_error",
    }
}

fn http_code(err: &HttpTransportError) -> &'static str {
    match err {
        HttpTransportError::DenoiseStorage => "journal_unavailable",
        HttpTransportError::NotEnqueued { reason, .. } => http_code(reason),
        // No cleaning model on the deployment reads as its weights not being there.
        HttpTransportError::RenderWeightsUnavailable | HttpTransportError::RenderNotConfigured => "weights_unavailable",
        HttpTransportError::ProviderPaused => "provider_paused",
        HttpTransportError::UnexpectedStatus { status: 401 | 403 } => "gateway_unauthorized",
        HttpTransportError::UnexpectedStatus { .. } => "gateway_error",
        HttpTransportError::ConnectionError
        | HttpTransportError::Timeout
        | HttpTransportError::DnsResolutionFailed
        | HttpTransportError::DnsResolverBusy => "gateway_unreachable",
        HttpTransportError::InvalidCredential | HttpTransportError::CredentialProviderMismatch => {
            "credential_missing"
        }
        HttpTransportError::InvalidTarget
        | HttpTransportError::InvalidProfileId
        | HttpTransportError::EndpointValidationFailed
        | HttpTransportError::NonPublicIpRejected
        | HttpTransportError::RedirectForbidden => "endpoint_invalid",
        HttpTransportError::ContentTypeMismatch
        | HttpTransportError::BodySizeLimitExceeded
        | HttpTransportError::WireValidation
        | HttpTransportError::JsonDeserialization
        | HttpTransportError::ProviderMismatch
        | HttpTransportError::InvalidHandle => "gateway_protocol",
        HttpTransportError::JobFailed { .. } => "gateway_error",
        HttpTransportError::JobCancelled => "cancelled",
        HttpTransportError::JobCancelUnconfirmed | HttpTransportError::JobDeadline => "gateway_unreachable",
    }
}

/// Polling configuration for status retrieval.
#[derive(Debug, Clone)]
pub struct PollOptions {
    pub poll_interval: Duration,
    pub timeout: Duration,
    pub max_polls: usize,
}

impl Default for PollOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            timeout: Duration::from_secs(120),
            max_polls: 240,
        }
    }
}

impl PollOptions {
    /// A render the user is watching: one status request a second for up to
    /// fifteen minutes. A cold start pays for a container, an image pull and a
    /// 5.5 GB weight load before the first step runs, so the bound is sized to
    /// that and not to the seconds a warm render takes.
    pub fn interactive() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            timeout: Duration::from_secs(15 * 60),
            max_polls: 15 * 60,
        }
    }
}

/// One phase of a cloud render, as the `cloud://attempt` event names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderPhase {
    Preparing,
    Submitting,
    Queued,
    Running,
    Downloading,
    Compositing,
    Committed,
    Failed,
    Cancelled,
    Unknown,
}

/// The `cloud://attempt` payload. `errorCode` is null until a render ends
/// anywhere but `committed`, and is then one of [`InferenceServiceError::code`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAttemptProgress {
    pub attempt_id: String,
    pub provider: Option<CloudProvider>,
    pub profile_id: Option<String>,
    pub region_id: String,
    pub chapter_id: String,
    pub page_index: u32,
    pub phase: RenderPhase,
    pub elapsed_ms: u64,
    pub error_code: Option<&'static str>,
}

/// Where a render's phases go: the Tauri layer emits them as events, tests
/// collect them.
pub type ProgressSink = Arc<dyn Fn(&CloudAttemptProgress) + Send + Sync>;

/// How often a waiting render looks at its cancel flag between polls.
const CANCEL_CHECK_INTERVAL: Duration = Duration::from_millis(50);
/// The gateway's window to confirm a cancel before the render stops waiting
/// for it. An unconfirmed cancel stays `CancelRequested` for recovery.
const CANCEL_CONFIRM_POLLS: u32 = 10;
/// The journal's code and message for a cached result that fails validation.
const RESULT_CORRUPT: (&str, &str) = ("result_corrupt", "the cached result failed validation");
/// How many polls in a row must answer "no such job" before a resumed wait
/// ends its attempt as failed.
const UNKNOWN_JOB_POLLS: u32 = 3;
const CANCEL_CONFIRM_INTERVAL: Duration = Duration::from_millis(200);
/// How long a journal write waits out a lock that another command holds for
/// the length of one write of its own.
const LOCK_RETRIES: u32 = 50;
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(20);

#[cfg(test)]
fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The attempt a grant nonce names: `att-` and the first 24 hex digits of its
/// SHA-256. Derived rather than minted so the interface can name the attempt
/// it just consented to without the nonce being stored anywhere.
pub fn attempt_id_for_nonce(grant_nonce: &str) -> String {
    format!("att-{}", &sha256_hex(grant_nonce.as_bytes())[0..24])
}

/// The chapter a job is. A library job lives at `<project>/<chapter>.mtclean`
/// ([`crate::library::Library::job_path`]), so the id is the file's stem and
/// not its folder's name, which is the project's.
/// Why a cloud result is flagged for review when its region carries no
/// reason of its own. A Qwen result always is: it is applied with nobody
/// asked first, and the flag is where the user finds it to keep or reject it.
/// Any other model is flagged where it generated texture unmasked, on a
/// region too `complex` to fit.
fn generated_review(recipe: &RenderRecipe, complex: bool) -> Option<String> {
    let qwen = recipe.model_id.starts_with(crate::inference::cloud_clean::QWEN_MODEL_PREFIX);
    (qwen || (complex && !recipe.native_mask_conditioning))
        .then(|| "review.reason.checkGeneratedTexture".to_owned())
}

fn chapter_id_of(job_path: &Path) -> String {
    job_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Renders and resumed waits running in this process, by attempt id, each
/// with the flag its poll loop watches.
fn live_renders() -> MutexGuard<'static, HashMap<String, Arc<AtomicBool>>> {
    static LIVE: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Ask a render running in this process to stop. Answers whether one was.
///
/// Until the gateway has accepted the job there is no handle for the journal
/// to cancel, so this flag is the only way to reach the render then; it acts
/// on it at its next safe point, and never after the result is downloaded.
pub fn request_render_cancel(attempt_id: &str) -> bool {
    match live_renders().get(attempt_id) {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

/// Whether a render or a resumed wait on this attempt is running in this
/// process. Recovery leaves such an attempt alone: its `Dispatching` is a
/// request in flight, not an interrupted one.
pub fn render_cancelled(attempt_id: &str) -> bool {
    live_renders().get(attempt_id).is_some_and(|flag| flag.load(Ordering::SeqCst))
}

pub fn is_render_live(attempt_id: &str) -> bool {
    live_renders().contains_key(attempt_id)
}

/// This process's claim on one attempt for the length of a render. Dropping
/// it takes the attempt out of reach of [`request_render_cancel`].
struct LiveRender {
    attempt_id: String,
    cancelled: Arc<AtomicBool>,
}

impl LiveRender {
    /// `None` when the attempt is already being rendered or waited on here: a
    /// second loop over one handle would only race the first to the journal.
    fn register(attempt_id: &str) -> Option<Self> {
        let mut live = live_renders();
        if live.contains_key(attempt_id) {
            return None;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        live.insert(attempt_id.to_string(), Arc::clone(&cancelled));
        Some(Self {
            attempt_id: attempt_id.to_string(),
            cancelled,
        })
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Sleep for `total`, waking early once a cancel arrives.
    fn wait(&self, total: Duration) {
        let deadline = Instant::now() + total;
        while !self.cancelled() {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            std::thread::sleep((deadline - now).min(CANCEL_CHECK_INTERVAL));
        }
    }
}

impl Drop for LiveRender {
    fn drop(&mut self) {
        live_renders().remove(&self.attempt_id);
    }
}

/// Reports one render's phases to the sink, each change once.
struct PhaseReporter<'a> {
    sink: Option<&'a ProgressSink>,
    template: CloudAttemptProgress,
    started: Instant,
    last: Cell<Option<RenderPhase>>,
}

impl<'a> PhaseReporter<'a> {
    fn new(
        sink: Option<&'a ProgressSink>,
        attempt_id: &str,
        region_id: &str,
        chapter_id: &str,
        page_index: u32,
        provider: Option<CloudProvider>,
        profile_id: Option<&str>,
    ) -> Self {
        Self {
            sink,
            template: CloudAttemptProgress {
                attempt_id: attempt_id.to_string(),
                provider,
                profile_id: profile_id.map(str::to_owned),
                region_id: region_id.to_string(),
                chapter_id: chapter_id.to_string(),
                page_index,
                phase: RenderPhase::Preparing,
                elapsed_ms: 0,
                error_code: None,
            },
            started: Instant::now(),
            last: Cell::new(None),
        }
    }

    fn enter(&self, phase: RenderPhase) {
        self.report(phase, None);
    }

    /// The render's last event: `committed`, or where it stopped and why.
    fn settle<T>(&self, outcome: &Result<T, InferenceServiceError>) {
        match outcome {
            Ok(_) => self.report(RenderPhase::Committed, None),
            Err(err) => self.report(err.phase(), Some(err.code())),
        }
    }

    fn report(&self, phase: RenderPhase, error_code: Option<&'static str>) {
        if self.last.replace(Some(phase)) == Some(phase) {
            return;
        }
        if let Some(sink) = self.sink {
            sink(&CloudAttemptProgress {
                phase,
                elapsed_ms: self.started.elapsed().as_millis() as u64,
                error_code,
                ..self.template.clone()
            });
        }
    }
}

/// What a render of one stored region sends and is bound to, read from disk
/// under the job's lock by [`read_render_input`].
pub(crate) struct RenderInput {
    pub crop_bounds: cleaner_core::mask::Rect,
    pub crop_png: Vec<u8>,
    pub crop_sha256: String,
    pub hint_png: Vec<u8>,
    pub hint_sha256: String,
    pub source_hash: String,
    pub mask_hash: String,
    pub revision: u64,
    /// The full revision digest [`RenderInput::revision`] is the head of. A
    /// batch grant binds each region to this (`docs/detect-clean.md` §3).
    pub revision_hash: String,
    pub input_sha256: String,
    pub predecessors_sha256: String,
    pub source_idx: usize,
}

impl RenderInput {
    /// The exact [`GrantScope`] a render of this input spends. One spelling, so
    /// a grant minted for it ahead of the dispatch (a batch cloud clean's
    /// per-region grant) is the scope the dispatch then asks for.
    pub(crate) fn scope(
        &self,
        provider: CloudProvider,
        profile_id: &str,
        canonical_endpoint_fingerprint: &str,
        operation_digest: String,
        recipe: &RenderRecipe,
        region_id: &str,
    ) -> GrantScope {
        GrantScope {
            capability: crate::inference::policy::FLUX_CAPABILITY.to_string(),
            provider,
            profile_id: profile_id.to_string(),
            canonical_endpoint_fingerprint: canonical_endpoint_fingerprint.to_string(),
            source_hash: self.source_hash.clone(),
            crop_sha256: self.crop_sha256.clone(),
            hint_sha256: self.hint_sha256.clone(),
            operation_digest,
            crop_bounds: self.crop_bounds,
            mask_hash: self.mask_hash.clone(),
            revision: self.revision,
            input_sha256: self.input_sha256.clone(),
            predecessors_sha256: self.predecessors_sha256.clone(),
            recipe: recipe.clone(),
            region_ids: vec![region_id.to_string()],
        }
    }
}

/// Read one stored region's render input: the page `page_index` resolves to,
/// the region (a patch, or a stored detection) on it, and the crop, hint and
/// hashes a render of it sends. Nothing is written and nothing is sent.
pub(crate) fn read_render_input(
    job_path: &Path,
    page_index: u32,
    region_id: &str,
    recipe: &RenderRecipe,
) -> Result<RenderInput, InferenceServiceError> {
    let preprocessing = Preprocessing::of(&recipe.preprocessing_version).map_err(|e| {
        InferenceServiceError::Consent(ConsentError::InvalidRecipe(e.to_string()))
    })?;
    let _lock = run::lock_job(job_path).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
    let job = Job::open(job_path).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;

    let source_idx = Library::resolve_page(&job.project, page_index as usize).ok_or_else(|| {
        InferenceServiceError::Consent(ConsentError::PageNotFound(page_index, "unknown".into()))
    })?;

    let source_path = job
        .source_path(source_idx)
        .ok_or(InferenceServiceError::Consent(ConsentError::SourceImageError))?;
    let source_bytes = std::fs::read(&source_path)?;
    let source_hash = sha256_hex(&source_bytes);
    let page = job.display_page(source_idx,&source_bytes).map_err(|e| {
        InferenceServiceError::Consent(ConsentError::RenderPrepareFailed(e.to_string()))
    })?;

    let region = load_cloud_region(&job, region_id, &source_hash).map_err(|e| match e {
        ConsentError::Internal(detail) => InferenceServiceError::JobManifest(detail),
        other => InferenceServiceError::Consent(other),
    })?;
    if region.source_idx != source_idx {
        return Err(InferenceServiceError::Consent(ConsentError::PageMismatch));
    }
    if region.hole(preprocessing).is_empty() {
        return Err(InferenceServiceError::Consent(ConsentError::RenderPrepareFailed(
            "seed mask is empty".to_string(),
        )));
    }

    let cloud = region.prepare(&job, &page, preprocessing).map_err(|e| {
        InferenceServiceError::Consent(ConsentError::RenderPrepareFailed(e))
    })?;
    let prepared = &cloud.prepared;
    let crop_bounds = cloud.crop_bounds;
    let crop_png = encode_rgb8_png(crop_bounds.w, crop_bounds.h, prepared.image_rgb8())?;
    let hint_png = encode_gray8_png(crop_bounds.w, crop_bounds.h, prepared.hint_gray8())?;
    Ok(RenderInput {
        crop_bounds,
        crop_sha256: sha256_hex(&crop_png),
        crop_png,
        hint_sha256: sha256_hex(&hint_png),
        hint_png,
        source_hash,
        mask_hash: region.mask_digest(preprocessing),
        revision: region.revision,
        revision_hash: region.revision_hash,
        input_sha256: cloud.input.digest,
        predecessors_sha256: cloud.input.predecessors,
        source_idx,
    })
}

/// Immutable manifest snapshot shared by all committed audits in one chapter.
/// Source bytes are hashed once per source, outside the editor's writer lock.
pub(crate) struct RecoveryAuditSnapshot {
    job: Job,
    key: String,
    history: crate::history::Journal,
    source_hashes: Mutex<HashMap<usize, String>>,
}
impl RecoveryAuditSnapshot {
    pub(crate) fn load(path: &Path) -> Result<Self, InferenceServiceError> {
        let job = Job::open(path).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
        let history = crate::history::load(path);
        let mut state = serde_json::to_vec(&job.project).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
        state.extend(serde_json::to_vec(&history).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?);
        for index in 0..job.project.sources.len() {
            if let Some(path) = job.source_path(index) {
                let metadata = std::fs::metadata(path)?;
                state.extend_from_slice(format!("{}:{:?}", metadata.len(), metadata.modified().ok()).as_bytes());
            }
        }
        Ok(Self { job, key: sha256_hex(&state), history, source_hashes: Mutex::new(HashMap::new()) })
    }

    fn source_hash(&self, source_idx: usize) -> Result<String, InferenceServiceError> {
        let mut hashes = self.source_hashes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hash) = hashes.get(&source_idx) { return Ok(hash.clone()); }
        let path = self.job.source_path(source_idx).ok_or_else(|| InferenceServiceError::StaleAttachment("source missing".into()))?;
        let hash = sha256_hex(&std::fs::read(path)?);
        hashes.insert(source_idx, hash.clone());
        Ok(hash)
    }

    fn records_user_replacement(&self, record: &AttemptRecord) -> bool {
        fn names_attempt(value: &serde_json::Value, id: &str) -> bool {
            match value {
                serde_json::Value::Object(fields) => fields.iter().any(|(key, value)|
                    ((key == "attempt_id" || key == "attemptId") && value.as_str() == Some(id)) || names_attempt(value, id)),
                serde_json::Value::Array(values) => values.iter().any(|value| names_attempt(value, id)),
                _ => false,
            }
        }
        self.history.entries.iter().enumerate().any(|(index, entry)| {
            let side = if index < self.history.cursor { &entry.before } else { &entry.after };
            entry.region_id == record.region_id && side.region.as_ref().is_some_and(|value| names_attempt(value, &record.attempt_id))
        })
    }
}

/// Coherent service managing the lifecycle of cloud inference requests.
type DispatchGuard = Box<dyn Fn(CloudProvider, &str) -> bool + Send + Sync>;

pub struct InferenceService {
    journal: AttemptJournal,
    consent: Option<Arc<ConsentService>>,
    grants: Option<Arc<GrantService>>,
    secrets: Option<Arc<SecretManager>>,
    custom_client: Option<CloudHttpClient>,
    progress: Option<ProgressSink>,
    dispatch_guard: Option<DispatchGuard>,
    #[cfg(test)]
    attachment_fault: Option<&'static str>,
}

impl InferenceService {
    #[cfg(test)]
    fn fail_attachment_at(&self, boundary: &str) -> Result<(), InferenceServiceError> {
        if self.attachment_fault == Some(boundary) {
            return Err(std::io::Error::other(format!("injected crash after {boundary}")).into());
        }
        Ok(())
    }

    /// Create a new InferenceService backed by default global authorities.
    pub fn new(journal_dir: impl Into<PathBuf>, limits: ServiceLimits) -> Self {
        Self {
            journal: AttemptJournal::new(journal_dir, limits),
            consent: None,
            grants: None,
            secrets: None,
            custom_client: None,
            progress: None,
            dispatch_guard: None,
            #[cfg(test)]
            attachment_fault: None,
        }
    }

    /// Create an isolated InferenceService for testing.
    pub fn new_isolated(
        journal_dir: impl Into<PathBuf>,
        limits: ServiceLimits,
        consent: Arc<ConsentService>,
        grants: Arc<GrantService>,
        secrets: Arc<SecretManager>,
    ) -> Self {
        Self {
            journal: AttemptJournal::new(journal_dir, limits),
            consent: Some(consent),
            grants: Some(grants),
            secrets: Some(secrets),
            custom_client: None,
            progress: None,
            dispatch_guard: None,
            #[cfg(test)]
            attachment_fault: None,
        }
    }

    /// Reuse an authenticated client for one profile-bound batch, or inject
    /// a test transport. Its clones share their HTTP pool. Permission, grant
    /// scope, and live profile/epoch checks still run before every dispatch.
    pub fn with_custom_client(mut self, client: CloudHttpClient) -> Self {
        self.custom_client = Some(client);
        self
    }

    /// Report every render's phases to `sink`.
    pub fn with_progress(mut self, sink: ProgressSink) -> Self {
        self.progress = Some(sink);
        self
    }

    /// Live app selection check for the last gap between grant consumption
    /// and the HTTP submit. Isolated services can supply their own guard.
    pub fn with_dispatch_guard(mut self, guard: DispatchGuard) -> Self {
        self.dispatch_guard = Some(guard);
        self
    }

    fn dispatch_authorized(&self, provider: CloudProvider, profile_id: &str, epoch: u64)
        -> Result<(), InferenceServiceError> {
        if self.resolve_grants().get_profile_epoch(provider, profile_id) != epoch
            || self.dispatch_guard.as_ref().is_some_and(|guard| !guard(provider, profile_id)) {
            return Err(InferenceServiceError::ProfileChanged);
        }
        Ok(())
    }

    pub fn journal(&self) -> &AttemptJournal {
        &self.journal
    }

    fn resolve_consent(&self) -> &ConsentService {
        match &self.consent {
            Some(c) => c.as_ref(),
            None => ConsentService::global(),
        }
    }

    fn resolve_grants(&self) -> &GrantService {
        match &self.grants {
            Some(g) => g.as_ref(),
            None => GrantService::global(),
        }
    }

    fn resolve_secrets(&self) -> &SecretManager {
        match &self.secrets {
            Some(s) => s.as_ref(),
            None => SecretManager::global(),
        }
    }

    /// The attempt lock, waited for briefly.
    ///
    /// `acquire_lock` never blocks, and what a render can meet here (a cancel
    /// or status command) holds the lock for one journal write, so a short
    /// wait turns a spurious `AttemptLocked` into the write the render needed.
    /// A render that gave up instead could leave an accepted job recorded as
    /// dispatching, which recovery would then have to call unknown.
    fn lock_attempt(&self, attempt_id: &str) -> Result<AttemptLockGuard, JournalError> {
        let mut tries = 0;
        loop {
            match self.journal.acquire_lock(attempt_id) {
                Err(JournalError::AttemptLocked { .. }) if tries < LOCK_RETRIES => {
                    tries += 1;
                    std::thread::sleep(LOCK_RETRY_INTERVAL);
                }
                other => return other,
            }
        }
    }

    /// Step 1: Prepare an immutable authorization proposal from real disk assets.
    pub fn prepare_proposal(
        &self,
        request: PrepareProposalRequest,
        config: &InferenceConfig,
        cloud_allowed: bool,
        job_path: &Path,
    ) -> Result<ConsentProposal, InferenceServiceError> {
        self.resolve_consent()
            .prepare_proposal(request, config, cloud_allowed, job_path)
            .map_err(InferenceServiceError::Consent)
    }

    /// Step 2: Confirm a prepared proposal and issue a scoped authorization grant.
    pub fn confirm_proposal(
        &self,
        opts: ConfirmProposalOptions<'_>,
    ) -> Result<Grant, InferenceServiceError> {
        self.resolve_consent()
            .confirm_proposal(opts)
            .map_err(InferenceServiceError::Consent)
    }

    /// Build or resolve the HTTP transport client for a given provider and profile.
    pub fn build_http_client(
        &self,
        provider: CloudProvider,
        profile_id: &str,
        config: &InferenceConfig,
    ) -> Result<CloudHttpClient, InferenceServiceError> {
        if let Some(ref client) = self.custom_client {
            if client.target().provider() != provider || client.target().profile_id() != profile_id {
                return Err(InferenceServiceError::ProfileChanged);
            }
            return Ok(client.clone());
        }

        let profile = match provider {
            CloudProvider::Beam => config.beam_profiles.get(profile_id),
            CloudProvider::Modal => config.modal_profiles.get(profile_id),
        }
        .ok_or_else(|| InferenceServiceError::ProfileNotFound(profile_id.to_string(), provider))?;

        let target = CloudEndpointTarget::new(provider, profile_id, &profile.endpoint_url)
            .map_err(InferenceServiceError::Http)?;

        let sec_key = SecretKey::new(
            provider,
            profile_id.to_string(),
            profile.canonical_origin_fingerprint.clone(),
            SecretRole::Runtime,
        );

        let secret_val = self
            .resolve_secrets()
            .get_secret(&sec_key)
            .map_err(|_| InferenceServiceError::CredentialMissing(profile_id.to_string()))?
            .ok_or_else(|| InferenceServiceError::CredentialMissing(profile_id.to_string()))?;

        let runtime_cred = match provider {
            CloudProvider::Beam => RuntimeCredential::BeamBearer(secret_val),
            CloudProvider::Modal => {
                let (token_id, token_secret) =
                    decode_modal_runtime_secret(&secret_val).map_err(|_| {
                        InferenceServiceError::CredentialMissing(profile_id.to_string())
                    })?;
                RuntimeCredential::ModalProxy {
                    token_id,
                    token_secret,
                }
            }
        };

        let bound_cred = BoundRuntimeCredential::new(&target, runtime_cred)
            .map_err(InferenceServiceError::Http)?;

        CloudHttpClient::new(target, bound_cred).map_err(InferenceServiceError::Http)
    }

    /// Execute a fully authenticated, durable remote cloud inference operation.
    ///
    /// Reports `preparing` first and then every phase change to the progress
    /// sink, and stops on [`request_render_cancel`] or a `CancelRequested`
    /// recorded in the journal while it waits. The attempt id is
    /// [`attempt_id_for_nonce`] of `grant_nonce`.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_cloud_render(
        &self,
        grant_nonce: &str,
        job_path: &Path,
        page_index: u32,
        region_id: &str,
        target: &ExecutionTarget,
        recipe: &RenderRecipe,
        intent: &OperationIntent,
        config: &InferenceConfig,
        cloud_allowed: bool,
        poll_opts: &PollOptions,
    ) -> Result<ApiRegion, InferenceServiceError> {
        self.execute_cloud_render_queued(grant_nonce, job_path, page_index, region_id, target,
            recipe, intent, config, cloud_allowed, poll_opts, &|| {})
    }

    /// The batch scheduler releases its preparation slot once acceptance is
    /// durable. Polling keeps the remote job in flight without holding the
    /// preparation budget or its encoded image payloads.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute_cloud_render_queued(
        &self,
        grant_nonce: &str,
        job_path: &Path,
        page_index: u32,
        region_id: &str,
        target: &ExecutionTarget,
        recipe: &RenderRecipe,
        intent: &OperationIntent,
        config: &InferenceConfig,
        cloud_allowed: bool,
        poll_opts: &PollOptions,
        submitted: &dyn Fn(),
    ) -> Result<ApiRegion, InferenceServiceError> {
        let attempt_id = attempt_id_for_nonce(grant_nonce);
        // Checked before a single event goes out: the render already running
        // under this id is the one the interface is watching, and a `failed`
        // from here would be read as its end.
        let Some(live) = LiveRender::register(&attempt_id) else {
            return Err(InferenceServiceError::Journal(
                JournalError::AttemptLocked { attempt_id },
            ));
        };
        let reporter = PhaseReporter::new(
            self.progress.as_ref(),
            &attempt_id,
            region_id,
            &chapter_id_of(job_path),
            page_index,
            target.provider(),
            target.profile_id(),
        );
        reporter.enter(RenderPhase::Preparing);
        let outcome = self.dispatch_and_attach(
            &live,
            &reporter,
            &attempt_id,
            grant_nonce,
            job_path,
            page_index,
            region_id,
            target,
            recipe,
            intent,
            config,
            cloud_allowed,
            poll_opts,
            submitted,
        );
        reporter.settle(&outcome);
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_and_attach(
        &self,
        live: &LiveRender,
        reporter: &PhaseReporter<'_>,
        attempt_id: &str,
        grant_nonce: &str,
        job_path: &Path,
        page_index: u32,
        region_id: &str,
        target: &ExecutionTarget,
        recipe: &RenderRecipe,
        intent: &OperationIntent,
        config: &InferenceConfig,
        cloud_allowed: bool,
        poll_opts: &PollOptions,
        submitted: &dyn Fn(),
    ) -> Result<ApiRegion, InferenceServiceError> {
        if !cloud_allowed {
            return Err(InferenceServiceError::CloudDisabled);
        }

        let provider = target
            .provider()
            .ok_or(InferenceServiceError::LocalTargetNotAllowed)?;
        let profile_id = target
            .profile_id()
            .ok_or(InferenceServiceError::LocalTargetNotAllowed)?;

        let profile = match provider {
            CloudProvider::Beam => config.beam_profiles.get(profile_id),
            CloudProvider::Modal => config.modal_profiles.get(profile_id),
        }
        .ok_or_else(|| InferenceServiceError::ProfileNotFound(profile_id.to_string(), provider))?;

        let canonical_endpoint_fingerprint =
            compute_canonical_endpoint_fingerprint(&profile.endpoint_url)
                .map_err(|e| InferenceServiceError::Consent(ConsentError::InvalidEndpoint(e)))?;

        // 1. Read local source & region under lock_job to extract exact crop, hint, and hashes,
        // validating page index resolution and region source match before remote client setup.
        let input = read_render_input(job_path, page_index, region_id, recipe)?;

        // 2. Build HTTP client first before creating durable intent or consuming grant
        let client = self.build_http_client(provider, profile_id, config)?;

        let operation_digest = compute_operation_digest(intent)?;

        // 3. Exact immutable GrantScope construction
        let scope = input.scope(
            provider,
            profile_id,
            &canonical_endpoint_fingerprint,
            operation_digest,
            recipe,
            region_id,
        );
        let RenderInput {
            crop_bounds,
            crop_png,
            crop_sha256,
            hint_png,
            hint_sha256,
            source_hash,
            revision,
            input_sha256,
            predecessors_sha256,
            source_idx,
            ..
        } = input;

        // 4. Create Intent & Acquire Journal Lock
        let job_id = format!(
            "job-{}",
            &sha256_hex(format!("{region_id}:{page_index}").as_bytes())[0..24]
        );

        let wire_recipe: WireRenderRecipe = recipe.clone().into();
        // The recipe's sampling has one source. A retry of a region adds to
        // its seed here, so that it is a new render and not the same image
        // again; a first attempt carries the seed unchanged. A gateway that
        // does not say it takes a later seed refuses one, so it gets the
        // recipe's, as before.
        let mut sampling = recipe.sampling();
        let sent_before = self.journal.sent_attempts(Some(&chapter_id_of(job_path)), Some(page_index), region_id)?;
        if sent_before > 0 && client.serves_operation(RETRY_SEED_OPERATION).unwrap_or(false) {
            sampling.seed += sent_before * recipe.retry_seed_stride();
        }

        let req_meta = JobRequestMetadata {
            protocol_version: PROTOCOL_VERSION.to_string(),
            job_id: job_id.clone(),
            attempt_id: attempt_id.to_string(),
            recipe: wire_recipe,
            width: crop_bounds.w,
            height: crop_bounds.h,
            seed: sampling.seed,
            steps: sampling.steps,
            guidance_scaled: sampling.guidance_scaled,
            image_sha256: crop_sha256.clone(),
            hint_sha256: hint_sha256.clone(),
            request_digest: String::new(),
        };

        let request_digest = compute_request_digest(&req_meta);
        let mut req_meta = req_meta;
        req_meta.request_digest = request_digest.clone();

        let create_intent = CreateAttemptIntent {
            qwen_edit: recipe.qwen_edit.clone(),
            attempt_id: attempt_id.to_string(),
            job_id: job_id.clone(),
            provider,
            profile_id: profile_id.to_string(),
            endpoint_fingerprint: canonical_endpoint_fingerprint,
            recipe_id: recipe.recipe_id.clone(),
            preprocessing_version: recipe.preprocessing_version.clone(),
            model_id: recipe.model_id.clone(),
            model_revision: recipe.model_revision.clone(),
            native_mask_conditioning: recipe.native_mask_conditioning,
            width: crop_bounds.w,
            height: crop_bounds.h,
            seed: sampling.seed,
            steps: sampling.steps,
            guidance_scaled: sampling.guidance_scaled,
            source_image_hash: source_hash.clone(),
            region_id: region_id.to_string(),
            region_revision: revision,
            input_sha256: Some(input_sha256),
            predecessors_sha256: Some(predecessors_sha256),
            crop_sha256: crop_sha256.clone(),
            hint_sha256: hint_sha256.clone(),
            request_digest,
            // Recovery finds the job again from these, so they are the
            // library's own ids and not whatever the caller called the page.
            chapter_id: Some(chapter_id_of(job_path)),
            page_index: Some(page_index),
        };

        // Nothing durable exists yet, so a cancel that arrived while the crop
        // was prepared ends the render here: no intent, no grant spent.
        if live.cancelled() {
            return Err(InferenceServiceError::Cancelled);
        }

        let (_record, guard) = self.journal.create_intent(create_intent)?;

        // The last point where a cancel costs nothing: the grant is unspent and
        // no request has been made, so the intent closes as never dispatched.
        if live.cancelled() {
            let _ = self.journal.abort_intent_no_dispatch(&guard);
            return Err(InferenceServiceError::Cancelled);
        }

        let dispatch_epoch = self.resolve_grants().get_profile_epoch(provider, profile_id);
        // 5. Consume grant at the last safe point before marking Dispatching and network dispatch
        if let Err(e) = self
            .resolve_grants()
            .validate_and_consume(grant_nonce, &scope)
        {
            let _ = self.journal.abort_intent_no_dispatch(&guard);
            return match e {
                GrantError::ScopeMismatch { field: "input_sha256" | "predecessors_sha256" } =>
                    Err(InferenceServiceError::StaleAttachment("underlay changed before dispatch".into())),
                other => Err(InferenceServiceError::Grant(other)),
            };
        }

        if let Err(error) = self.dispatch_authorized(provider, profile_id, dispatch_epoch) {
            let _ = self.journal.abort_intent_no_dispatch(&guard);
            return Err(error);
        }

        // 6. Mark Dispatching immediately before network dispatch
        let _permit = match self.journal.mark_dispatching(&guard) {
            Ok(p) => p,
            Err(e) => {
                let _ = self.journal.abort_intent_no_dispatch(&guard);
                return Err(InferenceServiceError::Journal(e));
            }
        };
        // Release exclusive attempt lock before performing network operations (Property 6)
        drop(guard);

        // 7. Submit job over HTTP POST (executed without holding attempt lock - Property 6)
        if let Err(error) = self.dispatch_authorized(provider, profile_id, dispatch_epoch) {
            let guard = self.lock_attempt(attempt_id)?;
            self.journal.record_not_enqueued(&guard, "profile changed before POST")?;
            return Err(error);
        }
        reporter.enter(RenderPhase::Submitting);
        let accepted =
            match client.submit_job(&req_meta, &crop_png, &hint_png, self.journal.limits()) {
                Ok(acc) => {
                    let guard = self.lock_attempt(attempt_id)?;
                    self.journal.record_accepted_handle(&guard, &acc.handle)?;
                    acc
                }
                Err(e) => {
                    let guard = self.lock_attempt(attempt_id)?;
                    if e.definitely_not_enqueued() {
                        self.journal.record_not_enqueued(&guard, "submission was not enqueued")?;
                        return Err(InferenceServiceError::Http(e));
                    }
                    self.journal.record_dispatch_transport_error(&guard)?;
                    return Err(InferenceServiceError::AmbiguousSubmission(e));
                }
            };

        // Upload payloads are no longer needed after durable acceptance. A
        // batch can prepare the next job while this one waits on the GPU.
        drop(crop_png);
        drop(hint_png);
        drop(scope);
        // 8. Poll status until completion (executed without holding attempt lock - Property 6)
        reporter.enter(RenderPhase::Queued);
        submitted();
        let status = self.poll_to_completion(
            live,
            reporter,
            &client,
            attempt_id,
            &accepted.handle,
            &req_meta,
            poll_opts,
            true,
            false,
        )?;

        // 9. Download, cache and attach
        self.download_and_attach(
            reporter,
            &client,
            attempt_id,
            &accepted.handle,
            &req_meta,
            &status,
            job_path,
            Some(source_idx),
        )
    }

    /// Wait for a remote job to finish, one status request per interval.
    ///
    /// A status fault is retried on the same handle, never by resubmitting. A
    /// cancel stops the wait: the flag at once, and a `CancelRequested` in the
    /// journal at the next poll when `watch_journal` says one there is news
    /// (it is not for an attempt whose cancel was recorded before the wait).
    #[allow(clippy::too_many_arguments)]
    fn poll_to_completion(
        &self,
        live: &LiveRender,
        reporter: &PhaseReporter<'_>,
        client: &CloudHttpClient,
        attempt_id: &str,
        handle: &str,
        req_meta: &JobRequestMetadata,
        poll_opts: &PollOptions,
        watch_journal: bool,
        unknown_job_ends: bool,
    ) -> Result<JobStatusResponse, InferenceServiceError> {
        let start = Instant::now();
        let mut polls = 0;
        let mut not_found = 0;
        loop {
            if self.journal.get_record(attempt_id).is_ok_and(|record| matches!(record.phase, AttemptPhase::Abandoned { .. })) {
                return Err(InferenceServiceError::Cancelled);
            }
            if live.cancelled() || (watch_journal && self.cancel_recorded(attempt_id)) {
                return Err(self.stop_after_cancel(client, attempt_id, handle, req_meta));
            }
            if polls >= poll_opts.max_polls || start.elapsed() >= poll_opts.timeout {
                return Err(InferenceServiceError::PollingTimeout(handle.to_string()));
            }
            polls += 1;
            // A polling fault leaves the durable handle in place: the next
            // poll asks again and nothing is resubmitted.
            let answer = client.get_job_status(handle, req_meta);
            // A job the gateway no longer knows can never finish. Only a wait
            // that resumes an old attempt ends it, and only on the same answer
            // several polls in a row: a render just accepted is not failed on
            // one stray answer.
            not_found = if matches!(answer, Err(HttpTransportError::UnexpectedStatus { status: 404 })) { not_found + 1 } else { 0 };
            if unknown_job_ends && not_found >= UNKNOWN_JOB_POLLS {
                let (error_code, message) = ("remote_job_not_found", "the gateway does not know this job");
                if let Ok(guard) = self.lock_attempt(attempt_id) {
                    let _ = self.journal.record_failed(&guard, error_code, message);
                }
                return Err(InferenceServiceError::RemoteJobFailed { error_code: error_code.into(), message: message.into() });
            }
            if let Ok(status) = answer {
                match status.status {
                    JobExecutionStatus::Pending => reporter.enter(RenderPhase::Queued),
                    JobExecutionStatus::Running => reporter.enter(RenderPhase::Running),
                    JobExecutionStatus::Completed => return Ok(status),
                    JobExecutionStatus::Failed => {
                        if let Ok(guard) = self.lock_attempt(attempt_id) {
                            let _ = self.journal.record_remote_failure(&guard, status.error.as_ref());
                        }
                        let err = status.error.unwrap_or(TypedError {
                            error_code: "remote_failure".into(),
                            message: "remote job failed".into(),
                        });
                        return Err(InferenceServiceError::RemoteJobFailed {
                            error_code: err.error_code,
                            message: err.message,
                        });
                    }
                    JobExecutionStatus::Cancelled => {
                        if let Ok(guard) = self.lock_attempt(attempt_id) {
                            let _ = self.journal.record_cancelled(&guard);
                        }
                        return Err(InferenceServiceError::RemoteJobCancelled(
                            handle.to_string(),
                        ));
                    }
                }
            }
            live.wait(poll_opts.poll_interval);
        }
    }

    /// Whether another command has recorded a cancel for this attempt.
    fn cancel_recorded(&self, attempt_id: &str) -> bool {
        matches!(
            self.journal
                .get_record(attempt_id)
                .map(|record| record.phase),
            Ok(AttemptPhase::CancelRequested { .. })
        )
    }

    /// Stop a render whose accepted job the user cancelled.
    ///
    /// The journal says `CancelRequested` before the request goes out, as
    /// [`Self::cancel_cloud_render`] does, so a crash from here recovers as a
    /// cancel still to be confirmed. The request is sent even when a cancel
    /// command recorded it first: the gateway treats a repeat as a no-op, and
    /// that command's own request may not have arrived. The gateway then gets
    /// a short window to confirm; a job it has not confirmed stays
    /// `CancelRequested`, which is non-terminal and settled by recovery.
    fn stop_after_cancel(
        &self,
        client: &CloudHttpClient,
        attempt_id: &str,
        handle: &str,
        req_meta: &JobRequestMetadata,
    ) -> InferenceServiceError {
        if let Ok(guard) = self.lock_attempt(attempt_id) {
            let _ = self.journal.record_cancel_requested(&guard);
        }
        let _ = client.cancel_job(handle, req_meta);
        for _ in 0..CANCEL_CONFIRM_POLLS {
            let status = client.get_job_status(handle, req_meta);
            match status.as_ref().map(|status| status.status) {
                Ok(JobExecutionStatus::Cancelled) => {
                    if let Ok(guard) = self.lock_attempt(attempt_id) {
                        let _ = self.journal.record_cancelled(&guard);
                    }
                    break;
                }
                Ok(JobExecutionStatus::Failed) => {
                    if let Ok(guard) = self.lock_attempt(attempt_id) {
                        let _ = self.journal.record_remote_failure(&guard, status.as_ref().ok().and_then(|status| status.error.as_ref()));
                    }
                    break;
                }
                // Finished before the cancel landed: the result stays with the
                // gateway and recovery attaches it, which is the journal's
                // `CancelRequested` to `ResultCached` edge.
                Ok(JobExecutionStatus::Completed) => break,
                _ => std::thread::sleep(CANCEL_CONFIRM_INTERVAL),
            }
        }
        InferenceServiceError::Cancelled
    }

    /// Download a finished job's result, cache it, and attach it.
    #[allow(clippy::too_many_arguments)]
    fn download_and_attach(
        &self,
        reporter: &PhaseReporter<'_>,
        client: &CloudHttpClient,
        attempt_id: &str,
        handle: &str,
        req_meta: &JobRequestMetadata,
        status: &JobStatusResponse,
        job_path: &Path,
        source_idx: Option<usize>,
    ) -> Result<ApiRegion, InferenceServiceError> {
        reporter.enter(RenderPhase::Downloading);
        let result_meta = ResultMetadata {
            handle: handle.to_string(),
            job_id: req_meta.job_id.clone(),
            attempt_id: req_meta.attempt_id.clone(),
            request_digest: req_meta.request_digest.clone(),
            recipe_id: req_meta.recipe.recipe_id.clone(),
            preprocessing_version: req_meta.recipe.preprocessing_version.clone(),
            model_id: req_meta.recipe.model_id.clone(),
            model_revision: req_meta.recipe.model_revision.clone(),
            native_mask_conditioning: req_meta.recipe.native_mask_conditioning,
            result_digest: status.result_digest.clone().unwrap_or_default(),
            reported_cost_usd: status.reported_cost_usd,
            width: req_meta.width,
            height: req_meta.height,
            byte_length: status.result_bytes.unwrap_or(0),
        };

        let result_bytes = match client.fetch_result_bytes_while(
            handle,
            &result_meta,
            req_meta,
            self.journal.limits(),
            &|| !render_cancelled(attempt_id),
        ) {
            Ok(bytes) => bytes,
            // A slow link can hold a download for minutes: a cancel stops it as
            // it stops the wait before it, and recovery settles the result.
            Err(_) if render_cancelled(attempt_id) => {
                return Err(self.stop_after_cancel(client, attempt_id, handle, req_meta));
            }
            Err(error) => return Err(error.into()),
        };

        let guard = self.lock_attempt(attempt_id)?;
        let decoded = self
            .journal
            .cache_result(&guard, &result_bytes, &result_meta)?;

        #[cfg(test)]
        self.fail_attachment_at("cache_result")?;
        // Applied at once, whatever the model: nobody is asked first. Only
        // the user rejects a result, afterwards (`library::reject_result`).
        reporter.enter(RenderPhase::Compositing);
        let (_patch_id, region) = self.attach_cached(&guard, job_path, source_idx, &decoded)?;
        Ok(region)
    }

    /// Attach a validated cached result to the project, two-phase, and answer
    /// with the committed patch id and the region.
    ///
    /// A retry of an attempt that already reached `AttachmentPending` reuses
    /// that patch id, so it commits the same patch instead of tripping
    /// `DuplicateCommit`. If the patch is already in the project (the crash
    /// fell between the manifest write and the confirmation), the attempt is
    /// confirmed without compositing again: the region's revision changed with
    /// that write, so a second attach would only be refused as stale.
    fn attach_cached(
        &self,
        guard: &AttemptLockGuard,
        job_path: &Path,
        source_idx: Option<usize>,
        decoded: &DecodedResultCrop,
    ) -> Result<(String, ApiRegion), InferenceServiceError> {
        let record = self.journal.read_record_locked(guard)?;
        let (handle, patch_id, result_digest, reported_cost_usd) = match &record.phase {
            AttemptPhase::ResultCached {
                handle,
                result_digest,
                reported_cost_usd,
                ..
            } => (
                handle.clone(),
                record.region_id.clone(),
                result_digest.clone(),
                *reported_cost_usd,
            ),
            AttemptPhase::AttachmentPending {
                handle,
                patch_id,
                result_digest,
                reported_cost_usd,
                ..
            } => (
                handle.clone(),
                patch_id.clone(),
                result_digest.clone(),
                *reported_cost_usd,
            ),
            other => {
                return Err(InferenceServiceError::Journal(
                    JournalError::InvalidTransition {
                        current: format!("{other:?}"),
                        attempted: "attach_cached".into(),
                    },
                ))
            }
        };

        let (source_idx, attached) = self.attachment_site(job_path, &record, source_idx)?;

        // Taken from the record, which is what the grant and the crop were
        // bound to: the attach re-reads the project and refuses any drift.
        let snapshot = ProjectRegionSnapshot {
            source_image_hash: record.source_image_hash.clone(),
            region_id: record.region_id.clone(),
            region_revision: record.region_revision,
            input_sha256: record.input_sha256.clone(),
            predecessors_sha256: record.predecessors_sha256.clone(),
            crop_sha256: record.crop_sha256.clone(),
            hint_sha256: record.hint_sha256.clone(),
        };
        // A no-op for an attempt already pending under this patch id.
        self.journal
            .prepare_attachment(guard, &snapshot, &patch_id)?;
        #[cfg(test)]
        self.fail_attachment_at("prepare_attachment")?;
        if let Some(region) = attached {
            self.journal.confirm_committed(guard, &patch_id)?;
            return Ok((patch_id, region));
        }

        let mut recipe = RenderRecipe::new(&record.recipe_id, &record.preprocessing_version,
            &record.model_id, &record.model_revision, record.native_mask_conditioning);
        recipe.qwen_edit = record.qwen_edit.clone();
        let region = self.attach_patch_to_project(
            job_path,
            source_idx,
            &record.region_id,
            &snapshot,
            decoded,
            &recipe,
            record.provider,
            &record.profile_id,
            &handle,
            &record.job_id,
            &record.attempt_id,
            &record.request_digest,
            &result_digest,
            reported_cost_usd,
            record.width,
            record.height,
            record.page_index,
        )?;

        #[cfg(test)]
        self.fail_attachment_at("manifest_write")?;
        self.journal.confirm_committed(guard, &patch_id)?;
        #[cfg(test)]
        self.fail_attachment_at("confirm_committed")?;
        Ok((patch_id, region))
    }

    /// Where an attempt's result goes, and the region as it stands when the
    /// project already holds this attempt's patch.
    ///
    /// A page or region that is gone is a stale attachment like any other
    /// drift: the result stays cached and nothing is written.
    fn attachment_site(
        &self,
        job_path: &Path,
        record: &AttemptRecord,
        source_idx: Option<usize>,
    ) -> Result<(usize, Option<ApiRegion>), InferenceServiceError> {
        let _lock = run::lock_job(job_path).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
        let job =
            Job::open(job_path).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
        let Some(patch) = job
            .project
            .patches
            .iter()
            .find(|patch| patch.id == record.region_id)
        else {
            // A stored detection has no patch until its render attaches one.
            let detection = job
                .project
                .detections
                .iter()
                .find(|row| row.id == record.region_id)
                .ok_or_else(|| {
                    InferenceServiceError::StaleAttachment("region no longer in chapter".into())
                })?;
            let mapped = match record.page_index {
                Some(page) => Library::resolve_page(&job.project, page as usize).ok_or_else(|| {
                    InferenceServiceError::StaleAttachment("page no longer in chapter".into())
                })?,
                None => source_idx.unwrap_or(detection.source_idx),
            };
            if mapped != detection.source_idx || source_idx.is_some_and(|idx| idx != mapped) {
                return Err(InferenceServiceError::StaleAttachment(
                    "page mapping changed".into(),
                ));
            }
            return Ok((mapped, None));
        };
        let source_idx = match (source_idx, record.page_index) {
            (Some(idx), Some(page)) => {
                let mapped =
                    Library::resolve_page(&job.project, page as usize).ok_or_else(|| {
                        InferenceServiceError::StaleAttachment("page no longer in chapter".into())
                    })?;
                if mapped != idx {
                    return Err(InferenceServiceError::StaleAttachment(
                        "page mapping changed".into(),
                    ));
                }
                idx
            }
            (Some(idx), None) => idx,
            (None, Some(page)) => {
                Library::resolve_page(&job.project, page as usize).ok_or_else(|| {
                    InferenceServiceError::StaleAttachment("page no longer in chapter".into())
                })?
            }
            (None, None) => patch.source_idx,
        };
        let carries_attempt = patch
            .provenance
            .cloud
            .as_ref()
            .and_then(|cloud| cloud.attempt_id.as_deref())
            == Some(record.attempt_id.as_str());
        if !carries_attempt {
            return Ok((source_idx, None));
        }
        let cloud = patch.provenance.cloud.as_ref().expect("checked attempt identity");
        if patch.source_idx != source_idx
            || cloud.job_id.as_deref() != Some(record.job_id.as_str())
            || cloud.request_id != record.request_digest
            || cloud.profile_id.as_deref() != Some(record.profile_id.as_str())
            || cloud.recipe_id.as_deref() != Some(record.recipe_id.as_str())
            || cloud.model != record.model_id
            || cloud.model_revision.as_deref() != Some(record.model_revision.as_str())
            || cloud.provider != match record.provider { CloudProvider::Modal => "modal", CloudProvider::Beam => "beam" }
            || patch.provenance.source_sha256 != record.source_image_hash
        {
            return Err(InferenceServiceError::StaleAttachment("repair needed: patch identity changed".into()));
        }
        let source_path = job.source_path(source_idx).ok_or_else(||
            InferenceServiceError::StaleAttachment("repair needed: committed source is missing".into()))?;
        if sha256_hex(&std::fs::read(source_path)?) != record.source_image_hash {
            return Err(InferenceServiceError::StaleAttachment("repair needed: committed source changed".into()));
        }
        job.load_display_patch(patch).map_err(|e| InferenceServiceError::StaleAttachment(
            format!("repair needed: committed patch buffers are unreadable: {e}"),
        ))?;
        let region = crate::library::region_and_status(
            &chapter_id_of(job_path),
            &job.project,
            source_idx,
            &record.region_id,
        )
        .map(|(region, _)| region);
        Ok((source_idx, region))
    }

    /// Internal helper to commit a composited patch into the project manifest.
    #[allow(clippy::too_many_arguments)]
    fn attach_patch_to_project(
        &self,
        job_path: &Path,
        source_idx: usize,
        region_id: &str,
        snapshot: &ProjectRegionSnapshot,
        decoded_crop: &DecodedResultCrop,
        recipe: &RenderRecipe,
        provider: CloudProvider,
        profile_id: &str,
        _handle: &str,
        job_id: &str,
        attempt_id: &str,
        request_digest: &str,
        _result_digest: &str,
        reported_cost_usd: Option<f64>,
        _page_w: u32,
        _page_h: u32,
        page_index: Option<u32>,
    ) -> Result<ApiRegion, InferenceServiceError> {
        let _lock = run::lock_job(job_path).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
        let mut job =
            Job::open(job_path).map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
        if let Some(page_index) = page_index {
            if Library::resolve_page(&job.project, page_index as usize) != Some(source_idx) {
                return Err(InferenceServiceError::StaleAttachment(
                    "page mapping changed".into(),
                ));
            }
        }
        // A page a Detect or Clean run is working on is that run's until its
        // `page-done`: it read the page before this result came back, and
        // would write its own results over this one. Asked under the job's
        // lock, as every edit of a page asks it ([`run::lock_page`]). A cloud
        // clean's own results land on the pages it is on, and what they may
        // replace is settled by the revision and input checks below.
        let chapter_id = chapter_id_of(job_path);
        let page = page_index
            .map(|page| page as usize)
            .or_else(|| crate::region::page_index_in(&chapter_id, region_id));
        if page.is_some_and(|page| {
            matches!(
                run::page_run(job_path, &chapter_id, page),
                Some(run::RunKind::Detect | run::RunKind::Clean)
            )
        }) {
            return Err(InferenceServiceError::PageBusy);
        }

        // Re-verify snapshot on disk before mutating project
        let cur_source_path = job
            .source_path(source_idx)
            .ok_or_else(|| InferenceServiceError::JobManifest("source path missing".into()))?;
        let cur_source_bytes = std::fs::read(&cur_source_path)?;
        if sha256_hex(&cur_source_bytes) != snapshot.source_image_hash {
            return Err(InferenceServiceError::StaleAttachment(
                "source_image_hash changed".into(),
            ));
        }

        // The region as it stands now: the patch this attempt re-renders, or
        // the stored detection it cleans. Either must be exactly what the
        // attempt was bound to.
        let cur = load_cloud_region(&job, region_id, &snapshot.source_image_hash).map_err(
            |e| match e {
                ConsentError::RegionNotFound(_) => {
                    InferenceServiceError::JobManifest("region record missing".into())
                }
                ConsentError::RenderPrepareFailed(_) => InferenceServiceError::StaleAttachment(
                    "text-shaped support cannot be attached through the legacy cloud path".into(),
                ),
                other => InferenceServiceError::JobManifest(other.to_string()),
            },
        )?;

        if cur.source_idx != source_idx {
            return Err(InferenceServiceError::StaleAttachment(
                "patch record source index mismatch".into(),
            ));
        }
        if let CloudRegionKind::Patch(stored) = &cur.kind {
            if !stored.0.visible {
                return Err(InferenceServiceError::StaleAttachment(
                    "region was deleted or hidden".into(),
                ));
            }
        }

        if cur.revision != snapshot.region_revision {
            return Err(InferenceServiceError::StaleAttachment(
                "region_revision changed".into(),
            ));
        }

        let page = job.display_page(source_idx,&cur_source_bytes).map_err(|e| {
            InferenceServiceError::Consent(ConsentError::RenderPrepareFailed(e.to_string()))
        })?;

        // The attempt's own recipe, which is the version its crop was prepared
        // under: a result still in flight when the version moved composites as
        // it was prepared, and one this build cannot composite is refused.
        let preprocessing = Preprocessing::of(&recipe.preprocessing_version).map_err(|e| {
            InferenceServiceError::StaleAttachment(e.to_string())
        })?;
        let cloud = cur
            .prepare(&job, &page, preprocessing)
            .map_err(|e| InferenceServiceError::Consent(ConsentError::RenderPrepareFailed(e)))?;
        if snapshot
            .input_sha256
            .as_deref()
            .is_some_and(|hash| hash != cloud.input.digest)
            || snapshot
                .predecessors_sha256
                .as_deref()
                .is_some_and(|hash| hash != cloud.input.predecessors)
            || cloud.crop_bounds.w != _page_w
            || cloud.crop_bounds.h != _page_h
        {
            return Err(InferenceServiceError::StaleAttachment(
                "composited input changed".into(),
            ));
        }
        let prepared = &cloud.prepared;
        let crop_png =
            encode_rgb8_png(prepared.crop().w, prepared.crop().h, prepared.image_rgb8())?;
        let hint_png =
            encode_gray8_png(prepared.crop().w, prepared.crop().h, prepared.hint_gray8())?;
        if sha256_hex(&crop_png) != snapshot.crop_sha256
            || sha256_hex(&hint_png) != snapshot.hint_sha256
        {
            return Err(InferenceServiceError::StaleAttachment(
                "encoded crop or hint changed".into(),
            ));
        }

        let generated = decoded_crop.as_generated_crop();
        let mut rendered = prepared.composite(&generated).map_err(|e| {
            InferenceServiceError::Consent(ConsentError::RenderPrepareFailed(e.to_string()))
        })?;
        rendered.mask.bounds.x += cloud.input.window.x + cloud.source_shift.0;
        rendered.mask.bounds.y += cloud.input.window.y + cloud.source_shift.1;

        let cloud_record = CloudRecord {
            provider: match provider {
                CloudProvider::Beam => "beam".to_string(),
                CloudProvider::Modal => "modal".to_string(),
            },
            profile_id: Some(profile_id.to_string()),
            job_id: Some(job_id.to_string()),
            request_id: request_digest.to_string(),
            attempt_id: Some(attempt_id.to_string()),
            recipe_id: Some(recipe.recipe_id.clone()),
            model: recipe.model_id.clone(),
            model_revision: Some(recipe.model_revision.clone()),
            tier: None,
            cost: reported_cost_usd,
            duration_ms: None,
        };
        let input_provenance = serde_json::json!({
            "version": 1,
            "input_sha256": cloud.input.digest,
            "predecessors_sha256": cloud.input.predecessors,
            "read_footprint": cleaner_core::mask::Rect::new(
                cloud.input.window.x + cloud.source_shift.0,
                cloud.input.window.y + cloud.source_shift.1,
                cloud.input.window.w, cloud.input.window.h),
            // What the render read of that window, for a later edit to be
            // tested against (`underlay::render_reach`).
            "reach": cleaner_core::mask::Rect::new(
                cloud.reach.x + cloud.source_shift.0,
                cloud.reach.y + cloud.source_shift.1,
                cloud.reach.w, cloud.reach.h),
            "reach_sha256": cloud.reach_digest,
        });
        let tone = rendered.tone.as_ref().map(|tone| tone.provenance());
        // The detected region as it stands before its result replaces it: one
        // side of the undo step the commit records below.
        let mut clean_before = None;

        match cur.kind {
            CloudRegionKind::Patch(stored) => {
                let (record, mut updated_patch) = *stored;
                let review_state = match record.review_state.as_deref() {
                    Some("review.reason.inputChanged" | "review.reason.inputUnknown") => updated_patch
                        .provenance.params_snapshot.get("review_before_input_change")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned),
                    _ => record.review_state,
                }.or_else(|| generated_review(recipe, false));
                if let Some(params) = updated_patch.provenance.params_snapshot.as_object_mut() {
                    params.remove("review_before_input_change");
                }
                updated_patch.provenance.engine = Engine::Flux;
                updated_patch.provenance.cloud = Some(cloud_record);
                let params = &mut updated_patch.provenance.params_snapshot;
                params["input_provenance"] = input_provenance;
                if let Some(edit) = &recipe.qwen_edit { params["qwen_edit"] = serde_json::json!(edit); }
                else if let Some(params) = params.as_object_mut() { params.remove("qwen_edit"); }
                // The record of the alignment these pixels had: a render
                // that aligned nothing (1.0.0) takes the earlier one away.
                match tone {
                    Some(tone) => params["tone"] = tone,
                    None => {
                        if let Some(params) = params.as_object_mut() {
                            params.remove("tone");
                        }
                    }
                }
                updated_patch.pixels = rendered.pixels;
                updated_patch.mask = rendered.mask;
                job.complete_display_region(source_idx, &updated_patch, review_state)
                    .map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
            }
            // A detection has no patch to update: its patch is made here, the
            // way a cloud re-render records one, keeps the detection's id and
            // order, and replaces it in one write (`Job::complete_detection`).
            CloudRegionKind::Detection(detection) => {
                let mask_sha256 = run::mask_digest(&rendered.mask);
                let mut params_snapshot = serde_json::json!({
                    "rung": Engine::Flux.rung_key(),
                    "fill_mode": "reconstruct",
                    "write_set": "ink",
                    "source": "auto",
                    "input_provenance": input_provenance,
                });
                if let Some(edit) = &recipe.qwen_edit { params_snapshot["qwen_edit"] = serde_json::json!(edit); }
                if let Some(tone) = tone {
                    params_snapshot["tone"] = tone;
                }
                // The detection's text group travels onto its patch, as a
                // local clean carries it: mask, group, request, output.
                if let Some(group) = detection.group.as_ref() {
                    params_snapshot["group"] = group.snapshot();
                }
                let patch = Patch {
                    id: region_id.to_string(),
                    mask: rendered.mask,
                    ink: cur.ink,
                    pixels: rendered.pixels,
                    order: detection.order,
                    visible: true,
                    provenance: Provenance {
                        engine: Engine::Flux,
                        engine_version: env!("CARGO_PKG_VERSION").into(),
                        model_sha256: None,
                        execution_provider: "cloud".into(),
                        params_snapshot,
                        mask_sha256,
                        source_sha256: snapshot.source_image_hash.clone(),
                        cloud: Some(cloud_record),
                        created: run::now(),
                    },
                };
                clean_before = crate::history::side_now(&chapter_id, &job.project, source_idx, region_id);
                let replaced = job
                    .complete_detection(
                        region_id,
                        &patch,
                        detection.review_state.clone().or_else(|| {
                            generated_review(recipe, detection.fit.is_some_and(|fit| {
                                fit.route == cleaner_core::fit::Route::Inpaint
                            }))
                        }),
                    )
                    .map_err(|e| InferenceServiceError::JobManifest(e.to_string()))?;
                if !replaced {
                    return Err(InferenceServiceError::StaleAttachment(
                        "region no longer in chapter".into(),
                    ));
                }
            }
        }
        crate::underlay::refresh_dependencies(&mut job, region_id)
            .map_err(InferenceServiceError::JobManifest)?;
        crate::library::invalidate_manifest_cache(job_path);
        // Every generated result is one undo step, written here under the
        // job's lock and not by the interface afterwards: a batch commits
        // while another chapter may be the open one.
        crate::history::record_clean(job_path, &chapter_id, &job.project, source_idx, region_id, clean_before);

        let (api_region, _) = crate::library::region_and_status(
            &chapter_id_of(job_path),
            &job.project,
            source_idx,
            region_id,
        )
        .ok_or_else(|| {
            InferenceServiceError::JobManifest("failed to resolve region in manifest".to_string())
        })?;

        Ok(api_region)
    }

    /// Request non-terminal cancellation of an in-flight cloud attempt.
    pub fn cancel_cloud_render(
        &self,
        attempt_id: &str,
        config: &InferenceConfig,
    ) -> Result<RecoveryDecision, InferenceServiceError> {
        let guard = self.journal.acquire_lock(attempt_id)?;
        let record = self.journal.read_record_locked(&guard)?;

        if record.is_terminal() {
            return Ok(RecoveryDecision::Terminal {
                message: format!("attempt is already in terminal phase: {:?}", record.phase),
            });
        }

        let handle = match &record.phase {
            AttemptPhase::Accepted { handle } | AttemptPhase::CancelRequested { handle } => {
                handle.clone()
            }
            _ => {
                return Err(InferenceServiceError::Journal(
                    JournalError::InvalidTransition {
                        current: format!("{:?}", record.phase),
                        attempted: "cancel_cloud_render".into(),
                    },
                ));
            }
        };

        // Transition to CancelRequested in journal
        self.journal.record_cancel_requested(&guard)?;
        // Drop lock before network cancellation (Property 6)
        drop(guard);

        let client = self.build_http_client(record.provider, &record.profile_id, config)?;
        let cancel_resp = client.cancel_job(&handle, &record.to_request_metadata())?;

        if cancel_resp.status != "cancel_requested" || !cancel_resp.acknowledged {
            return Err(InferenceServiceError::Wire(
                WireValidationError::NonterminalCancelStatus(cancel_resp.status),
            ));
        }

        // Poll until terminal status without holding attempt lock (Property 6)
        let req_meta = record.to_request_metadata();
        for _ in 0..10 {
            if let Ok(status) = client.get_job_status(&handle, &req_meta) {
                if status.status == JobExecutionStatus::Cancelled {
                    let guard = self.journal.acquire_lock(attempt_id)?;
                    self.journal.record_cancelled(&guard)?;
                    drop(guard);
                    return Ok(RecoveryDecision::Terminal {
                        message: "cancellation confirmed by gateway".into(),
                    });
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        Ok(RecoveryDecision::ResumeCancelPolling { handle })
    }

    /// Settle what can be settled about an attempt without the network.
    ///
    /// An interrupted submission becomes `AmbiguousUnknown` and is never
    /// resubmitted. A cached result gets one try at its attach: it is attached
    /// (`AlreadyCommitted`), or, when it can never be (its region, page or
    /// chapter changed or is gone, or the cached file is damaged), its attempt
    /// is recorded failed and comes back `Terminal`, so the region is free to
    /// be cleaned again. A chapter that cannot be read at this moment is an
    /// error and the attempt is kept for the next recovery. An accepted job
    /// comes back as `ResumePolling` or `ResumeCancelPolling` for
    /// [`Self::resume_cloud_render`]. Everything else is the journal's own
    /// decision, unchanged. The rule is the table in `docs/cloud-journal.md`
    /// section 4.3.
    pub fn recover_attempt_locally(
        &self,
        attempt_id: &str,
        job_path: Option<&Path>,
    ) -> Result<RecoveryDecision, InferenceServiceError> {
        self.recover_attempt_with_snapshot(attempt_id, job_path, None)
    }

    pub(crate) fn recover_attempt_with_snapshot(
        &self, attempt_id: &str, job_path: Option<&Path>, snapshot: Option<&RecoveryAuditSnapshot>,
    ) -> Result<RecoveryDecision, InferenceServiceError> {
        let guard = self.lock_attempt(attempt_id)?;
        match self.journal.recover_attempt(&guard)? {
            RecoveryDecision::ResultCachedReadyForAttach { .. }
            | RecoveryDecision::AttachmentPendingVerification { .. } => {
                let Some(job_path) = job_path else {
                    return self.end_unapplied(&guard, "region_changed", "the chapter is no longer in the library", true);
                };
                let decoded = match self.journal.read_cached_crop(&guard) {
                    Ok(decoded) => decoded,
                    Err(JournalError::Io(error)) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(JournalError::Io(error).into());
                    }
                    Err(JournalError::AttemptLocked { attempt_id }) => {
                        return Err(JournalError::AttemptLocked { attempt_id }.into());
                    }
                    // Kept on disk: a damaged result is evidence.
                    Err(_) => return self.end_unapplied(&guard, RESULT_CORRUPT.0, RESULT_CORRUPT.1, false),
                };
                match self.attach_cached(&guard, job_path, None, &decoded) {
                    Ok((patch_id, _)) => Ok(RecoveryDecision::AlreadyCommitted { patch_id }),
                    Err(
                        InferenceServiceError::StaleAttachment(_)
                        | InferenceServiceError::Journal(JournalError::StaleAttachment { .. }),
                    ) => self.end_unapplied(&guard, "region_changed",
                        "the region or its page changed before the result was applied", true),
                    Err(error) => Err(error),
                }
            }
            RecoveryDecision::CorruptedResultRetainedForInspection { .. } => {
                self.end_unapplied(&guard, RESULT_CORRUPT.0, RESULT_CORRUPT.1, false)
            }
            decision @ RecoveryDecision::AlreadyCommitted { .. } => {
                let record = self.journal.read_record_locked(&guard)?;
                let Some(path) = job_path else {
                    return Ok(RecoveryDecision::Terminal { message: "chapter removed".into() });
                };
                if self.journal.repair_acknowledged(attempt_id) { return Ok(decision); }
                let loaded;
                let snapshot = match snapshot {
                    Some(snapshot) => snapshot,
                    None => { loaded = RecoveryAuditSnapshot::load(path)?; &loaded }
                };
                let valid = match self.journal.read_audit(attempt_id, &snapshot.key) {
                    Some(valid) => valid,
                    None => {
                        let valid = self.audit_committed_snapshot(snapshot, &record)?;
                        self.journal.record_audit(&guard, &snapshot.key, valid)?;
                        valid
                    }
                };
                if !valid {
                    return Err(InferenceServiceError::StaleAttachment(
                        "repair needed: committed patch missing at the original region revision".into(),
                    ));
                }
                Ok(decision)
            }
            other => Ok(other),
        }
    }

    /// End an attempt whose result can never be applied. Its region is then
    /// an ordinary detected region again, and the next cloud clean plans it.
    fn end_unapplied(
        &self, guard: &AttemptLockGuard, error_code: &str, message: &str, drop_result: bool,
    ) -> Result<RecoveryDecision, InferenceServiceError> {
        self.journal.record_failed(guard, error_code, message)?;
        if drop_result { self.journal.drop_cached_result(guard)?; }
        Ok(RecoveryDecision::Terminal { message: message.into() })
    }

    /// Whether a committed attempt's result is still what its region shows:
    /// the audit [`Self::recover_attempt_with_snapshot`] runs, and nothing
    /// else - no attach, no network, no change to the attempt. An answer
    /// already recorded for this manifest state is reused, and a fresh one is
    /// recorded when no other command holds the attempt, so the next read is
    /// cheap.
    pub(crate) fn committed_result_intact(
        &self, record: &AttemptRecord, snapshot: &RecoveryAuditSnapshot,
    ) -> Result<bool, InferenceServiceError> {
        if let Some(valid) = self.journal.read_audit(&record.attempt_id, &snapshot.key) {
            return Ok(valid);
        }
        let valid = self.audit_committed_snapshot(snapshot, record)?;
        if let Ok(guard) = self.journal.acquire_lock(&record.attempt_id) {
            let _ = self.journal.record_audit(&guard, &snapshot.key, valid);
        }
        Ok(valid)
    }

    fn audit_committed_snapshot(
        &self, snapshot: &RecoveryAuditSnapshot, record: &AttemptRecord,
    ) -> Result<bool, InferenceServiceError> {
        let job = &snapshot.job;
        if snapshot.records_user_replacement(record) { return Ok(true); }
        // A result the user rejected or undid is set aside whole
        // (`Job::restore_detection`), and its region is the detection it was
        // rendered from, at the revision the attempt captured. That is the
        // user's choice and not a lost result, whether or not the undo
        // journal still holds the step.
        if job.project.withdrawn_patches.iter().any(|patch| patch.id == record.region_id
            && patch.provenance.cloud.as_ref().and_then(|cloud| cloud.attempt_id.as_deref()) == Some(record.attempt_id.as_str()))
        {
            return Ok(true);
        }
        let patch = job.project.patches.iter().find(|patch| patch.id == record.region_id);
        let detection = job.project.detections.iter().find(|row| row.id == record.region_id);
        let Some(source_idx) = patch.map(|row| row.source_idx).or_else(|| detection.map(|row| row.source_idx)) else {
            return Ok(true); // Region was deleted, not restored as its predecessor.
        };
        let source_hash = snapshot.source_hash(source_idx)?;
        if let Some(patch) = patch {
            if let Some(cloud) = &patch.provenance.cloud {
                if cloud.attempt_id.as_deref() == Some(record.attempt_id.as_str()) {
                    let matches = source_hash == record.source_image_hash
                        && patch.provenance.source_sha256 == record.source_image_hash
                        && cloud.provider == match record.provider { CloudProvider::Modal => "modal", CloudProvider::Beam => "beam" }
                        && cloud.job_id.as_deref() == Some(record.job_id.as_str())
                        && cloud.request_id == record.request_digest
                        && cloud.profile_id.as_deref() == Some(record.profile_id.as_str())
                        && cloud.recipe_id.as_deref() == Some(record.recipe_id.as_str())
                        && cloud.model == record.model_id
                        && cloud.model_revision.as_deref() == Some(record.model_revision.as_str());
                    return Ok(matches && job.load_display_patch(patch).is_ok());
                }
                // A later committed cloud rerender is valid, a rollback to an
                // earlier cloud attempt without an undo action is not.
                if let Some(next) = cloud.attempt_id.as_deref().and_then(|id| self.journal.get_record(id).ok()) {
                    if matches!(next.phase, AttemptPhase::Committed { .. })
                        && next.created_at_ms >= record.updated_at_ms && next.region_id == record.region_id
                        && next.chapter_id == record.chapter_id && next.page_index == record.page_index
                        && cloud.request_id == next.request_digest
                    {
                        return self.audit_committed_snapshot(snapshot, &next);
                    }
                }
                return Ok(false);
            }
        }
        // Revision is a content identity, not a monotonic integer. A different
        // local output or freshly detected region supersedes the captured input.
        let revision = if let Some(patch) = patch {
            job.load_display_patch(patch).ok().map(|loaded|
                crate::inference::consent::compute_region_revision_hash(&source_hash, patch, &loaded.mask, &loaded.ink).0)
        } else {
            job.load_detection(&record.region_id).ok().flatten().map(|loaded|
                crate::inference::consent::compute_detection_revision_hash(&source_hash, &loaded.record, &loaded.mask, &loaded.ink).0)
        };
        // Missing original detection masks are evidence, not a new revision.
        Ok(revision.is_some_and(|revision| revision != record.region_revision))
    }

    /// Resolve an interrupted submission using only the original provider and endpoint.
    /// No grant is issued or consumed and no image is uploaded on this path.
    pub fn resolve_unknown_attempt(
        &self, attempt_id: &str, config: &InferenceConfig,
    ) -> Result<RecoveryDecision, InferenceServiceError> {
        let record = {
            let guard = self.lock_attempt(attempt_id)?;
            let decision = self.journal.recover_attempt(&guard)?;
            if !matches!(decision, RecoveryDecision::AmbiguousUnknown { .. }) {
                return Ok(decision);
            }
            self.journal.read_record_locked(&guard)?
        };
        let unresolved = || RecoveryDecision::AmbiguousUnknown {
            reason: "existing submission remains unresolved; no replacement sent".into(),
        };
        let client = match self.recovery_client(&record, config) {
            Ok(client) => client,
            Err(_) => return Ok(unresolved()),
        };
        let status = match client.lookup_attempt(&record.to_request_metadata()) {
            Ok(Some(status)) if status.status != JobExecutionStatus::Pending => status,
            _ => return Ok(unresolved()),
        };
        let guard = self.lock_attempt(attempt_id)?;
        // Another recovery may have resolved it while the GET was in flight.
        if !matches!(self.journal.read_record_locked(&guard)?.phase, AttemptPhase::Unknown { .. }) {
            return Ok(self.journal.recover_attempt(&guard)?);
        }
        self.journal.record_lookup_acceptance(&guard, &status)?;
        Ok(RecoveryDecision::ResumePolling { handle: status.handle })
    }

    fn recovery_client(
        &self, record: &AttemptRecord, config: &InferenceConfig,
    ) -> Result<CloudHttpClient, InferenceServiceError> {
        let profile = match record.provider {
            CloudProvider::Modal => config.modal_profiles.get(&record.profile_id),
            CloudProvider::Beam => config.beam_profiles.get(&record.profile_id),
        }.ok_or(InferenceServiceError::ProfileChanged)?;
        if compute_canonical_endpoint_fingerprint(&profile.endpoint_url)
            .map_err(|_| InferenceServiceError::ProfileChanged)? != record.endpoint_fingerprint
        {
            return Err(InferenceServiceError::ProfileChanged);
        }
        self.build_http_client(record.provider, &record.profile_id, config)
    }

    /// Wait on an attempt the journal holds as accepted, then attach its
    /// result: the recovery path for a job that outlived the app.
    ///
    /// Never submits. Reports the same phases as [`Self::execute_cloud_render`]
    /// from `queued` on and stops on the same cancel. An attempt whose cancel
    /// was recorded before this wait is waited on until the gateway settles
    /// it, and attached if the job finished anyway.
    pub fn resume_cloud_render(
        &self,
        attempt_id: &str,
        job_path: &Path,
        config: &InferenceConfig,
        poll_opts: &PollOptions,
    ) -> Result<ApiRegion, InferenceServiceError> {
        let Some(live) = LiveRender::register(attempt_id) else {
            return Err(InferenceServiceError::Journal(
                JournalError::AttemptLocked {
                    attempt_id: attempt_id.to_string(),
                },
            ));
        };
        let record = self.journal.get_record(attempt_id)?;
        let chapter_id = record
            .chapter_id
            .clone()
            .unwrap_or_else(|| chapter_id_of(job_path));
        let reporter = PhaseReporter::new(
            self.progress.as_ref(),
            attempt_id,
            &record.region_id,
            &chapter_id,
            record.page_index.unwrap_or(0),
            Some(record.provider),
            Some(&record.profile_id),
        );
        let outcome = self.wait_and_attach(&live, &reporter, &record, job_path, config, poll_opts);
        reporter.settle(&outcome);
        outcome
    }

    fn wait_and_attach(
        &self,
        live: &LiveRender,
        reporter: &PhaseReporter<'_>,
        record: &AttemptRecord,
        job_path: &Path,
        config: &InferenceConfig,
        poll_opts: &PollOptions,
    ) -> Result<ApiRegion, InferenceServiceError> {
        let (handle, cancel_is_news) = match &record.phase {
            AttemptPhase::Accepted { handle } => (handle.clone(), true),
            AttemptPhase::CancelRequested { handle } => (handle.clone(), false),
            other => {
                return Err(InferenceServiceError::Journal(
                    JournalError::InvalidTransition {
                        current: format!("{other:?}"),
                        attempted: "resume_cloud_render".into(),
                    },
                ))
            }
        };
        reporter.enter(RenderPhase::Queued);
        let client = self.recovery_client(record, config)?;
        let req_meta = record.to_request_metadata();
        let status = self.poll_to_completion(
            live,
            reporter,
            &client,
            &record.attempt_id,
            &handle,
            &req_meta,
            poll_opts,
            cancel_is_news,
            true,
        )?;
        self.download_and_attach(
            reporter,
            &client,
            &record.attempt_id,
            &handle,
            &req_meta,
            &status,
            job_path,
            None,
        )
    }

    /// Recover an attempt upon application restart or following a crash.
    ///
    /// [`Self::recover_attempt_locally`], and then for an accepted job a wait
    /// bounded by `poll_opts`: a result that arrives is attached
    /// (`AlreadyCommitted`), a job still running stays `ResumePolling` (or
    /// `ResumeCancelPolling`), and one the gateway failed, cancelled or no
    /// longer knows, or whose endpoint is no longer configured, is `Terminal`.
    /// Nothing is ever resubmitted.
    pub fn recover_cloud_attempt(
        &self,
        attempt_id: &str,
        job_path: &Path,
        config: &InferenceConfig,
        poll_opts: &PollOptions,
    ) -> Result<RecoveryDecision, InferenceServiceError> {
        let local = self.recover_attempt_locally(attempt_id, Some(job_path))?;
        let local = if matches!(local, RecoveryDecision::AmbiguousUnknown { .. }) {
            self.resolve_unknown_attempt(attempt_id, config)?
        } else { local };
        let waiting = match local {
            decision @ (RecoveryDecision::ResumePolling { .. }
            | RecoveryDecision::ResumeCancelPolling { .. }) => decision,
            settled => return Ok(settled),
        };
        match self.resume_cloud_render(attempt_id, job_path, config, poll_opts) {
            Ok(_) => match self.journal.get_record(attempt_id)?.phase {
                AttemptPhase::Committed { patch_id, .. } => {
                    Ok(RecoveryDecision::AlreadyCommitted { patch_id })
                }
                other => Err(InferenceServiceError::Journal(
                    JournalError::InvalidTransition {
                        current: format!("{other:?}"),
                        attempted: "recover_cloud_attempt".into(),
                    },
                )),
            },
            Err(InferenceServiceError::PollingTimeout(_)) => Ok(waiting),
            Err(
                err @ (InferenceServiceError::RemoteJobFailed { .. }
                | InferenceServiceError::RemoteJobCancelled(_)),
            ) => Ok(RecoveryDecision::Terminal {
                message: err.to_string(),
            }),
            // The endpoint the job was sent to is no longer configured, so
            // nothing can ask for its result again.
            Err(InferenceServiceError::ProfileChanged) => {
                let message = "the endpoint this job was sent to is no longer configured";
                let guard = self.lock_attempt(attempt_id)?;
                self.journal.record_failed(&guard, "profile_removed", message)?;
                Ok(RecoveryDecision::Terminal { message: message.into() })
            }
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn dispatch_guard_rechecks_selection_and_profile_epoch_after_grant_consumption() {
        let service = InferenceService::new(std::env::temp_dir().join("mc-dispatch-guard-test"),
            cleaner_core::cloud_wire::provisional_fixture_limits())
            .with_dispatch_guard(Box::new(|_, _| false));
        let profile = "modal-prof-dispatch-guard";
        let epoch = GrantService::global().get_profile_epoch(CloudProvider::Modal, profile);
        let error = service.dispatch_authorized(CloudProvider::Modal, profile, epoch).unwrap_err();
        assert_eq!(error.code(), "cloud_run_profile_changed");

        let service = InferenceService::new(std::env::temp_dir().join("mc-dispatch-epoch-test"),
            cleaner_core::cloud_wire::provisional_fixture_limits());
        GrantService::global().invalidate_profile(CloudProvider::Modal, profile);
        let error = service.dispatch_authorized(CloudProvider::Modal, profile, epoch).unwrap_err();
        assert_eq!(error.code(), "cloud_run_profile_changed");
    }
    use crate::inference::config::validate_https_endpoint;
    use cleaner_core::cloud_decode::decode_result_crop;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use crate::inference::secrets::SecretValue;
    use cleaner_core::engines::render::CloudProvider;
    use cleaner_core::image::fixtures;
    use cleaner_core::image::Format;
    use cleaner_core::mask::{Mask, Rect};
    use cleaner_core::patch::{Patch, Provenance};
    use crate::inference::consent::compute_region_revision_hash;
    use cleaner_core::project::{Project, StripMode};

    fn test_scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("mc-inference-service-tests")
            .join(name)
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup_test_job(root: &Path, patch_id: &str) -> (PathBuf, Rect) {
        let raws = root.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let mut raster = fixtures::by_name("l8").raster;
        raster.width = 256;
        raster.height = 256;
        raster.data = vec![200; 256 * 256];
        let bytes = cleaner_core::image::encode(&raster, Format::Png).unwrap();
        let page_path = raws.join("001.png");
        std::fs::write(&page_path, &bytes).unwrap();

        let source_ref = cleaner_core::ingest::source_ref(&page_path, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(
            manifest.parent().unwrap(),
            "test-app",
            StripMode::Single,
            &[source_ref],
        );
        let mut job = Job::create(&manifest, project).unwrap();

        let bounds = Rect::new(100, 100, 32, 32);
        let mut pixels = raster.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![200; (bounds.w * bounds.h) as usize];

        let patch = Patch {
            id: patch_id.to_string(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 1,
            visible: true,
            provenance: Provenance {
                engine: Engine::Fill,
                engine_version: "1.0".into(),
                model_sha256: None,
                execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({ "thickness": 2 }),
                mask_sha256: sha256_hex(&vec![255; (bounds.w * bounds.h) as usize]),
                source_sha256: sha256_hex(&bytes),
                cloud: None,
                created: 1_760_000_000,
            },
        };

        job.complete_region(0, &patch, None).unwrap();
        (manifest, bounds)
    }

    #[test]
    fn text_shaped_region_is_refused_before_cloud_dispatch_or_target_mutation() {
        let scratch = test_scratch("text-shape-dispatch-refusal");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let grants = Arc::new(GrantService::new());
        let consent = Arc::new(ConsentService::new_isolated(Arc::clone(&grants)));
        let target = ExecutionTarget::Modal { profile_id: "modal-prof-1".into() };
        let recipe = RenderRecipe::new("sdnq-v1", "1.0.0", "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567", false);
        let intent = OperationIntent::CleanAnyway;
        let proposal = consent.prepare_proposal(PrepareProposalRequest {
            chapter_id: "c1".into(), page_index: 0, region_id: Some("reg-1".into()),
            target: target.clone(), recipe: recipe.clone(), intent: intent.clone(),
        }, &config, true, &manifest).unwrap();
        let grant = consent.confirm_proposal(ConfirmProposalOptions {
            proposal_id: &proposal.proposal_id, intent: &intent, config: &config,
            cloud_allowed: true, job_path: &manifest,
            grant_ttl: Duration::from_secs(60), max_attempts: 1,
        }).unwrap();
        let mut job = Job::open(&manifest).unwrap();
        job.project.patches[0].geometry_policy = cleaner_core::text_shape::GeometryPolicy::TextShape;
        job.flush().unwrap();
        let before = std::fs::read(&manifest).unwrap();
        let service = InferenceService::new_isolated(
            scratch.join("journal"), cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent, Arc::clone(&grants), Arc::new(SecretManager::new_in_memory()),
        );
        let result = service.execute_cloud_render(&grant.nonce, &manifest, 0, "reg-1",
            &target, &recipe, &intent, &config, true, &PollOptions::default());
        assert!(matches!(result,
            Err(InferenceServiceError::Consent(ConsentError::RenderPrepareFailed(reason)))
            if reason.contains("text-shaped")));
        assert_eq!(grants.get_grant(&grant.nonce).unwrap().attempts_used, 0);
        assert!(service.journal().list_attempt_ids().unwrap().is_empty());
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
    }

    fn test_config() -> InferenceConfig {
        let mut cfg = InferenceConfig::default();
        let (origin, origin_fp) = validate_https_endpoint("https://modal.run/mc/v1").unwrap();
        cfg.modal_profiles.insert(
            "modal-prof-1".to_string(),
            crate::inference::config::CloudProfile {
                id: "modal-prof-1".to_string(),
                name: "Modal Test".to_string(),
                endpoint_url: "https://modal.run/mc/v1".to_string(),
                canonical_origin: origin,
                canonical_origin_fingerprint: origin_fp,
                created_at_ms: 100,
                updated_at_ms: 100,
            },
        );
        cfg
    }

    #[test]
    fn changed_predecessor_inside_read_window_refuses_paid_dispatch_and_preserves_grant() {
        let scratch = test_scratch("changed-predecessor-before-dispatch");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let grants = Arc::new(GrantService::new());
        let consent = Arc::new(ConsentService::new_isolated(Arc::clone(&grants)));
        let target = ExecutionTarget::Modal { profile_id: "modal-prof-1".into() };
        let recipe = RenderRecipe::new("sdnq-v1", "1.0.0", "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567", false);
        let intent = OperationIntent::CleanAnyway;
        let proposal = consent.prepare_proposal(PrepareProposalRequest {
            chapter_id: "chapter".into(), page_index: 0, region_id: Some("reg-1".into()),
            target: target.clone(), recipe: recipe.clone(), intent: intent.clone(),
        }, &config, true, &manifest).unwrap();
        let grant = consent.confirm_proposal(ConfirmProposalOptions {
            proposal_id: &proposal.proposal_id, intent: &intent, config: &config,
            cloud_allowed: true, job_path: &manifest, grant_ttl: Duration::from_secs(60),
            max_attempts: 1,
        }).unwrap();
        let mut job = Job::open(&manifest).unwrap();
        let mut earlier = job.load_patch(&job.project.patches[0]).unwrap();
        earlier.id = "earlier".into();
        earlier.order = 0;
        earlier.mask = Mask::filled(Rect::new(0, 0, 16, 16));
        earlier.ink = earlier.mask.clone();
        earlier.pixels.width = 16;
        earlier.pixels.height = 16;
        earlier.pixels.data = vec![100; 16 * 16];
        job.complete_region(0, &earlier, None).unwrap();
        let endpoint = CloudEndpointTarget::new_test_target(CloudProvider::Modal,
            "modal-prof-1", "http://127.0.0.1:1/mc/v1").unwrap();
        let credential = BoundRuntimeCredential::new(&endpoint, RuntimeCredential::ModalProxy {
            token_id: "tid".into(), token_secret: SecretValue::new("tsec"),
        }).unwrap();
        let client = CloudHttpClient::new_test_client(endpoint, credential,
            reqwest::blocking::Client::new());
        let service = InferenceService::new_isolated(scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(), consent,
            Arc::clone(&grants), Arc::new(SecretManager::new_in_memory())).with_custom_client(client);
        let result = service.execute_cloud_render(&grant.nonce, &manifest, 0, "reg-1",
            &target, &recipe, &intent, &config, true, &PollOptions::default());
        assert!(matches!(result, Err(InferenceServiceError::StaleAttachment(_))));
        assert_eq!(grants.get_grant(&grant.nonce).unwrap().attempts_used, 0);
    }

    fn test_crop_bounds(manifest: &Path, region_id: &str) -> Rect {
        let job = Job::open(manifest).unwrap();
        let patch_rec = job
            .project
            .patches
            .iter()
            .find(|r| r.id == region_id)
            .unwrap();
        let patch = job.load_patch(patch_rec).unwrap();
        let source_path = job.source_path(0).unwrap();
        let source_bytes = std::fs::read(source_path).unwrap();
        let page = cleaner_core::image::decode(&source_bytes).unwrap();
        let noise = fit::page_noise_sigma(&page);
        let edges = EdgeMap::sobel(&page);
        let fitted = fit::fit(&page, &patch.mask, 1.0, noise, &edges, true);
        // The fixtures' recipes name preprocessing 1.0.0.
        let prepared = PreparedRender::prepare_as(&page, &fitted, Preprocessing::V1).unwrap();
        prepared.crop()
    }

    /// Fault Test 1: Ambiguous acceptance is NEVER automatically resubmitted.
    #[test]
    fn test_fault_ambiguous_acceptance_never_resubmitted() {
        let scratch = test_scratch("ambiguous-never-resubmit");
        let (manifest, _bounds) = setup_test_job(&scratch, "reg-1");

        let submit_count = Arc::new(AtomicUsize::new(0));
        let sc_clone = Arc::clone(&submit_count);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Fake gateway: drops connection during POST /mc/v1/jobs to simulate transport disconnect
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let req_str = String::from_utf8_lossy(&buf);

                if req_str.starts_with("POST /mc/v1/jobs") {
                    sc_clone.fetch_add(1, Ordering::SeqCst);
                    // Drop TCP connection immediately before sending 202 Accepted
                    drop(stream);
                    break;
                }
            }
        });

        let endpoint_url = format!("http://127.0.0.1:{port}/mc/v1");
        let config = test_config();

        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());

        let target = ExecutionTarget::Modal {
            profile_id: "modal-prof-1".into(),
        };
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let intent = OperationIntent::CleanAnyway;

        let proposal = consent_service
            .prepare_proposal(
                PrepareProposalRequest {
                    chapter_id: "c1".into(),
                    page_index: 0,
                    region_id: Some("reg-1".into()),
                    target: target.clone(),
                    recipe: recipe.clone(),
                    intent: intent.clone(),
                },
                &config,
                true,
                &manifest,
            )
            .unwrap();

        let grant = consent_service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        // Custom client with loopback target
        let http_target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-prof-1",
            &endpoint_url,
        )
        .unwrap();

        let bound_cred = BoundRuntimeCredential::new(
            &http_target,
            RuntimeCredential::ModalProxy {
                token_id: "tid".into(),
                token_secret: SecretValue::new("tsec"),
            },
        )
        .unwrap();

        let custom_client = CloudHttpClient::new_test_client(
            http_target,
            bound_cred,
            reqwest::blocking::Client::new(),
        );

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            grant_service,
            secret_mgr,
        )
        .with_custom_client(custom_client);

        // Execution fails due to dropped connection during submit
        let exec_res = service.execute_cloud_render(
            &grant.nonce,
            &manifest,
            0,
            "reg-1",
            &target,
            &recipe,
            &intent,
            &config,
            true,
            &PollOptions::default(),
        );
        assert!(exec_res.is_err());

        // Exactly 1 submission attempt occurred
        assert_eq!(submit_count.load(Ordering::SeqCst), 1);

        // Attempt ID derived deterministically
        let attempt_id = format!("att-{}", &sha256_hex(grant.nonce.as_bytes())[0..24]);

        // On recovery: decider marks attempt AmbiguousUnknown and NEVER resubmits
        let recovery = service
            .recover_cloud_attempt(&attempt_id, &manifest, &config, &PollOptions::default())
            .unwrap();

        assert!(matches!(
            recovery,
            RecoveryDecision::AmbiguousUnknown { .. }
        ));
        assert!(!recovery.is_auto_retryable());

        // Gateway received ZERO additional submissions
        assert_eq!(submit_count.load(Ordering::SeqCst), 1);
    }

    /// Fault Test 2: Known handles are reused after polling/download faults without resubmitting.
    #[test]
    fn test_fault_known_handles_reused_after_polling_or_download_faults() {
        let scratch = test_scratch("known-handle-reuse");
        let (manifest, _bounds) = setup_test_job(&scratch, "reg-1");

        let submit_count = Arc::new(AtomicUsize::new(0));
        let poll_count = Arc::new(AtomicUsize::new(0));
        let sc_clone = Arc::clone(&submit_count);
        let pc_clone = Arc::clone(&poll_count);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let crop_bounds = test_crop_bounds(&manifest, "reg-1");

        let raster_res = cleaner_core::image::Raster {
            width: crop_bounds.w,
            height: crop_bounds.h,
            mode: cleaner_core::image::ColorMode::Rgb,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: vec![220; (crop_bounds.w * crop_bounds.h * 3) as usize],
        };
        let res_png = cleaner_core::image::encode(&raster_res, Format::Png).unwrap();
        let res_digest = sha256_hex(&res_png);
        let res_png_clone = res_png.clone();

        let req_info = Arc::new(std::sync::Mutex::new((
            String::new(),
            String::new(),
            String::new(),
        )));
        let req_info_clone = Arc::clone(&req_info);

        thread::spawn(move || {
            let find_field = |req: &str, field: &str| -> String {
                let needle = format!("\"{field}\":\"");
                if let Some(idx) = req.find(&needle) {
                    let rest = &req[idx + needle.len()..];
                    if let Some(end) = rest.find('"') {
                        return rest[..end].to_string();
                    }
                }
                String::new()
            };

            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut tmp) {
                        Ok(n) if n > 0 => n,
                        _ => break,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    let s = String::from_utf8_lossy(&buf);
                    if s.contains("GET ") && s.contains("\r\n\r\n") {
                        break;
                    }
                    if s.contains("POST ") && find_field(&s, "request_digest").len() == 64 {
                        break;
                    }
                    if buf.len() > 65536 {
                        break;
                    }
                }
                let req_str = String::from_utf8_lossy(&buf);

                if req_str.starts_with("POST /mc/v1/jobs") {
                    sc_clone.fetch_add(1, Ordering::SeqCst);
                    let job_id = find_field(&req_str, "job_id");
                    let attempt_id = find_field(&req_str, "attempt_id");
                    let digest = find_field(&req_str, "request_digest");
                    *req_info_clone.lock().unwrap() =
                        (job_id.clone(), attempt_id.clone(), digest.clone());

                    let json = format!(
                        r#"{{
                        "handle": "h-reuse-999",
                        "status": "pending",
                        "job_id": "{job_id}",
                        "attempt_id": "{attempt_id}",
                        "request_digest": "{digest}",
                        "recipe_id": "sdnq-v1",
                        "preprocessing_version": "1.0.0",
                        "model_id": "flux-schnell",
                        "model_revision": "0123456789abcdef0123456789abcdef01234567",
                        "native_mask_conditioning": false
                    }}"#
                    );
                    let resp = format!("HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", json.len(), json);
                    let _ = stream.write_all(resp.as_bytes());
                } else if req_str.starts_with("GET /mc/v1/jobs/h-reuse-999/result") {
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",
                        res_png_clone.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(&res_png_clone);
                } else if req_str.starts_with("GET /mc/v1/jobs/h-reuse-999") {
                    let p = pc_clone.fetch_add(1, Ordering::SeqCst);
                    if p == 0 {
                        // First poll fails with 500 error
                        let resp =
                            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n";
                        let _ = stream.write_all(resp.as_bytes());
                    } else {
                        let (job_id, attempt_id, digest) = req_info_clone.lock().unwrap().clone();
                        // Subsequent poll succeeds with Completed
                        let json = format!(
                            r#"{{
                            "handle": "h-reuse-999",
                            "job_id": "{job_id}",
                            "attempt_id": "{attempt_id}",
                            "request_digest": "{digest}",
                            "recipe_id": "sdnq-v1",
                            "preprocessing_version": "1.0.0",
                            "model_id": "flux-schnell",
                            "model_revision": "0123456789abcdef0123456789abcdef01234567",
                            "native_mask_conditioning": false,
                            "status": "completed",
                            "result_digest": "{}",
                            "result_bytes": {}
                        }}"#,
                            res_digest,
                            res_png_clone.len()
                        );
                        let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", json.len(), json);
                        let _ = stream.write_all(resp.as_bytes());
                    }
                }
            }
        });

        let endpoint_url = format!("http://127.0.0.1:{port}/mc/v1");
        let config = test_config();

        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());

        let target = ExecutionTarget::Modal {
            profile_id: "modal-prof-1".into(),
        };
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let intent = OperationIntent::CleanAnyway;

        let proposal = consent_service
            .prepare_proposal(
                PrepareProposalRequest {
                    chapter_id: "c1".into(),
                    page_index: 0,
                    region_id: Some("reg-1".into()),
                    target: target.clone(),
                    recipe: recipe.clone(),
                    intent: intent.clone(),
                },
                &config,
                true,
                &manifest,
            )
            .unwrap();

        let grant = consent_service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        let http_target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-prof-1",
            &endpoint_url,
        )
        .unwrap();

        let bound_cred = BoundRuntimeCredential::new(
            &http_target,
            RuntimeCredential::ModalProxy {
                token_id: "tid".into(),
                token_secret: SecretValue::new("tsec"),
            },
        )
        .unwrap();

        let custom_client = CloudHttpClient::new_test_client(
            http_target,
            bound_cred,
            reqwest::blocking::Client::new(),
        );

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            grant_service,
            secret_mgr,
        )
        .with_custom_client(custom_client);

        let poll_opts = PollOptions {
            poll_interval: Duration::from_millis(50),
            timeout: Duration::from_secs(5),
            max_polls: 10,
        };

        // Execution survives initial polling 500 error, continues polling, downloads result, and commits
        let api_region = service
            .execute_cloud_render(
                &grant.nonce,
                &manifest,
                0,
                "reg-1",
                &target,
                &recipe,
                &intent,
                &config,
                true,
                &poll_opts,
            )
            .unwrap();

        assert_eq!(api_region.outcome, "cleaned");
        assert_eq!(submit_count.load(Ordering::SeqCst), 1);
        assert!(poll_count.load(Ordering::SeqCst) >= 2);
    }

    /// Fault Test 3: Cancellation acknowledgement is verified to be nonterminal.
    #[test]
    fn test_fault_cancellation_acknowledgement_is_nonterminal() {
        let scratch = test_scratch("cancel-nonterminal");
        let (manifest, _bounds) = setup_test_job(&scratch, "reg-1");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let crop_bounds = test_crop_bounds(&manifest, "reg-1");

        let req_meta = JobRequestMetadata {
            protocol_version: PROTOCOL_VERSION.to_string(),
            job_id: "job-1".to_string(),
            attempt_id: "att-cancel-test".to_string(),
            recipe: WireRenderRecipe {
                qwen_edit: None,
                recipe_id: "sdnq-v1".into(),
                preprocessing_version: "1.0.0".into(),
                model_id: "flux-schnell".into(),
                model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
                native_mask_conditioning: false,
            },
            width: crop_bounds.w,
            height: crop_bounds.h,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            image_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            request_digest: String::new(),
        };
        let request_digest = compute_request_digest(&req_meta);
        let mut req_meta = req_meta;
        req_meta.request_digest = request_digest.clone();
        let digest_clone = request_digest.clone();

        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let req_str = String::from_utf8_lossy(&buf);

                if req_str.starts_with("POST /mc/v1/jobs/h-cancel-123/cancel") {
                    let json = format!(
                        r#"{{
                        "handle": "h-cancel-123",
                        "job_id": "job-1",
                        "attempt_id": "att-cancel-test",
                        "request_digest": "{digest_clone}",
                        "status": "cancel_requested",
                        "acknowledged": true
                    }}"#
                    );
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", json.len(), json);
                    let _ = stream.write_all(resp.as_bytes());
                } else if req_str.starts_with("GET /mc/v1/jobs/h-cancel-123") {
                    let json = format!(
                        r#"{{
                        "handle": "h-cancel-123",
                        "job_id": "job-1",
                        "attempt_id": "att-cancel-test",
                        "request_digest": "{digest_clone}",
                        "recipe_id": "sdnq-v1",
                        "preprocessing_version": "1.0.0",
                        "model_id": "flux-schnell",
                        "model_revision": "0123456789abcdef0123456789abcdef01234567",
                        "native_mask_conditioning": false,
                        "status": "cancelled"
                    }}"#
                    );
                    let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", json.len(), json);
                    let _ = stream.write_all(resp.as_bytes());
                }
            }
        });

        let endpoint_url = format!("http://127.0.0.1:{port}/mc/v1");
        let config = test_config();

        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());

        let http_target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-prof-1",
            &endpoint_url,
        )
        .unwrap();

        let bound_cred = BoundRuntimeCredential::new(
            &http_target,
            RuntimeCredential::ModalProxy {
                token_id: "tid".into(),
                token_secret: SecretValue::new("tsec"),
            },
        )
        .unwrap();

        let custom_client = CloudHttpClient::new_test_client(
            http_target,
            bound_cred,
            reqwest::blocking::Client::new(),
        );

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            grant_service,
            secret_mgr,
        )
        .with_custom_client(custom_client);

        // Manually seed an Accepted attempt in journal
        let create_intent = CreateAttemptIntent {
            qwen_edit: None,
            attempt_id: "att-cancel-test".into(),
            job_id: "job-1".into(),
            provider: CloudProvider::Modal,
            profile_id: "modal-prof-1".into(),
            endpoint_fingerprint:
                "0000000000000000000000000000000000000000000000000000000000000000".into(),
            recipe_id: "sdnq-v1".into(),
            preprocessing_version: "1.0.0".into(),
            model_id: "flux-schnell".into(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            native_mask_conditioning: false,
            width: crop_bounds.w,
            height: crop_bounds.h,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            source_image_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            region_id: "reg-1".into(),
            region_revision: 10,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            request_digest: request_digest.clone(),
            chapter_id: Some("chapter-1".into()),
            page_index: Some(0),
        };

        let (_rec, guard) = service.journal.create_intent(create_intent).unwrap();
        service.journal.mark_dispatching(&guard).unwrap();
        service
            .journal
            .record_accepted_handle(&guard, "h-cancel-123")
            .unwrap();
        drop(guard);

        let decision = service
            .cancel_cloud_render("att-cancel-test", &config)
            .unwrap();

        assert!(matches!(decision, RecoveryDecision::Terminal { .. }));

        let guard = service.journal.acquire_lock("att-cancel-test").unwrap();
        let record = service.journal.read_record_locked(&guard).unwrap();
        assert!(matches!(record.phase, AttemptPhase::Cancelled { .. }));
        assert!(record.is_terminal());
    }

    /// Fault Test 4: Cached results recover via decode validation without resubmitting.
    #[test]
    fn test_fault_cached_results_recover_without_resubmit() {
        let scratch = test_scratch("cached-result-recovery");
        let (manifest, _bounds) = setup_test_job(&scratch, "reg-1");

        let submit_count = Arc::new(AtomicUsize::new(0));
        let sc_clone = Arc::clone(&submit_count);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let req_str = String::from_utf8_lossy(&buf);
                if req_str.starts_with("POST /mc/v1/jobs") {
                    sc_clone.fetch_add(1, Ordering::SeqCst);
                }
            }
        });

        let crop_bounds = test_crop_bounds(&manifest, "reg-1");
        let job = Job::open(&manifest).unwrap();
        let bytes = std::fs::read(job.source_path(0).unwrap()).unwrap();
        let raw = cleaner_core::image::decode(&bytes).unwrap();
        let patch_record = job
            .project
            .patches
            .iter()
            .find(|p| p.id == "reg-1")
            .unwrap();
        let patch = job.load_patch(patch_record).unwrap();
        let cloud = crate::underlay::prepare_cloud(&job, 0, &raw, patch_record, &patch, Preprocessing::V1).unwrap();
        let image_hash = sha256_hex(
            &encode_rgb8_png(
                cloud.prepared.crop().w,
                cloud.prepared.crop().h,
                cloud.prepared.image_rgb8(),
            )
            .unwrap(),
        );
        let hint_hash = sha256_hex(
            &encode_gray8_png(
                cloud.prepared.crop().w,
                cloud.prepared.crop().h,
                cloud.prepared.hint_gray8(),
            )
            .unwrap(),
        );

        let _endpoint_url = format!("http://127.0.0.1:{port}/mc/v1");
        let config = test_config();

        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            grant_service,
            secret_mgr,
        );

        let job = Job::open(&manifest).unwrap();
        let patch_rec = job
            .project
            .patches
            .iter()
            .find(|r| r.id == "reg-1")
            .unwrap();
        let patch = job.load_patch(patch_rec).unwrap();
        let source_path = job.source_path(0).unwrap();
        let source_bytes = std::fs::read(source_path).unwrap();
        let source_hash = sha256_hex(&source_bytes);
        let (revision, _) =
            compute_region_revision_hash(&source_hash, patch_rec, &patch.mask, &patch.ink);

        let raster_res = cleaner_core::image::Raster {
            width: crop_bounds.w,
            height: crop_bounds.h,
            mode: cleaner_core::image::ColorMode::Rgb,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: vec![199; (crop_bounds.w * crop_bounds.h * 3) as usize],
        };
        let res_png = cleaner_core::image::encode(&raster_res, Format::Png).unwrap();
        let res_digest = sha256_hex(&res_png);

        let req_meta = JobRequestMetadata {
            protocol_version: PROTOCOL_VERSION.to_string(),
            job_id: "job-rec-1".to_string(),
            attempt_id: "att-rec-1".to_string(),
            recipe: WireRenderRecipe {
                qwen_edit: None,
                recipe_id: "sdnq-v1".into(),
                preprocessing_version: "1.0.0".into(),
                model_id: "flux-schnell".into(),
                model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
                native_mask_conditioning: false,
            },
            width: crop_bounds.w,
            height: crop_bounds.h,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            image_sha256: image_hash.clone(),
            hint_sha256: hint_hash.clone(),
            request_digest: String::new(),
        };
        let request_digest = compute_request_digest(&req_meta);
        let mut req_meta = req_meta;
        req_meta.request_digest = request_digest.clone();

        let create_intent = CreateAttemptIntent {
            qwen_edit: None,
            attempt_id: "att-rec-1".into(),
            job_id: "job-rec-1".into(),
            provider: CloudProvider::Modal,
            profile_id: "modal-prof-1".into(),
            endpoint_fingerprint:
                "0000000000000000000000000000000000000000000000000000000000000000".into(),
            recipe_id: "sdnq-v1".into(),
            preprocessing_version: "1.0.0".into(),
            model_id: "flux-schnell".into(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            native_mask_conditioning: false,
            width: crop_bounds.w,
            height: crop_bounds.h,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            source_image_hash: source_hash.clone(),
            region_id: "reg-1".into(),
            region_revision: revision,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: image_hash,
            hint_sha256: hint_hash,
            request_digest: request_digest.clone(),
            chapter_id: Some("chapter-1".into()),
            page_index: Some(0),
        };

        let (_rec, guard) = service.journal.create_intent(create_intent).unwrap();
        service.journal.mark_dispatching(&guard).unwrap();
        service
            .journal
            .record_accepted_handle(&guard, "h-rec-1")
            .unwrap();

        let result_meta = ResultMetadata {
            handle: "h-rec-1".into(),
            job_id: "job-rec-1".into(),
            attempt_id: "att-rec-1".into(),
            request_digest: request_digest.clone(),
            recipe_id: "sdnq-v1".into(),
            preprocessing_version: "1.0.0".into(),
            model_id: "flux-schnell".into(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            native_mask_conditioning: false,
            result_digest: res_digest,
            reported_cost_usd: Some(0.01),
            width: crop_bounds.w,
            height: crop_bounds.h,
            byte_length: res_png.len() as u64,
        };

        // Cache result
        service
            .journal
            .cache_result(&guard, &res_png, &result_meta)
            .unwrap();
        drop(guard);

        // Recover attempt: reconciles cached result and commits without resubmitting
        let decision = service
            .recover_cloud_attempt("att-rec-1", &manifest, &config, &PollOptions::default())
            .unwrap();

        assert!(matches!(
            decision,
            RecoveryDecision::AlreadyCommitted { .. }
        ));
        assert_eq!(submit_count.load(Ordering::SeqCst), 0);
    }

    /// Fault Test 5: Stale revision attachment is rejected and cached result is preserved.
    #[test]
    fn test_fault_stale_revision_attachment_rejected_and_result_retained() {
        let scratch = test_scratch("stale-revision-rejection");
        let (manifest, bounds) = setup_test_job(&scratch, "reg-1");

        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            grant_service,
            secret_mgr,
        );

        let job = Job::open(&manifest).unwrap();
        let patch_rec = job
            .project
            .patches
            .iter()
            .find(|r| r.id == "reg-1")
            .unwrap();
        let patch = job.load_patch(patch_rec).unwrap();
        let source_path = job.source_path(0).unwrap();
        let source_bytes = std::fs::read(source_path).unwrap();
        let source_hash = sha256_hex(&source_bytes);
        let (original_revision, _) =
            compute_region_revision_hash(&source_hash, patch_rec, &patch.mask, &patch.ink);

        let crop_bounds = test_crop_bounds(&manifest, "reg-1");

        let raster_res = cleaner_core::image::Raster {
            width: crop_bounds.w,
            height: crop_bounds.h,
            mode: cleaner_core::image::ColorMode::Rgb,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: vec![210; (crop_bounds.w * crop_bounds.h * 3) as usize],
        };
        let res_png = cleaner_core::image::encode(&raster_res, Format::Png).unwrap();
        let res_digest = sha256_hex(&res_png);

        let req_meta = JobRequestMetadata {
            protocol_version: PROTOCOL_VERSION.to_string(),
            job_id: "job-stale-1".to_string(),
            attempt_id: "att-stale-1".to_string(),
            recipe: WireRenderRecipe {
                qwen_edit: None,
                recipe_id: "sdnq-v1".into(),
                preprocessing_version: "1.0.0".into(),
                model_id: "flux-schnell".into(),
                model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
                native_mask_conditioning: false,
            },
            width: crop_bounds.w,
            height: crop_bounds.h,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            image_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            request_digest: String::new(),
        };
        let request_digest = compute_request_digest(&req_meta);
        let mut req_meta = req_meta;
        req_meta.request_digest = request_digest.clone();

        let create_intent = CreateAttemptIntent {
            qwen_edit: None,
            attempt_id: "att-stale-1".into(),
            job_id: "job-stale-1".into(),
            provider: CloudProvider::Modal,
            profile_id: "modal-prof-1".into(),
            endpoint_fingerprint:
                "0000000000000000000000000000000000000000000000000000000000000000".into(),
            recipe_id: "sdnq-v1".into(),
            preprocessing_version: "1.0.0".into(),
            model_id: "flux-schnell".into(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            native_mask_conditioning: false,
            width: crop_bounds.w,
            height: crop_bounds.h,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            source_image_hash: source_hash.clone(),
            region_id: "reg-1".into(),
            region_revision: original_revision,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            request_digest: request_digest.clone(),
            chapter_id: Some("chapter-1".into()),
            page_index: Some(0),
        };

        let (_rec, guard) = service.journal.create_intent(create_intent).unwrap();
        service.journal.mark_dispatching(&guard).unwrap();
        service
            .journal
            .record_accepted_handle(&guard, "h-stale-1")
            .unwrap();

        let result_meta = ResultMetadata {
            handle: "h-stale-1".into(),
            job_id: "job-stale-1".into(),
            attempt_id: "att-stale-1".into(),
            request_digest: request_digest.clone(),
            recipe_id: "sdnq-v1".into(),
            preprocessing_version: "1.0.0".into(),
            model_id: "flux-schnell".into(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            native_mask_conditioning: false,
            result_digest: res_digest,
            reported_cost_usd: Some(0.01),
            width: crop_bounds.w,
            height: crop_bounds.h,
            byte_length: res_png.len() as u64,
        };

        service
            .journal
            .cache_result(&guard, &res_png, &result_meta)
            .unwrap();
        drop(guard);

        // Now modify the project region mask on disk (changing region revision to Drifting)
        {
            let mut job = Job::open(&manifest).unwrap();
            let record = job
                .project
                .patches
                .iter()
                .find(|r| r.id == "reg-1")
                .unwrap();
            let mut patch = job.load_patch(record).unwrap();
            patch.mask = Mask::empty(bounds);
            job.complete_region(0, &patch, None).unwrap();
        }

        let guard = service.journal.acquire_lock("att-stale-1").unwrap();
        let decoded = decode_result_crop(
            &res_png,
            &result_meta,
            &req_meta,
            service.journal.limits(),
            "h-stale-1",
        )
        .unwrap();

        let snapshot = ProjectRegionSnapshot {
            source_image_hash: source_hash.clone(),
            region_id: "reg-1".into(),
            region_revision: original_revision,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        };

        // Attachment fails closed because revision on disk has drifted
        let attach_res = service.attach_patch_to_project(
            &manifest,
            0,
            "reg-1",
            &snapshot,
            &decoded,
            &RenderRecipe::new(
                "sdnq-v1",
                "1.0.0",
                "flux-schnell",
                "0123456789abcdef0123456789abcdef01234567",
                false,
            ),
            CloudProvider::Modal,
            "modal-prof-1",
            "h-stale-1",
            "job-stale-1",
            "att-stale-1",
            &compute_request_digest(&req_meta),
            &result_meta.result_digest,
            None,
            crop_bounds.w,
            crop_bounds.h,
            Some(0),
        );

        assert!(matches!(
            attach_res,
            Err(InferenceServiceError::StaleAttachment(_))
        ));

        // Cached result PNG is preserved intact in the journal directory for review
        let cached_png = service.journal.read_cached_png(&guard).unwrap();
        assert_eq!(cached_png, res_png);
    }

    #[test]
    fn changed_predecessor_rejects_paid_result_without_mutating_target() {
        let scratch = test_scratch("stale-predecessor-attachment");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            Arc::new(ConsentService::new_isolated(Arc::new(GrantService::new()))),
            Arc::new(GrantService::new()),
            Arc::new(SecretManager::new_in_memory()),
        );
        let mut job = Job::open(&manifest).unwrap();
        let record = job
            .project
            .patches
            .iter()
            .find(|r| r.id == "reg-1")
            .unwrap();
        let patch = job.load_patch(record).unwrap();
        let source_bytes = std::fs::read(job.source_path(0).unwrap()).unwrap();
        let source_hash = sha256_hex(&source_bytes);
        let raw = cleaner_core::image::decode(&source_bytes).unwrap();
        let (revision, _) =
            compute_region_revision_hash(&source_hash, record, &patch.mask, &patch.ink);
        let cloud = crate::underlay::prepare_cloud(&job, 0, &raw, record, &patch, Preprocessing::V1).unwrap();
        let crop = cloud.prepared.crop();
        let snapshot = ProjectRegionSnapshot {
            source_image_hash: source_hash,
            region_id: "reg-1".into(),
            region_revision: revision,
            input_sha256: Some(cloud.input.digest.clone()),
            predecessors_sha256: Some(cloud.input.predecessors.clone()),
            crop_sha256: sha256_hex(
                &encode_rgb8_png(crop.w, crop.h, cloud.prepared.image_rgb8()).unwrap(),
            ),
            hint_sha256: sha256_hex(
                &encode_gray8_png(crop.w, crop.h, cloud.prepared.hint_gray8()).unwrap(),
            ),
        };
        let mut a = patch.clone();
        a.id = "earlier".into();
        a.order = 0;
        a.mask = Mask::filled(Rect::new(90, 90, 16, 16));
        a.ink = a.mask.clone();
        a.pixels.width = 16;
        a.pixels.height = 16;
        a.pixels.data = vec![255; 16 * 16];
        job.complete_region(0, &a, None).unwrap();
        let decoded =
            DecodedResultCrop::new(crop.w, crop.h, vec![255; (crop.w * crop.h * 3) as usize])
                .unwrap();
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let result = service.attach_patch_to_project(
            &manifest,
            0,
            "reg-1",
            &snapshot,
            &decoded,
            &recipe,
            CloudProvider::Modal,
            "modal-prof-1",
            "handle",
            "job",
            "attempt",
            "request",
            "result",
            None,
            crop.w,
            crop.h,
            Some(0),
        );
        assert!(matches!(
            result,
            Err(InferenceServiceError::StaleAttachment(_))
        ));
        let after = Job::open(&manifest).unwrap();
        let target = after
            .project
            .patches
            .iter()
            .find(|r| r.id == "reg-1")
            .unwrap();
        assert_eq!(
            after.load_patch(target).unwrap().pixels.data,
            patch.pixels.data
        );
        assert!(target.provenance.cloud.is_none());
    }

    /// Attach one all-white result for `region_id`, bound to the input
    /// [`read_render_input`] reads under `prepared_as` and attached under
    /// `recipe`.
    fn attach_white(
        service: &InferenceService,
        manifest: &Path,
        region_id: &str,
        prepared_as: &RenderRecipe,
        recipe: &RenderRecipe,
    ) -> Result<ApiRegion, InferenceServiceError> {
        let input = read_render_input(manifest, 0, region_id, prepared_as).unwrap();
        let snapshot = ProjectRegionSnapshot {
            source_image_hash: input.source_hash.clone(),
            region_id: region_id.into(),
            region_revision: input.revision,
            input_sha256: Some(input.input_sha256.clone()),
            predecessors_sha256: Some(input.predecessors_sha256.clone()),
            crop_sha256: input.crop_sha256.clone(),
            hint_sha256: input.hint_sha256.clone(),
        };
        let (w, h) = (input.crop_bounds.w, input.crop_bounds.h);
        let decoded = DecodedResultCrop::new(w, h, vec![255; (w * h * 3) as usize]).unwrap();
        service.attach_patch_to_project(
            manifest, 0, region_id, &snapshot, &decoded, recipe, CloudProvider::Modal,
            "modal-prof-1", "handle", "job", "attempt", "request", "result", None, w, h, Some(0),
        )
    }

    fn recipe_at(version: &str) -> RenderRecipe {
        RenderRecipe::new("sdnq-v1", version, "flux-schnell", "0123456789abcdef0123456789abcdef01234567", false)
    }

    fn isolated_service(scratch: &Path) -> InferenceService {
        InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            Arc::new(ConsentService::new_isolated(Arc::new(GrantService::new()))),
            Arc::new(GrantService::new()),
            Arc::new(SecretManager::new_in_memory()),
        )
    }

    /// A result is composited under the preprocessing its attempt was
    /// prepared with: one this build does not know is refused and leaves the
    /// region as it was, and a 1.0.0 result still in flight when the build
    /// moved on attaches as 1.0.0 prepared it, with no tone alignment.
    #[test]
    fn attach_follows_the_attempts_preprocessing_and_refuses_an_unknown_one() {
        let scratch = test_scratch("attach-preprocessing");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let service = isolated_service(&scratch);
        let (v1, unknown) = (recipe_at("1.0.0"), recipe_at("9.9.9"));
        assert!(matches!(
            read_render_input(&manifest, 0, "reg-1", &unknown),
            Err(InferenceServiceError::Consent(ConsentError::InvalidRecipe(_)))
        ));

        let before = Job::open(&manifest).unwrap();
        let record = before.project.patches.iter().find(|r| r.id == "reg-1").unwrap();
        let pixels = before.load_patch(record).unwrap().pixels.data;
        let refused = attach_white(&service, &manifest, "reg-1", &v1, &unknown);
        assert!(matches!(refused, Err(InferenceServiceError::StaleAttachment(_))), "{refused:?}");
        let after = Job::open(&manifest).unwrap();
        let record = after.project.patches.iter().find(|r| r.id == "reg-1").unwrap();
        assert_eq!(after.load_patch(record).unwrap().pixels.data, pixels);
        assert!(record.provenance.cloud.is_none());

        attach_white(&service, &manifest, "reg-1", &v1, &v1).expect("a 1.0.0 result attaches as 1.0.0");
        let after = Job::open(&manifest).unwrap();
        let record = after.project.patches.iter().find(|r| r.id == "reg-1").unwrap();
        assert!(record.provenance.params_snapshot.get("tone").is_none());
        // Straight blend: the white answer is white in the core.
        let patch = after.load_patch(record).unwrap();
        let (cx, cy) = (116 - patch.mask.bounds.x, 116 - patch.mask.bounds.y);
        assert_eq!(patch.pixels.sample(cx as u32, cy as u32, 0), 255);
    }

    /// A stored detection with a text group, its mask and lettering `bbox`.
    fn store_test_detection(manifest: &Path, id: &str, bbox: Rect) {
        let mut job = Job::open(manifest).unwrap();
        job.store_detection(
            cleaner_core::project::DetectedRegion {
                id: id.into(), source_idx: 0, bbox, mask_ref: String::new(), inside: true,
                balloon_color: None, script: Some("ja".into()), pick: "lama".into(), detector: "local".into(),
                created: 1_760_000_000, mask_sha256: String::new(), order: 0, review_state: None, fit: None,
                group: Some(test_group(bbox)),
                padding_from_seed: false,
                padding_px: 0,
            },
            &Mask::filled(bbox),
            &Mask::filled(bbox),
        )
        .unwrap();
    }

    #[test]
    fn unmasked_cloud_inpaint_detection_requests_texture_review_without_replacing_other_reasons() {
        use cleaner_core::fit::Route;
        use cleaner_core::project::DetectionFit;

        const QWEN: &str = "Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32";
        for (name, route, existing, native_mask, model, expected) in [
            ("complex", Route::Inpaint, None, false, "flux-schnell", Some("review.reason.checkGeneratedTexture")),
            (
                "existing",
                Route::Inpaint,
                Some("review.reason.unusuallyLarge"),
                false,
                "flux-schnell",
                Some("review.reason.unusuallyLarge"),
            ),
            ("flat", Route::Fill, None, false, "flux-schnell", None),
            ("masked", Route::Inpaint, None, true, "flux-schnell", None),
            // A Qwen result is flagged whatever its region is.
            ("qwen-flat", Route::Fill, None, false, QWEN, Some("review.reason.checkGeneratedTexture")),
            ("qwen-masked", Route::Inpaint, None, true, QWEN, Some("review.reason.checkGeneratedTexture")),
            ("qwen-existing", Route::Fill, Some("review.reason.unusuallyLarge"), false, QWEN,
                Some("review.reason.unusuallyLarge")),
        ] {
            let scratch = test_scratch(&format!("texture-review-{name}"));
            let (manifest, _) = setup_test_job(&scratch, "reg-1");
            store_test_detection(&manifest, "p-d0", Rect::new(30, 30, 24, 16));
            {
                let mut job = Job::open(&manifest).unwrap();
                let detection = job.project.detections.iter_mut()
                    .find(|row| row.id == "p-d0").unwrap();
                detection.fit = Some(DetectionFit {
                    route,
                    thickness: 2,
                    best_deviation: 0.0,
                    scale: 1.0,
                });
                detection.review_state = existing.map(str::to_owned);
                job.flush().unwrap();
            }
            let service = isolated_service(&scratch);
            let mut recipe = recipe_at(cleaner_core::engines::render::PREPROCESSING_VERSION);
            recipe.native_mask_conditioning = native_mask;
            recipe.model_id = model.into();
            attach_white(&service, &manifest, "p-d0", &recipe, &recipe).unwrap();
            let job = Job::open(&manifest).unwrap();
            let record = job.project.patches.iter().find(|row| row.id == "p-d0").unwrap();
            assert_eq!(record.review_state.as_deref(), expected, "{name}");
            drop(job);

            if name == "qwen-flat" {
                // A result that was not flagged is, once Qwen renders it again.
                attach_white(&service, &manifest, "reg-1", &recipe, &recipe).unwrap();
                let reopened = Job::open(&manifest).unwrap();
                let record = reopened.project.patches.iter().find(|row| row.id == "reg-1").unwrap();
                assert_eq!(record.review_state.as_deref(), expected, "a Qwen rerender is flagged");
            }

            if name == "complex" {
                attach_white(&service, &manifest, "p-d0", &recipe, &recipe).unwrap();
                let reopened = Job::open(&manifest).unwrap();
                let record = reopened.project.patches.iter().find(|row| row.id == "p-d0").unwrap();
                assert_eq!(record.review_state.as_deref(), expected, "rerender retained the review reason");
            }
        }
    }

    /// **A detection's cloud patch keeps the tone it was aligned by**, as a
    /// patch's re-render does, beside the rest of what the attach records.
    #[test]
    fn a_detections_cloud_patch_records_the_tone_it_was_aligned_by() {
        let scratch = test_scratch("detection-tone");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let bbox = Rect::new(30, 30, 24, 16);
        store_test_detection(&manifest, "p-d0", bbox);
        let service = isolated_service(&scratch);
        let v2 = recipe_at(cleaner_core::engines::render::PREPROCESSING_VERSION);
        attach_white(&service, &manifest, "p-d0", &v2, &v2).expect("a 2.0.0 detection result attaches");

        let job = Job::open(&manifest).unwrap();
        assert!(job.project.detections.is_empty());
        let record = job.project.patches.iter().find(|r| r.id == "p-d0").unwrap();
        let snapshot = &record.provenance.params_snapshot;
        assert_eq!(snapshot["tone"]["preprocessing_version"], "2.0.0", "{snapshot}");
        // A white answer on a page of 200 is drifted past the bound.
        assert_eq!(snapshot["tone"]["curves"][0][0][1], -cleaner_core::engines::render::MAX_TONE_OFFSET);
        assert!(snapshot["tone"]["abstained"].is_null());
        assert_eq!(snapshot["group"], test_group(bbox).snapshot());
        assert_eq!(snapshot["write_set"], "ink");
        assert!(snapshot["input_provenance"]["input_sha256"].is_string());
    }

    /// **The stored-mask contract under 2.0.0.** The render removes the
    /// stored lettering, not the patch's applied mask; the attach records the
    /// tone it aligned by; and a second render of the result prepares the
    /// same crop and hint as the first, where 1.0.0 grew the hole by the
    /// isolation radius on every pass.
    #[test]
    fn a_v2_render_removes_the_stored_lettering_and_a_second_render_does_not_grow() {
        let scratch = test_scratch("v2-no-growth");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let ink = Mask::filled(Rect::new(110, 110, 12, 12));
        {
            let mut job = Job::open(&manifest).unwrap();
            let record = job.project.patches.iter().find(|r| r.id == "reg-1").unwrap().clone();
            let mut patch = job.load_patch(&record).unwrap();
            patch.ink = ink.clone();
            job.complete_region(0, &patch, None).unwrap();
        }
        let service = isolated_service(&scratch);
        let v2 = recipe_at(cleaner_core::engines::render::PREPROCESSING_VERSION);
        let applied = ink.dilated(cleaner_core::constants::ISOLATION_RADIUS, 256, 256);

        let first = read_render_input(&manifest, 0, "reg-1", &v2).unwrap();
        assert_eq!(Some(first.crop_bounds), Preprocessing::V2.crop_for(applied.bounds));
        let grown = read_render_input(&manifest, 0, "reg-1", &recipe_at("1.0.0")).unwrap();
        assert!(grown.crop_bounds.w < first.crop_bounds.w, "V1's narrower context");

        attach_white(&service, &manifest, "reg-1", &v2, &v2).expect("a 2.0.0 result attaches");
        let job = Job::open(&manifest).unwrap();
        let record = job.project.patches.iter().find(|r| r.id == "reg-1").unwrap();
        let patch = job.load_patch(record).unwrap();
        assert_eq!(patch.mask, applied, "the write support is the lettering plus the isolation radius");
        assert_eq!(patch.ink, ink, "the lettering is kept for the next render");
        let tone = &record.provenance.params_snapshot["tone"];
        assert_eq!(tone["preprocessing_version"], "2.0.0");
        // A white answer on a page of 200 is drifted past the bound.
        assert_eq!(tone["curves"][0][0][1], -cleaner_core::engines::render::MAX_TONE_OFFSET);

        let second = read_render_input(&manifest, 0, "reg-1", &v2).unwrap();
        assert_eq!(second.crop_bounds, first.crop_bounds);
        assert_eq!(second.hint_sha256, first.hint_sha256);
        assert_eq!(second.mask_hash, first.mask_hash);
    }

    /// **A tone record describes the render that made the pixels.** A patch
    /// aligned under 2.0.0 and then rendered again under 1.0.0, which aligns
    /// nothing, loses the old record instead of keeping a correction no
    /// longer in its pixels.
    #[test]
    fn a_rerender_without_alignment_drops_the_earlier_tone_record() {
        let scratch = test_scratch("stale-tone");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        {
            let mut job = Job::open(&manifest).unwrap();
            let record = job.project.patches.iter_mut().find(|r| r.id == "reg-1").unwrap();
            record.review_state = Some("review.reason.unusuallyLarge".into());
            record.provenance.params_snapshot["layer"] = serde_json::json!({"opacity": 40});
            job.flush().unwrap();
        }
        let service = isolated_service(&scratch);
        let v2 = recipe_at(cleaner_core::engines::render::PREPROCESSING_VERSION);
        attach_white(&service, &manifest, "reg-1", &v2, &v2).expect("a 2.0.0 result attaches");
        let tone = |manifest: &Path| {
            let job = Job::open(manifest).unwrap();
            let record = job.project.patches.iter().find(|r| r.id == "reg-1").unwrap();
            record.provenance.params_snapshot.get("tone").cloned()
        };
        assert_eq!(tone(&manifest).unwrap()["preprocessing_version"], "2.0.0");
        {
            let mut job = Job::open(&manifest).unwrap();
            let record = job.project.patches.iter_mut().find(|r| r.id == "reg-1").unwrap();
            assert_eq!(record.review_state.as_deref(), Some("review.reason.unusuallyLarge"));
            record.review_state = Some("review.reason.inputChanged".into());
            record.provenance.params_snapshot["review_before_input_change"] =
                serde_json::json!("review.reason.unusuallyLarge");
            job.flush().unwrap();
        }

        let v1 = recipe_at("1.0.0");
        attach_white(&service, &manifest, "reg-1", &v1, &v1).expect("a 1.0.0 result attaches");
        assert_eq!(tone(&manifest), None, "a 1.0.0 render kept the 2.0.0 tone record");
        let job = Job::open(&manifest).unwrap();
        let record = job.project.patches.iter().find(|r| r.id == "reg-1").unwrap();
        assert_eq!(record.review_state.as_deref(), Some("review.reason.unusuallyLarge"));
        assert_eq!(record.provenance.params_snapshot["layer"]["opacity"], 40);
        assert!(record.provenance.params_snapshot.get("review_before_input_change").is_none());
    }

    /// Unit Test: Local client construction failure (e.g. missing credentials) leaves grant unconsumed and retryable.
    #[test]
    fn test_grant_not_consumed_on_client_construction_failure() {
        let scratch = test_scratch("no-consume-client-failure");
        let (manifest, _bounds) = setup_test_job(&scratch, "reg-1");

        let config = test_config(); // contains "modal-prof-1"
        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());
        // NOTE: SecretManager intentionally does NOT have the secret stored for modal-prof-1!

        let target = ExecutionTarget::Modal {
            profile_id: "modal-prof-1".into(),
        };
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let intent = OperationIntent::CleanAnyway;

        // Prepare & confirm proposal, issuing a grant with max_attempts = 1
        let proposal = consent_service
            .prepare_proposal(
                PrepareProposalRequest {
                    chapter_id: "c1".into(),
                    page_index: 0,
                    region_id: Some("reg-1".into()),
                    target: target.clone(),
                    recipe: recipe.clone(),
                    intent: intent.clone(),
                },
                &config,
                true,
                &manifest,
            )
            .unwrap();

        let grant = consent_service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            Arc::clone(&grant_service),
            secret_mgr,
        );

        // Execute render: fails because secret is missing during client construction
        let res = service.execute_cloud_render(
            &grant.nonce,
            &manifest,
            0,
            "reg-1",
            &target,
            &recipe,
            &intent,
            &config,
            true,
            &PollOptions::default(),
        );

        assert!(matches!(
            res,
            Err(InferenceServiceError::CredentialMissing(_))
        ));

        // Grant attempt MUST NOT be consumed (attempts_used == 0)
        let grant_state = grant_service.get_grant(&grant.nonce).expect("grant exists");
        assert_eq!(grant_state.attempts_used, 0);

        // Grant is still usable / valid for a retry
        assert!(grant_service
            .validate_and_consume(&grant.nonce, &grant.scope)
            .is_ok());
    }

    /// Unit Test: Local revalidation failure leaves grant unconsumed and retryable.
    #[test]
    fn test_grant_not_consumed_on_local_revalidation_failure() {
        let scratch = test_scratch("no-consume-reval-failure");
        let (manifest, bounds) = setup_test_job(&scratch, "reg-1");

        let config = test_config();
        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());

        // Store valid credential in SecretManager
        let (_, origin_fp) = validate_https_endpoint("https://modal.run/mc/v1").unwrap();
        let key = SecretKey::new(
            CloudProvider::Modal,
            "modal-prof-1".to_string(),
            origin_fp,
            SecretRole::Runtime,
        );
        let compound =
            crate::inference::secrets::encode_modal_runtime_secret("tid", "tsec").unwrap();
        secret_mgr.store_secret(&key, compound, true).unwrap();

        let target = ExecutionTarget::Modal {
            profile_id: "modal-prof-1".into(),
        };
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let intent = OperationIntent::CleanAnyway;

        let proposal = consent_service
            .prepare_proposal(
                PrepareProposalRequest {
                    chapter_id: "c1".into(),
                    page_index: 0,
                    region_id: Some("reg-1".into()),
                    target: target.clone(),
                    recipe: recipe.clone(),
                    intent: intent.clone(),
                },
                &config,
                true,
                &manifest,
            )
            .unwrap();

        let grant = consent_service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        // Mutate the local disk state (e.g. empty mask) to cause local revalidation to fail
        {
            let mut job = Job::open(&manifest).unwrap();
            let patch_rec = job
                .project
                .patches
                .iter()
                .find(|r| r.id == "reg-1")
                .unwrap();
            let mut patch = job.load_patch(patch_rec).unwrap();
            patch.mask = Mask::empty(bounds);
            job.complete_region(0, &patch, None).unwrap();
        }

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            Arc::clone(&grant_service),
            secret_mgr,
        );

        let res = service.execute_cloud_render(
            &grant.nonce,
            &manifest,
            0,
            "reg-1",
            &target,
            &recipe,
            &intent,
            &config,
            true,
            &PollOptions::default(),
        );

        // Should fail during local render preparation
        assert!(res.is_err());

        // Grant attempt MUST NOT be consumed (attempts_used == 0)
        let grant_state = grant_service.get_grant(&grant.nonce).expect("grant exists");
        assert_eq!(grant_state.attempts_used, 0);

        // Grant is still usable / unburned
        assert!(grant_service
            .validate_and_consume(&grant.nonce, &grant.scope)
            .is_ok());
    }

    /// Unit Test: Network dispatch consumes exactly one attempt on the grant.
    #[test]
    fn test_one_grant_consumed_on_dispatch() {
        let scratch = test_scratch("one-consume-on-dispatch");
        let (manifest, _bounds) = setup_test_job(&scratch, "reg-1");

        let submit_count = Arc::new(AtomicUsize::new(0));
        let sc_clone = Arc::clone(&submit_count);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let crop_bounds = test_crop_bounds(&manifest, "reg-1");

        let raster_res = cleaner_core::image::Raster {
            width: crop_bounds.w,
            height: crop_bounds.h,
            mode: cleaner_core::image::ColorMode::Rgb,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: vec![220; (crop_bounds.w * crop_bounds.h * 3) as usize],
        };
        let res_png = cleaner_core::image::encode(&raster_res, Format::Png).unwrap();
        let res_digest = sha256_hex(&res_png);
        let res_png_clone = res_png.clone();

        let req_info = Arc::new(std::sync::Mutex::new((
            String::new(),
            String::new(),
            String::new(),
        )));
        let req_info_clone = Arc::clone(&req_info);

        thread::spawn(move || {
            let find_field = |req: &str, field: &str| -> String {
                let needle = format!("\"{field}\":\"");
                if let Some(idx) = req.find(&needle) {
                    let rest = &req[idx + needle.len()..];
                    if let Some(end) = rest.find('"') {
                        return rest[..end].to_string();
                    }
                }
                String::new()
            };

            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut tmp) {
                        Ok(n) if n > 0 => n,
                        _ => break,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    let s = String::from_utf8_lossy(&buf);
                    if s.contains("GET ") && s.contains("\r\n\r\n") {
                        break;
                    }
                    if s.contains("POST ") && find_field(&s, "request_digest").len() == 64 {
                        break;
                    }
                    if buf.len() > 65536 {
                        break;
                    }
                }
                let req_str = String::from_utf8_lossy(&buf);

                if req_str.starts_with("POST /mc/v1/jobs") {
                    sc_clone.fetch_add(1, Ordering::SeqCst);
                    let job_id = find_field(&req_str, "job_id");
                    let attempt_id = find_field(&req_str, "attempt_id");
                    let digest = find_field(&req_str, "request_digest");
                    *req_info_clone.lock().unwrap() =
                        (job_id.clone(), attempt_id.clone(), digest.clone());

                    let json = format!(
                        r#"{{
                        "handle": "h-test-dispatch-1",
                        "status": "pending",
                        "job_id": "{job_id}",
                        "attempt_id": "{attempt_id}",
                        "request_digest": "{digest}",
                        "recipe_id": "sdnq-v1",
                        "preprocessing_version": "1.0.0",
                        "model_id": "flux-schnell",
                        "model_revision": "0123456789abcdef0123456789abcdef01234567",
                        "native_mask_conditioning": false
                    }}"#
                    );
                    let resp = format!(
                        "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes());
                } else if req_str.contains("GET /mc/v1/jobs/h-test-dispatch-1/result") {
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",
                        res_png_clone.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(&res_png_clone);
                } else if req_str.contains("GET /mc/v1/jobs/h-test-dispatch-1") {
                    let (job_id, attempt_id, digest) = req_info_clone.lock().unwrap().clone();
                    let json = format!(
                        r#"{{
                        "handle": "h-test-dispatch-1",
                        "job_id": "{job_id}",
                        "attempt_id": "{attempt_id}",
                        "request_digest": "{digest}",
                        "recipe_id": "sdnq-v1",
                        "preprocessing_version": "1.0.0",
                        "model_id": "flux-schnell",
                        "model_revision": "0123456789abcdef0123456789abcdef01234567",
                        "native_mask_conditioning": false,
                        "status": "completed",
                        "result_digest": "{}",
                        "result_bytes": {},
                        "reported_cost_usd": 0.01
                    }}"#,
                        res_digest,
                        res_png_clone.len()
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
            }
        });

        let endpoint_url = format!("http://127.0.0.1:{port}/mc/v1");
        let config = test_config();

        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());

        let target = ExecutionTarget::Modal {
            profile_id: "modal-prof-1".into(),
        };
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let intent = OperationIntent::CleanAnyway;

        let proposal = consent_service
            .prepare_proposal(
                PrepareProposalRequest {
                    chapter_id: "c1".into(),
                    page_index: 0,
                    region_id: Some("reg-1".into()),
                    target: target.clone(),
                    recipe: recipe.clone(),
                    intent: intent.clone(),
                },
                &config,
                true,
                &manifest,
            )
            .unwrap();

        let grant = consent_service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        let http_target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-prof-1",
            &endpoint_url,
        )
        .unwrap();

        let bound_cred = BoundRuntimeCredential::new(
            &http_target,
            RuntimeCredential::ModalProxy {
                token_id: "tid".into(),
                token_secret: SecretValue::new("tsec"),
            },
        )
        .unwrap();

        let custom_client = CloudHttpClient::new_test_client(
            http_target,
            bound_cred,
            reqwest::blocking::Client::new(),
        );

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            Arc::clone(&grant_service),
            secret_mgr,
        )
        .with_custom_client(custom_client);

        let res = service.execute_cloud_render(
            &grant.nonce,
            &manifest,
            0,
            "reg-1",
            &target,
            &recipe,
            &intent,
            &config,
            true,
            &PollOptions {
                poll_interval: Duration::from_millis(10),
                timeout: Duration::from_secs(5),
                max_polls: 50,
            },
        );

        match &res {
            Ok(_) => {}
            Err(e) => panic!("expected execute_cloud_render to succeed, got Err: {e:?}"),
        }
        assert_eq!(submit_count.load(Ordering::SeqCst), 1);

        // Grant attempt MUST be consumed exactly once (attempts_used == 1)
        let grant_state = grant_service.get_grant(&grant.nonce).expect("grant exists");
        assert_eq!(grant_state.attempts_used, 1);

        // Reusing the same single-use grant must fail closed with AttemptsExceeded
        assert_eq!(
            grant_service.validate_and_consume(&grant.nonce, &grant.scope),
            Err(GrantError::AttemptsExceeded {
                max_attempts: 1,
                attempts_used: 1,
            })
        );
    }

    fn setup_two_page_service_job(root: &Path, patch_id: &str) -> (PathBuf, Rect) {
        let raws = root.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let mut raster = fixtures::by_name("l8").raster;
        raster.width = 256;
        raster.height = 256;
        raster.data = vec![200; 256 * 256];
        let bytes = cleaner_core::image::encode(&raster, Format::Png).unwrap();
        let page0_path = raws.join("001.png");
        let page1_path = raws.join("002.png");
        std::fs::write(&page0_path, &bytes).unwrap();
        std::fs::write(&page1_path, &bytes).unwrap();

        let source_ref0 = cleaner_core::ingest::source_ref(&page0_path, &bytes).unwrap();
        let source_ref1 = cleaner_core::ingest::source_ref(&page1_path, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = Project::new(
            manifest.parent().unwrap(),
            "test-app",
            StripMode::Single,
            &[source_ref0, source_ref1],
        );
        let mut job = Job::create(&manifest, project).unwrap();

        let bounds = Rect::new(100, 100, 32, 32);
        let mut pixels = raster.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![200; (bounds.w * bounds.h) as usize];

        let patch = Patch {
            id: patch_id.to_string(),
            mask: Mask::filled(bounds),
            ink: Mask::filled(bounds),
            pixels,
            order: 1,
            visible: true,
            provenance: Provenance {
                engine: Engine::Fill,
                engine_version: "1.0".into(),
                model_sha256: None,
                execution_provider: "cpu".into(),
                params_snapshot: serde_json::json!({ "thickness": 2 }),
                mask_sha256: sha256_hex(&vec![255; (bounds.w * bounds.h) as usize]),
                source_sha256: sha256_hex(&bytes),
                cloud: None,
                created: 1_760_000_000,
            },
        };

        job.complete_region(0, &patch, None).unwrap();
        (manifest, bounds)
    }

    #[test]
    fn test_execute_cloud_render_rejects_page_deletion_before_intent_grant_network() {
        let scratch = test_scratch("exec-page-del");
        let (manifest, _bounds) = setup_test_job(&scratch, "reg-del");

        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());
        let config = test_config();

        // Store valid credential in SecretManager so client construction succeeds and pre-POST revalidation is reached
        let (_, origin_fp) = validate_https_endpoint("https://modal.run/mc/v1").unwrap();
        let key = SecretKey::new(
            CloudProvider::Modal,
            "modal-prof-1".to_string(),
            origin_fp,
            SecretRole::Runtime,
        );
        let compound =
            crate::inference::secrets::encode_modal_runtime_secret("tid", "tsec").unwrap();
        secret_mgr.store_secret(&key, compound, true).unwrap();

        let target = ExecutionTarget::Modal {
            profile_id: "modal-prof-1".into(),
        };
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let intent = OperationIntent::CleanAnyway;

        let proposal = consent_service
            .prepare_proposal(
                PrepareProposalRequest {
                    chapter_id: "c1".into(),
                    page_index: 0,
                    region_id: Some("reg-del".into()),
                    target: target.clone(),
                    recipe: recipe.clone(),
                    intent: intent.clone(),
                },
                &config,
                true,
                &manifest,
            )
            .unwrap();

        let grant = consent_service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        let http_target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-prof-1",
            "https://modal.run/mc/v1",
        )
        .unwrap();

        let bound_cred = BoundRuntimeCredential::new(
            &http_target,
            RuntimeCredential::ModalProxy {
                token_id: "tid".into(),
                token_secret: SecretValue::new("tsec"),
            },
        )
        .unwrap();

        let custom_client = CloudHttpClient::new_test_client(
            http_target,
            bound_cred,
            reqwest::blocking::Client::new(),
        );

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            Arc::clone(&grant_service),
            secret_mgr,
        )
        .with_custom_client(custom_client);

        // Execute with deleted/out-of-bounds page_index
        let res = service.execute_cloud_render(
            &grant.nonce,
            &manifest,
            999,
            "reg-del",
            &target,
            &recipe,
            &intent,
            &config,
            true,
            &PollOptions::default(),
        );

        match &res {
            Err(InferenceServiceError::Consent(ConsentError::PageNotFound(999, _))) => {}
            other => panic!("expected ConsentError::PageNotFound(999, _), got: {other:?}"),
        }

        // Invariant: grant is NOT consumed and journal has NO attempt created
        let grant_state = grant_service.get_grant(&grant.nonce).unwrap();
        assert_eq!(grant_state.attempts_used, 0);
        assert!(service.journal().list_attempt_ids().unwrap().is_empty());
    }

    #[test]
    fn test_execute_cloud_render_rejects_page_reorder_and_patch_source_mismatch_before_intent_grant_network(
    ) {
        let scratch = test_scratch("exec-reorder-mismatch");
        let (manifest, _bounds) = setup_two_page_service_job(&scratch, "reg-reorder");

        let grant_service = Arc::new(GrantService::new());
        let consent_service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_service)));
        let secret_mgr = Arc::new(SecretManager::new_in_memory());
        let config = test_config();

        // Store valid credential in SecretManager so client construction succeeds and pre-POST revalidation is reached
        let (_, origin_fp) = validate_https_endpoint("https://modal.run/mc/v1").unwrap();
        let key = SecretKey::new(
            CloudProvider::Modal,
            "modal-prof-1".to_string(),
            origin_fp,
            SecretRole::Runtime,
        );
        let compound =
            crate::inference::secrets::encode_modal_runtime_secret("tid", "tsec").unwrap();
        secret_mgr.store_secret(&key, compound, true).unwrap();

        let target = ExecutionTarget::Modal {
            profile_id: "modal-prof-1".into(),
        };
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let intent = OperationIntent::CleanAnyway;

        let proposal = consent_service
            .prepare_proposal(
                PrepareProposalRequest {
                    chapter_id: "c1".into(),
                    page_index: 0,
                    region_id: Some("reg-reorder".into()),
                    target: target.clone(),
                    recipe: recipe.clone(),
                    intent: intent.clone(),
                },
                &config,
                true,
                &manifest,
            )
            .unwrap();

        let grant = consent_service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        // Reorder pages in manifest so page 0 now resolves to source 1, while region is on source 0
        {
            let mut job = Job::open(&manifest).unwrap();
            job.project.strip.order = vec![1, 0];
            job.flush().unwrap();
        }

        let http_target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-prof-1",
            "https://modal.run/mc/v1",
        )
        .unwrap();

        let bound_cred = BoundRuntimeCredential::new(
            &http_target,
            RuntimeCredential::ModalProxy {
                token_id: "tid".into(),
                token_secret: SecretValue::new("tsec"),
            },
        )
        .unwrap();

        let custom_client = CloudHttpClient::new_test_client(
            http_target,
            bound_cred,
            reqwest::blocking::Client::new(),
        );

        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            consent_service,
            Arc::clone(&grant_service),
            secret_mgr,
        )
        .with_custom_client(custom_client);

        let res = service.execute_cloud_render(
            &grant.nonce,
            &manifest,
            0,
            "reg-reorder",
            &target,
            &recipe,
            &intent,
            &config,
            true,
            &PollOptions::default(),
        );

        match &res {
            Err(InferenceServiceError::Consent(ConsentError::PageMismatch)) => {}
            other => panic!("expected ConsentError::PageMismatch, got: {other:?}"),
        }

        // Invariant: grant is NOT consumed and journal has NO attempt created
        let grant_state = grant_service.get_grant(&grant.nonce).unwrap();
        assert_eq!(grant_state.attempts_used, 0);
        assert!(service.journal().list_attempt_ids().unwrap().is_empty());
    }

    // ------------------------------------------------------------------
    // Interactive render end to end, against a loopback gateway that parses
    // each request properly (headers, then exactly Content-Length bytes).
    // ------------------------------------------------------------------

    const FAKE_HANDLE: &str = "h-fake-1";

    /// How the fake gateway's one job behaves once accepted.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum JobScript {
        /// `pending`, `running`, then `completed`.
        Completes,
        /// `running` until a cancel arrives, then `cancelled`.
        RunsUntilCancelled,
        /// `running` until [`FakeGateway::finish`], then `completed`.
        RunsUntilFinished,
        /// The submission's connection closes before any answer.
        DropsSubmission,
        AcceptedTimeout,
        PendingTimeout,
        WrongIdentityTimeout,
        NotEnqueued(u16),
        /// `failed`, with the worker's own code and message.
        Fails,
        /// The gateway no longer knows the job: every status request is a 404.
        Forgotten,
    }

    /// What the fake gateway's failed job says of itself.
    const FAKE_FAILURE: (&str, &str) = ("edit_not_applied", "Qwen did not make a sufficient edit after three seeds.");

    /// One parsed HTTP/1.1 request.
    pub(crate) struct FakeRequest {
        pub(crate) method: String,
        pub(crate) path: String,
        pub(crate) body: Vec<u8>,
    }

    pub(crate) fn read_fake_request(stream: &mut std::net::TcpStream) -> Option<FakeRequest> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let n = stream.read(&mut chunk).ok().filter(|n| *n > 0)?;
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next()?.split(' ');
        let method = request_line.next()?.to_string();
        let path = request_line.next()?.to_string();
        let length = lines
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buf[header_end..].to_vec();
        while body.len() < length {
            let n = stream.read(&mut chunk).ok().filter(|n| *n > 0)?;
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(length);
        Some(FakeRequest { method, path, body })
    }

    pub(crate) fn fake_respond(
        stream: &mut std::net::TcpStream,
        status: &str,
        content_type: &str,
        body: &[u8],
    ) {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    }

    /// The JSON metadata part of a multipart submission.
    pub(crate) fn submission_metadata(body: &[u8]) -> serde_json::Value {
        let needle = b"{\"protocol_version\"";
        let start = body
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("metadata part in the submission");
        serde_json::Deserializer::from_slice(&body[start..])
            .into_iter::<serde_json::Value>()
            .next()
            .expect("one JSON object")
            .expect("valid metadata JSON")
    }

    /// A result the size the request asked for.
    pub(crate) fn fake_result_png(meta: &serde_json::Value) -> Vec<u8> {
        let width = meta["width"].as_u64().unwrap() as u32;
        let height = meta["height"].as_u64().unwrap() as u32;
        let raster = cleaner_core::image::Raster {
            width,
            height,
            mode: cleaner_core::image::ColorMode::Rgb,
            depth: cleaner_core::image::BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None, color: Default::default(),
            data: vec![220; (width * height * 3) as usize],
        };
        cleaner_core::image::encode(&raster, Format::Png).unwrap()
    }

    /// The identity every job answer repeats back from the request.
    pub(crate) fn fake_job_identity(meta: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "handle": FAKE_HANDLE,
            "job_id": meta["job_id"],
            "attempt_id": meta["attempt_id"],
            "request_digest": meta["request_digest"],
            "recipe_id": meta["recipe"]["recipe_id"],
            "preprocessing_version": meta["recipe"]["preprocessing_version"],
            "model_id": meta["recipe"]["model_id"],
            "model_revision": meta["recipe"]["model_revision"],
            "native_mask_conditioning": false,
        })
    }

    #[derive(Clone, Default)]
    struct GatewayCounters {
        submits: Arc<AtomicUsize>,
        polls: Arc<AtomicUsize>,
        cancels: Arc<AtomicUsize>,
        results: Arc<AtomicUsize>,
        metadata: Arc<Mutex<Option<serde_json::Value>>>,
        finished: Arc<AtomicBool>,
        /// A gateway from before `jobs.retry_seed`: it does not list it.
        old: Arc<AtomicBool>,
    }

    struct FakeGateway {
        endpoint: String,
        seen: GatewayCounters,
    }

    impl FakeGateway {
        fn start(script: JobScript) -> FakeGateway {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = GatewayCounters::default();
            let shared = seen.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let seen = shared.clone();
                    thread::spawn(move || seen.serve(script, &mut stream));
                }
            });
            FakeGateway {
                endpoint: format!("http://127.0.0.1:{port}/mc/v1"),
                seen,
            }
        }

        /// A `RunsUntilFinished` job completes from the next poll on.
        fn finish(&self) {
            self.seen.finished.store(true, Ordering::SeqCst);
        }

        fn client(&self) -> CloudHttpClient {
            self.client_with_timeout(Duration::from_secs(30))
        }

        fn client_with_timeout(&self, timeout: Duration) -> CloudHttpClient {
            let target = CloudEndpointTarget::new_test_target(
                CloudProvider::Modal,
                "modal-prof-1",
                &self.endpoint,
            )
            .unwrap();
            let credential = BoundRuntimeCredential::new(
                &target,
                RuntimeCredential::ModalProxy {
                    token_id: "tid".into(),
                    token_secret: SecretValue::new("tsec"),
                },
            )
            .unwrap();
            CloudHttpClient::new_test_client(target, credential,
                reqwest::blocking::Client::builder().timeout(timeout).build().unwrap())
        }

        fn count(counter: &AtomicUsize) -> usize {
            counter.load(Ordering::SeqCst)
        }
    }

    impl GatewayCounters {
        fn serve(&self, script: JobScript, stream: &mut std::net::TcpStream) {
            let Some(request) = read_fake_request(stream) else {
                return;
            };
            let lookup_script = matches!(script, JobScript::AcceptedTimeout | JobScript::PendingTimeout | JobScript::WrongIdentityTimeout);
            let handle = if lookup_script {
                self.metadata.lock().unwrap().as_ref().map(|meta| {
                    format!("handle-modal-{}", &sha256_hex(format!("{}\0{}",
                        meta["job_id"].as_str().unwrap(), meta["attempt_id"].as_str().unwrap()).as_bytes())[..32])
                }).unwrap_or_default()
            } else { FAKE_HANDLE.to_string() };
            let job = format!("/mc/v1/jobs/{handle}");
            let meta = || {
                self.metadata
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("a submission first")
            };
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/mc/v1/capabilities") => {
                    let mut operations = vec!["jobs.submit", "jobs.status", "jobs.result", "jobs.cancel"];
                    if !self.old.load(Ordering::SeqCst) { operations.push(RETRY_SEED_OPERATION); }
                    let body = serde_json::json!({"protocol_version": "1.0.0", "provider": "modal", "operations": operations});
                    fake_respond(stream, "200 OK", "application/json", body.to_string().as_bytes());
                }
                ("POST", "/mc/v1/jobs") => {
                    self.submits.fetch_add(1, Ordering::SeqCst);
                    if script == JobScript::DropsSubmission {
                        return;
                    }
                    let submitted = submission_metadata(&request.body);
                    if let JobScript::NotEnqueued(status) = script {
                        let body = serde_json::json!({ "enqueued": false, "error_code": if status == 503 { "service_unavailable_pre_queue" } else { "unsupported_recipe" },
                            "message": "GPU was just stopped", "job_id": submitted["job_id"],
                            "attempt_id": submitted["attempt_id"], "request_digest": submitted["request_digest"], "retryable": status == 503 });
                        fake_respond(stream, &format!("{status} Rejected"), "application/json", &serde_json::to_vec(&body).unwrap());
                        return;
                    }
                    let mut accepted = fake_job_identity(&submitted);
                    accepted["status"] = "pending".into();
                    *self.metadata.lock().unwrap() = Some(submitted);
                    if lookup_script {
                        thread::sleep(Duration::from_millis(75));
                        return;
                    }
                    fake_respond(
                        stream,
                        "202 Accepted",
                        "application/json",
                        accepted.to_string().as_bytes(),
                    );
                }
                ("POST", path) if path == format!("{job}/cancel") => {
                    self.cancels.fetch_add(1, Ordering::SeqCst);
                    let meta = meta();
                    let body = serde_json::json!({
                        "handle": FAKE_HANDLE,
                        "job_id": meta["job_id"],
                        "attempt_id": meta["attempt_id"],
                        "request_digest": meta["request_digest"],
                        "status": "cancel_requested",
                        "acknowledged": true,
                    });
                    fake_respond(
                        stream,
                        "200 OK",
                        "application/json",
                        body.to_string().as_bytes(),
                    );
                }
                ("GET", path) if path == format!("{job}/result") => {
                    self.results.fetch_add(1, Ordering::SeqCst);
                    fake_respond(stream, "200 OK", "image/png", &fake_result_png(&meta()));
                }
                ("GET", path) if path == job => {
                    let poll = self.polls.fetch_add(1, Ordering::SeqCst);
                    if script == JobScript::Forgotten {
                        let body = serde_json::json!({"error_code": "not_found", "message": "Job handle not found"});
                        fake_respond(stream, "404 Not Found", "application/json", body.to_string().as_bytes());
                        return;
                    }
                    let cancelled = self.cancels.load(Ordering::SeqCst) > 0;
                    let status = match script {
                        JobScript::Fails => "failed",
                        JobScript::Completes => ["pending", "running", "completed"][poll.min(2)],
                        JobScript::AcceptedTimeout | JobScript::WrongIdentityTimeout => "completed",
                        JobScript::PendingTimeout => "pending",
                        JobScript::RunsUntilCancelled if cancelled => "cancelled",
                        JobScript::RunsUntilFinished if self.finished.load(Ordering::SeqCst) => {
                            "completed"
                        }
                        _ => "running",
                    };
                    let meta = meta();
                    let mut body = fake_job_identity(&meta);
                    body["status"] = status.into();
                    body["handle"] = handle.into();
                    if script == JobScript::WrongIdentityTimeout { body["attempt_id"] = "different-attempt".into(); }
                    if status == "failed" {
                        body["error"] = serde_json::json!({"error_code": FAKE_FAILURE.0, "message": FAKE_FAILURE.1});
                    }
                    if status == "completed" {
                        let png = fake_result_png(&meta);
                        body["result_digest"] = sha256_hex(&png).into();
                        body["result_bytes"] = png.len().into();
                    }
                    fake_respond(
                        stream,
                        "200 OK",
                        "application/json",
                        body.to_string().as_bytes(),
                    );
                }
                _ => fake_respond(stream, "404 Not Found", "application/json", b"{}"),
            }
        }
    }

    #[test]
    fn accepted_timeout_recovers_without_second_post() {
        let fixture = RenderFixture::new("accepted-timeout-recovery");
        let gateway = FakeGateway::start(JobScript::AcceptedTimeout);
        let service = fixture.service(&gateway)
            .with_custom_client(gateway.client_with_timeout(Duration::from_millis(30)));
        assert!(matches!(fixture.render(&service, &PollOptions::default()),
            Err(InferenceServiceError::AmbiguousSubmission(_))));
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Unknown { .. }));
        let (report, waits) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert_eq!(report.needs_attention[0].reason, Some("ambiguous"));
        assert_eq!(waits.len(), 1);
        for _ in 0..3 {
            assert!(matches!(service.recover_cloud_attempt(&fixture.attempt_id(), &fixture.manifest,
                &fixture.config, &PollOptions::default()).unwrap(), RecoveryDecision::AlreadyCommitted { .. }));
        }
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);
        let record = service.journal.get_record(&fixture.attempt_id()).unwrap();
        assert_eq!(record.lookup_evidence.unwrap().status, JobExecutionStatus::Completed);
        assert_eq!(Job::open(&fixture.manifest).unwrap().project.patches.len(), 1);
    }

    #[test]
    fn proven_no_enqueue_and_profile_change_do_not_leave_unknown_or_block_region() {
        for status in [400, 401, 503] {
            let fixture = RenderFixture::new(&format!("not-enqueued-{status}"));
            let gateway = FakeGateway::start(JobScript::NotEnqueued(status));
            let service = fixture.service(&gateway);
            assert!(matches!(fixture.render(&service, &PollOptions::default()), Err(InferenceServiceError::Http(_))));
            assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Cancelled { handle: None, .. }));
            assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);
            assert_eq!(FakeGateway::count(&gateway.seen.polls), 0);
            let record = service.journal.get_record(&fixture.attempt_id()).unwrap();
            assert!(service.journal.unresolved_region_attempt(record.chapter_id.as_deref(), record.page_index, &record.region_id).unwrap().is_none());
        }
        let fixture = RenderFixture::new("epoch-change-after-dispatch");
        let gateway = FakeGateway::start(JobScript::Completes);
        let checks = Arc::new(AtomicUsize::new(0));
        let observed = checks.clone();
        let service = fixture.service(&gateway).with_dispatch_guard(Box::new(move |_, _| observed.fetch_add(1, Ordering::SeqCst) == 0));
        assert!(matches!(fixture.render(&service, &PollOptions::default()), Err(InferenceServiceError::ProfileChanged)));
        assert_eq!(checks.load(Ordering::SeqCst), 2);
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Cancelled { handle: None, .. }));
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 0);
    }

    #[test]
    fn uncertain_lookup_and_changed_endpoint_never_resubmit() {
        for script in [JobScript::PendingTimeout, JobScript::WrongIdentityTimeout, JobScript::DropsSubmission] {
            let fixture = RenderFixture::new("unresolved-timeout-recovery");
            let gateway = FakeGateway::start(script);
            let service = fixture.service(&gateway);
            assert!(fixture.render(&service, &PollOptions::default()).is_err());
            assert!(matches!(service.recover_cloud_attempt(&fixture.attempt_id(), &fixture.manifest,
                &fixture.config, &PollOptions::default()).unwrap(), RecoveryDecision::AmbiguousUnknown { .. }));
            assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Unknown { .. }));
            assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);
        }
        let fixture = RenderFixture::new("changed-endpoint-recovery");
        let gateway = FakeGateway::start(JobScript::AcceptedTimeout);
        let service = fixture.service(&gateway);
        assert!(fixture.render(&service, &PollOptions::default()).is_err());
        let mut config = fixture.config.clone();
        config.modal_profiles.get_mut("modal-prof-1").unwrap().endpoint_url = "https://different.modal.run/mc/v1".into();
        assert!(matches!(service.resolve_unknown_attempt(&fixture.attempt_id(), &config).unwrap(),
            RecoveryDecision::AmbiguousUnknown { .. }));
        assert_eq!(FakeGateway::count(&gateway.seen.polls), 0);
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);
    }

    /// Durable accepted/cache state, built entirely from the local production input.
    fn cached_fixture(fixture: &RenderFixture) -> InferenceService {
        let service = InferenceService::new(&fixture.journal_dir,
            cleaner_core::cloud_wire::provisional_fixture_limits());
        let input = read_render_input(&fixture.manifest, 0, "reg-1", &fixture.recipe).unwrap();
        let mut request = JobRequestMetadata {
            protocol_version: PROTOCOL_VERSION.into(), job_id: "job-local-recovery".into(),
            attempt_id: fixture.attempt_id(), recipe: fixture.recipe.clone().into(),
            width: input.crop_bounds.w, height: input.crop_bounds.h,
            seed: 1, steps: if fixture.recipe.recipe_id == "mc-qwen-image-edit-2511-v4" { 8 } else { 4 }, guidance_scaled: 100,
            image_sha256: input.crop_sha256.clone(), hint_sha256: input.hint_sha256.clone(),
            request_digest: String::new(),
        };
        request.request_digest = compute_request_digest(&request);
        let intent = CreateAttemptIntent {
            qwen_edit: request.recipe.qwen_edit.clone(),
            attempt_id: request.attempt_id.clone(), job_id: request.job_id.clone(),
            provider: CloudProvider::Modal, profile_id: "modal-prof-1".into(),
            endpoint_fingerprint: compute_canonical_endpoint_fingerprint("https://modal.run/mc/v1").unwrap(),
            recipe_id: request.recipe.recipe_id.clone(), preprocessing_version: request.recipe.preprocessing_version.clone(),
            model_id: request.recipe.model_id.clone(), model_revision: request.recipe.model_revision.clone(),
            native_mask_conditioning: false, width: request.width, height: request.height,
            seed: request.seed, steps: request.steps, guidance_scaled: request.guidance_scaled,
            source_image_hash: input.source_hash, region_id: "reg-1".into(), region_revision: input.revision,
            input_sha256: Some(input.input_sha256), predecessors_sha256: Some(input.predecessors_sha256),
            crop_sha256: input.crop_sha256, hint_sha256: input.hint_sha256,
            request_digest: request.request_digest.clone(), chapter_id: Some("chapter".into()), page_index: Some(0),
        };
        let (_, guard) = service.journal.create_intent(intent).unwrap();
        service.journal.mark_dispatching(&guard).unwrap();
        service.journal.record_accepted_handle(&guard, FAKE_HANDLE).unwrap();
        let value = serde_json::to_value(&request).unwrap();
        let png = fake_result_png(&value);
        let mut status = fake_job_identity(&value);
        status["status"] = "completed".into();
        status["result_digest"] = sha256_hex(&png).into();
        status["result_bytes"] = png.len().into();
        let status: JobStatusResponse = serde_json::from_value(status).unwrap();
        let meta = cleaner_core::cloud_decode::result_metadata_from_status(&status, &request, FAKE_HANDLE).unwrap();
        service.journal.cache_result(&guard, &png, &meta).unwrap();
        drop(guard);
        service
    }

    fn qwen_cached_fixture(name: &str) -> (RenderFixture, InferenceService) {
        let mut fixture = RenderFixture::new(name);
        fixture.recipe.recipe_id = "mc-qwen-image-edit-2511-v4".into();
        fixture.recipe.model_id = "Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32".into();
        fixture.recipe.qwen_edit = Some(cleaner_core::engines::render::QwenEdit {
            target: cleaner_core::engines::render::QwenTarget::SoundEffect,
            description: "Letters beside the hand".into(),
        });
        let service = cached_fixture(&fixture);
        (fixture, service)
    }

    /// A cached Qwen result is applied at once, with nobody asked.
    #[test]
    fn qwen_recovery_applies_the_result_at_once_and_retains_the_exact_guidance() {
        let (fixture, service) = qwen_cached_fixture("qwen-unreviewed");
        for _ in 0..2 {
            assert!(matches!(service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap(),
                RecoveryDecision::AlreadyCommitted { .. }));
        }
        let record = service.journal.get_record(&fixture.attempt_id()).unwrap();
        assert!(matches!(record.phase, AttemptPhase::Committed { .. }));
        assert_eq!(record.qwen_edit, fixture.recipe.qwen_edit);
        assert_eq!(compute_request_digest(&record.to_request_metadata()), record.request_digest);
        assert_eq!(fixture.committed_patch().provenance.params_snapshot["qwen_edit"]["description"], "Letters beside the hand");
        assert_eq!(review_state_of(&fixture).as_deref(), Some("review.reason.checkGeneratedTexture"));
        let (fixture, service) = qwen_cached_fixture("qwen-sweep-unreviewed");
        let (report, _) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert_eq!((report.attached.len(), report.needs_attention.len()), (1, 0));
    }

    /// A record an older build left waiting for a review, or marked reviewed,
    /// is applied like any other: nothing reads `review_accepted`.
    #[test]
    fn a_qwen_result_cached_by_a_build_that_asked_first_is_applied() {
        for reviewed in [false, true] {
            let (fixture, service) = qwen_cached_fixture("qwen-old-record");
            let mut record = service.journal.get_record(&fixture.attempt_id()).unwrap();
            record.review_accepted = reviewed;
            let directory = fixture.journal_dir.join("attempts").join(fixture.attempt_id());
            std::fs::write(directory.join("journal.json"), serde_json::to_vec(&record).unwrap()).unwrap();
            assert!(matches!(service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap(),
                RecoveryDecision::AlreadyCommitted { .. }));
        }
    }

    #[test]
    fn qwen_accepted_recovery_downloads_and_applies_the_result() {
        let (fixture, service) = qwen_cached_fixture("qwen-accepted-recovery");
        let mut record = service.journal.get_record(&fixture.attempt_id()).unwrap();
        record.phase = AttemptPhase::Accepted { handle: FAKE_HANDLE.into() };
        let directory = fixture.journal_dir.join("attempts").join(fixture.attempt_id());
        std::fs::write(directory.join("journal.json"), serde_json::to_vec(&record).unwrap()).unwrap();
        std::fs::remove_file(directory.join("result.png")).unwrap();
        std::fs::remove_file(directory.join("result_meta.json")).unwrap();
        let gateway = FakeGateway::start(JobScript::Completes);
        *gateway.seen.metadata.lock().unwrap() = Some(serde_json::to_value(record.to_request_metadata()).unwrap());
        let service = service.with_custom_client(gateway.client());
        assert!(matches!(service.recover_cloud_attempt(&fixture.attempt_id(), &fixture.manifest,
            &fixture.config, &PollOptions::default()).unwrap(), RecoveryDecision::AlreadyCommitted { .. }));
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 0);
        assert_eq!(FakeGateway::count(&gateway.seen.results), 1);
        assert_eq!(review_state_of(&fixture).as_deref(), Some("review.reason.checkGeneratedTexture"));
    }

    #[test]
    fn qwen_cache_written_before_journal_update_is_applied_on_first_recovery() {
        let (fixture, service) = qwen_cached_fixture("qwen-cache-crash");
        let mut record = service.journal.get_record(&fixture.attempt_id()).unwrap();
        // Simulate cache_result's crash boundary: metadata and PNG are
        // durable, but the phase still names the accepted remote job.
        record.phase = AttemptPhase::Accepted { handle: FAKE_HANDLE.into() };
        let directory = fixture.journal_dir.join("attempts").join(fixture.attempt_id());
        std::fs::write(directory.join("journal.json"), serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(matches!(service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap(),
            RecoveryDecision::AlreadyCommitted { .. }));
        assert!(matches!(service.journal.get_record(&fixture.attempt_id()).unwrap().phase,
            AttemptPhase::Committed { .. }));
    }

    /// The app never rejects a result (docs/fixes-2026-10-12/02). A Qwen
    /// render ends committed with nobody asked, reports no review phase, and
    /// its result carries the review flag the user finds it by.
    #[test]
    fn a_qwen_render_is_applied_with_nobody_asked_and_carries_the_review_flag() {
        let mut fixture = RenderFixture::new("qwen-live-render");
        fixture.recipe.recipe_id = "mc-qwen-image-edit-2511-v4".into();
        fixture.recipe.model_id = "Disty0/Qwen-Image-Edit-2511-SDNQ-uint4-svd-r32".into();
        fixture.recipe.qwen_edit = Some(cleaner_core::engines::render::QwenEdit {
            target: cleaner_core::engines::render::QwenTarget::Auto, description: String::new(),
        });
        fixture.nonce = fixture.grant();
        let gateway = FakeGateway::start(JobScript::Completes);
        let service = fixture.service(&gateway);

        let region = fixture.render(&service, &fast_polls()).unwrap();

        assert_eq!(region.outcome, "cleaned");
        assert!(region.mask.as_ref().unwrap().generated_texture_review);
        assert_eq!(fixture.take_phases(), [
            RenderPhase::Preparing, RenderPhase::Submitting, RenderPhase::Queued, RenderPhase::Running,
            RenderPhase::Downloading, RenderPhase::Compositing, RenderPhase::Committed,
        ]);
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Committed { .. }));
        assert_eq!(review_state_of(&fixture).as_deref(), Some("review.reason.checkGeneratedTexture"));
    }

    /// The review reason `reg-1` carries in the chapter.
    fn review_state_of(fixture: &RenderFixture) -> Option<String> {
        let job = Job::open(&fixture.manifest).unwrap();
        job.project.patches.iter().find(|row| row.id == "reg-1").unwrap().review_state.clone()
    }

    fn detection_predecessor(fixture: &RenderFixture) {
        let mut job = Job::open(&fixture.manifest).unwrap();
        job.project.patches.clear();
        let bbox = Rect::new(30, 30, 24, 16);
        job.store_detection(cleaner_core::project::DetectedRegion {
            id: "reg-1".into(), source_idx: 0, bbox, mask_ref: String::new(),
            inside: true, balloon_color: None, script: None, pick: "lama".into(),
            detector: "local".into(), created: 1_760_000_000, mask_sha256: String::new(),
            order: 0, review_state: None, fit: None, group: None,
            padding_from_seed: false,
            padding_px: 0,
        }, &Mask::filled(bbox), &Mask::filled(bbox)).unwrap();
    }

    #[test]
    fn interrupted_dispatch_report_offers_confirmed_abandonment() {
        let fixture = RenderFixture::new("abandon-interrupted-dispatch");
        let service = cached_fixture(&fixture);
        let mut record = service.journal.get_record(&fixture.attempt_id()).unwrap();
        record.phase = AttemptPhase::Dispatching;
        let directory = fixture.journal_dir.join("attempts").join(fixture.attempt_id());
        std::fs::write(directory.join("journal.json"), serde_json::to_vec(&record).unwrap()).unwrap();
        let (report, waits) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert_eq!(waits.len(), 1);
        assert_eq!(report.needs_attention[0].reason, Some("ambiguous"));
        assert!(report.needs_attention[0].can_abandon);
        assert!(crate::inference::commands::apply_recovery_action(&service.journal, &fixture.attempt_id(), "abandon", false).is_err());
        crate::inference::commands::apply_recovery_action(&service.journal, &fixture.attempt_id(), "abandon", true).unwrap();
        assert!(directory.join("result.png").exists());
        let (report, waits) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert!(report.needs_attention.is_empty());
        assert!(waits.is_empty());
        assert!(service.journal.unresolved_region_attempt(Some("chapter"), Some(0), "reg-1").unwrap().is_none());
    }

    #[test]
    fn startup_skips_incomplete_create_and_separates_load_error_from_corruption() {
        let fixture = RenderFixture::new("incomplete-create");
        let service = cached_fixture(&fixture);
        service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap();
        std::fs::create_dir_all(fixture.journal_dir.join("attempts/incomplete-attempt")).unwrap();
        let (report, _) = crate::inference::commands::recover_all_attempts(&service, &|_| None);
        assert!(report.needs_attention.is_empty(), "deleted chapters and incomplete creates are not corruption");
        std::fs::write(&fixture.manifest, b"invalid manifest").unwrap();
        let (report, _) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert_eq!(report.needs_attention.len(), 1);
        assert_eq!(report.needs_attention[0].reason, Some("load_error"));
    }

    #[test]
    fn committed_audit_reuses_manifest_marker_without_hashing_or_chapter_lock() {
        let fixture = RenderFixture::new("audit-cache");
        let service = cached_fixture(&fixture);
        service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap();
        let snapshot = RecoveryAuditSnapshot::load(&fixture.manifest).unwrap();
        let _editor_lock = run::lock_job(&fixture.manifest).unwrap();
        service.recover_attempt_with_snapshot(&fixture.attempt_id(), Some(&fixture.manifest), Some(&snapshot)).unwrap();
        assert_eq!(snapshot.source_hashes.lock().unwrap().len(), 1);
        let restarted = RecoveryAuditSnapshot::load(&fixture.manifest).unwrap();
        service.recover_attempt_with_snapshot(&fixture.attempt_id(), Some(&fixture.manifest), Some(&restarted)).unwrap();
        assert!(restarted.source_hashes.lock().unwrap().is_empty());
    }

    #[test]
    fn normal_region_actions_supersede_committed_attempt_and_repair_ack_survives_restart() {
        for action in ["local", "redetect", "delete", "undo", "repair"] {
            let fixture = RenderFixture::new(action);
            detection_predecessor(&fixture);
            let predecessor = std::fs::read(&fixture.manifest).unwrap();
            let service = cached_fixture(&fixture);
            service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap();
            let mut job = Job::open(&fixture.manifest).unwrap();
            match action {
                "local" => { job.project.patches[0].provenance.cloud = None; job.flush().unwrap(); }
                "delete" => { job.project.patches.clear(); job.flush().unwrap(); }
                "redetect" => {
                    detection_predecessor(&fixture);
                    let mut job = Job::open(&fixture.manifest).unwrap();
                    job.project.detections[0].created += 1;
                    job.flush().unwrap();
                }
                "undo" => {
                    let mut history = crate::history::load(&fixture.manifest);
                    history.entries.push(crate::history::Delta {
                        seq: 1, label: "undo".into(), op: "region-state".into(), region_id: "reg-1".into(),
                        before: crate::history::Side { present: true, page_status: None, region: None },
                        after: crate::history::Side { present: true, page_status: None,
                            region: Some(serde_json::to_value(&job.project.patches[0]).unwrap()) },
                    });
                    history.cursor = 0;
                    crate::history::save(&fixture.manifest, &history).unwrap();
                    std::fs::write(&fixture.manifest, &predecessor).unwrap();
                }
                _ => { std::fs::write(&fixture.manifest, &predecessor).unwrap(); }
            }
            let (report, _) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
            assert_eq!(report.needs_attention.len(), usize::from(action == "repair"), "{action}");
            if action == "repair" {
                crate::inference::commands::apply_recovery_action(&service.journal, &fixture.attempt_id(), "acknowledge", false).unwrap();
                let restarted = InferenceService::new(&fixture.journal_dir, cleaner_core::cloud_wire::provisional_fixture_limits());
                let (report, _) = crate::inference::commands::recover_all_attempts(&restarted, &|_| Some(fixture.manifest.clone()));
                assert!(report.needs_attention.is_empty());
                assert!(matches!(fixture.phase_on_disk(&restarted), AttemptPhase::Committed { .. }));
            }
        }
    }

    #[test]
    fn every_attachment_boundary_recovers_once_and_audits_commit_loss() {
        for boundary in ["cache_result", "prepare_attachment", "manifest_write", "confirm_committed"] {
            let fixture = RenderFixture::new(boundary);
            detection_predecessor(&fixture);
            let predecessor = std::fs::read(&fixture.manifest).unwrap();
            let mut service = cached_fixture(&fixture);
            service.attachment_fault = Some(boundary);
            if boundary != "cache_result" {
                let guard = service.journal.acquire_lock(&fixture.attempt_id()).unwrap();
                let crop = service.journal.read_cached_crop(&guard).unwrap();
                assert!(service.attach_cached(&guard, &fixture.manifest, None, &crop).is_err());
            }
            // A new service has no in-memory knowledge of the failed attachment.
            let service = InferenceService::new(&fixture.journal_dir,
                cleaner_core::cloud_wire::provisional_fixture_limits());
            for _ in 0..3 {
                assert!(matches!(service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap(), RecoveryDecision::AlreadyCommitted { .. }));
            }
            assert_eq!(Job::open(&fixture.manifest).unwrap().project.patches.len(), 1);
            let (healthy, _) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
            assert!(healthy.needs_attention.is_empty());
            assert!(healthy.attached.is_empty());
            // Inject the observed manifest rollback, retaining all revision buffers.
            std::fs::write(&fixture.manifest, predecessor).unwrap();
            let original_manifest = std::fs::read(&fixture.manifest).unwrap();
            let (report, waits) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
            assert_eq!(report.needs_attention.len(), 1);
            assert_eq!(report.needs_attention[0].reason, Some("repair_needed"));
            assert!(waits.is_empty());
            assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Committed { .. }));
            let guard = service.journal.acquire_lock(&fixture.attempt_id()).unwrap();
            service.journal.read_cached_crop(&guard).unwrap();
            assert_eq!(std::fs::read(&fixture.manifest).unwrap(), original_manifest);
        }
    }

    /// The journal read behind the region flags, which runs with the cloud
    /// off: a result cached and never applied is `cloudResultNotApplied`,
    /// never `repairNeeded`; a committed result the manifest lost is
    /// `repairNeeded`; a chapter that cannot be read to check is
    /// `cloudResultUnchecked`; an acknowledged attempt flags nothing. Reading
    /// attaches nothing and moves no attempt on.
    #[test]
    fn cloud_attention_names_only_a_lost_commit_as_repair_needed() {
        use crate::inference::commands::scan_cloud_attention;
        let fixture = RenderFixture::new("attention-scan");
        detection_predecessor(&fixture);
        let predecessor = std::fs::read(&fixture.manifest).unwrap();
        let service = cached_fixture(&fixture);
        let id = fixture.attempt_id();
        let flags = |service: &InferenceService| scan_cloud_attention(service, &|_| Some(fixture.manifest.clone())).flags;

        assert_eq!(flags(&service), [("reg-1".to_owned(), id.clone(), "review.reason.cloudResultNotApplied")]);
        assert_eq!(std::fs::read(&fixture.manifest).unwrap(), predecessor, "reading attaches nothing");
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::ResultCached { .. }));

        service.recover_attempt_locally(&id, Some(&fixture.manifest)).unwrap();
        assert!(flags(&service).is_empty(), "a committed result the region shows is not flagged");

        std::fs::write(&fixture.manifest, &predecessor).unwrap();
        let restarted = InferenceService::new(&fixture.journal_dir, cleaner_core::cloud_wire::provisional_fixture_limits());
        assert_eq!(flags(&restarted), [("reg-1".to_owned(), id.clone(), "review.reason.repairNeeded")]);
        assert!(scan_cloud_attention(&restarted, &|_| None).flags.is_empty(), "no chapter, no region");

        std::fs::write(&fixture.manifest, b"invalid manifest").unwrap();
        assert_eq!(flags(&restarted), [("reg-1".to_owned(), id.clone(), "review.reason.cloudResultUnchecked")]);

        std::fs::write(&fixture.manifest, &predecessor).unwrap();
        crate::inference::commands::apply_recovery_action(&restarted.journal, &id, "acknowledge", false).unwrap();
        assert!(flags(&restarted).is_empty());
        assert!(matches!(fixture.phase_on_disk(&restarted), AttemptPhase::Committed { .. }));
    }

    #[test]
    fn committed_successor_is_valid_but_rollback_to_older_attempt_needs_repair() {
        let mut fixture = RenderFixture::new("committed-successor-audit");
        let service = cached_fixture(&fixture);
        let first = fixture.attempt_id();
        service.recover_attempt_locally(&first, Some(&fixture.manifest)).unwrap();
        let first_manifest = std::fs::read(&fixture.manifest).unwrap();
        fixture.nonce.push_str("-second");
        let service = cached_fixture(&fixture);
        service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap();
        let (report, _) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert!(report.needs_attention.is_empty());
        std::fs::write(&fixture.manifest, first_manifest).unwrap();
        let (report, _) = crate::inference::commands::recover_all_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert_eq!(report.needs_attention.len(), 1);
        assert_eq!(report.needs_attention[0].attempt_id, fixture.attempt_id());
        assert_eq!(report.needs_attention[0].reason, Some("repair_needed"));
    }

    /// An earlier patch lands under `reg-1`: its underlay, and so the input
    /// the cached result was rendered from, is no longer what the page holds.
    fn change_underlay(fixture: &RenderFixture) {
        let mut job = Job::open(&fixture.manifest).unwrap();
        let mut earlier = job.load_patch(&job.project.patches[0]).unwrap();
        earlier.id = "earlier".into();
        earlier.order = 0;
        earlier.mask = Mask::filled(Rect::new(0, 0, 16, 16));
        earlier.ink = earlier.mask.clone();
        earlier.pixels.width = 16;
        earlier.pixels.height = 16;
        earlier.pixels.data = vec![100; 16 * 16];
        job.complete_region(0, &earlier, None).unwrap();
    }

    /// `cached_fixture` one step on: the attach was prepared and did not finish.
    fn pending_fixture(fixture: &RenderFixture) -> InferenceService {
        let service = cached_fixture(fixture);
        let guard = service.journal.acquire_lock(&fixture.attempt_id()).unwrap();
        let record = service.journal.read_record_locked(&guard).unwrap();
        service.journal.prepare_attachment(&guard, &ProjectRegionSnapshot {
            source_image_hash: record.source_image_hash.clone(), region_id: record.region_id.clone(),
            region_revision: record.region_revision, input_sha256: record.input_sha256.clone(),
            predecessors_sha256: record.predecessors_sha256.clone(), crop_sha256: record.crop_sha256.clone(),
            hint_sha256: record.hint_sha256.clone(),
        }, "reg-1").unwrap();
        assert!(matches!(service.journal.read_record_locked(&guard).unwrap().phase, AttemptPhase::AttachmentPending { .. }));
        drop(guard);
        service
    }

    /// `cached_fixture` one step back: accepted, with no result on disk yet.
    /// The gateway is told the request, as if it had taken the submission.
    fn accepted_fixture(fixture: &RenderFixture, gateway: &FakeGateway) -> InferenceService {
        let service = cached_fixture(fixture);
        let mut record = service.journal.get_record(&fixture.attempt_id()).unwrap();
        record.phase = AttemptPhase::Accepted { handle: FAKE_HANDLE.into() };
        let directory = fixture.journal_dir.join("attempts").join(fixture.attempt_id());
        std::fs::write(directory.join("journal.json"), serde_json::to_vec(&record).unwrap()).unwrap();
        std::fs::remove_file(directory.join("result.png")).unwrap();
        std::fs::remove_file(directory.join("result_meta.json")).unwrap();
        *gateway.seen.metadata.lock().unwrap() = Some(serde_json::to_value(record.to_request_metadata()).unwrap());
        service.with_custom_client(gateway.client())
    }

    fn sweep(fixture: &RenderFixture, service: &InferenceService)
        -> (crate::inference::commands::CloudRecoveryReportDto, Vec<(String, PathBuf)>) {
        crate::inference::commands::recover_all_attempts(service, &|_| Some(fixture.manifest.clone()))
    }

    /// The end the sweep gave an attempt, and that its region is free: the
    /// question a cloud clean's plan asks of the journal.
    fn assert_ended(fixture: &RenderFixture, service: &InferenceService, code: &str) {
        let phase = fixture.phase_on_disk(service);
        assert!(matches!(&phase, AttemptPhase::Failed { error_code, .. } if error_code == code), "{phase:?}");
        assert!(service.journal.unresolved_region_attempt(Some("chapter"), Some(0), "reg-1").unwrap().is_none());
    }

    fn cached_files(fixture: &RenderFixture) -> [bool; 2] {
        let directory = fixture.journal_dir.join("attempts").join(fixture.attempt_id());
        [directory.join("result.png").exists(), directory.join("result_meta.json").exists()]
    }

    // The sweep's table (docs/cloud-journal.md 4.3), one test per row. After
    // it no attempt is unresolved except an unknown one.

    /// Row `result_cached`: it goes on to attach, or ends when it never can.
    #[test]
    fn sweep_attaches_a_cached_result_or_ends_one_whose_region_changed() {
        let fixture = RenderFixture::new("sweep-cached-attach");
        let service = cached_fixture(&fixture);
        let (report, waits) = sweep(&fixture, &service);
        assert_eq!((report.attached.len(), report.released.len(), report.needs_attention.len(), waits.len()), (1, 0, 0, 0));
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Committed { .. }));

        let fixture = RenderFixture::new("sweep-cached-changed");
        let service = cached_fixture(&fixture);
        change_underlay(&fixture);
        let before = std::fs::read(&fixture.manifest).unwrap();
        let (report, waits) = sweep(&fixture, &service);
        assert_eq!((report.attached.len(), report.released.len(), report.needs_attention.len(), waits.len()), (0, 1, 0, 0));
        assert_eq!(report.released[0].attempt_id, fixture.attempt_id());
        assert_eq!(std::fs::read(&fixture.manifest).unwrap(), before, "nothing was applied");
        assert_ended(&fixture, &service, "region_changed");
        assert_eq!(cached_files(&fixture), [false, false], "the result that cannot be applied is dropped");
        // Ended: a restart has nothing more to say about it.
        let restarted = InferenceService::new(&fixture.journal_dir, cleaner_core::cloud_wire::provisional_fixture_limits());
        let (report, waits) = sweep(&fixture, &restarted);
        assert!(report.released.is_empty() && report.needs_attention.is_empty() && waits.is_empty());
    }

    /// Row `attachment_pending`: one try at the attach.
    #[test]
    fn sweep_tries_a_pending_attach_once_and_ends_one_whose_region_changed() {
        let fixture = RenderFixture::new("sweep-pending-attach");
        let service = pending_fixture(&fixture);
        let (report, _) = sweep(&fixture, &service);
        assert_eq!((report.attached.len(), report.released.len(), report.needs_attention.len()), (1, 0, 0));
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Committed { .. }));

        let fixture = RenderFixture::new("sweep-pending-changed");
        let service = pending_fixture(&fixture);
        change_underlay(&fixture);
        let before = std::fs::read(&fixture.manifest).unwrap();
        let (report, _) = sweep(&fixture, &service);
        assert_eq!((report.attached.len(), report.released.len(), report.needs_attention.len()), (0, 1, 0));
        assert_eq!(std::fs::read(&fixture.manifest).unwrap(), before);
        assert_ended(&fixture, &service, "region_changed");
        assert_eq!(cached_files(&fixture), [false, false]);
    }

    /// A chapter that is gone is a page that changed, for a result and for a
    /// job still accepted: there is nowhere to put either.
    #[test]
    fn sweep_ends_every_attempt_of_a_chapter_that_is_gone() {
        let gone = |service: &InferenceService| crate::inference::commands::recover_all_attempts(service, &|_| None);
        let fixture = RenderFixture::new("sweep-gone-pending");
        let service = pending_fixture(&fixture);
        let (report, waits) = gone(&service);
        assert_eq!((report.released.len(), report.needs_attention.len(), waits.len()), (1, 0, 0));
        assert_ended(&fixture, &service, "region_changed");

        let fixture = RenderFixture::new("sweep-gone-accepted");
        let gateway = FakeGateway::start(JobScript::Completes);
        let service = accepted_fixture(&fixture, &gateway);
        let (report, waits) = gone(&service);
        assert_eq!((report.released.len(), report.needs_attention.len(), waits.len()), (1, 0, 0));
        assert_ended(&fixture, &service, "region_changed");
        assert_eq!(FakeGateway::count(&gateway.seen.polls), 0, "nothing is fetched for a chapter that is gone");
    }

    /// The reason the 20 requests of 2026-10-12 stayed stuck: a dismissed
    /// notice kept its attempt out of the sweep. The notice stays dismissed
    /// and the attempt is ended all the same.
    #[test]
    fn sweep_ends_an_attempt_whose_notice_was_dismissed() {
        let fixture = RenderFixture::new("sweep-dismissed");
        let service = pending_fixture(&fixture);
        change_underlay(&fixture);
        crate::inference::commands::apply_recovery_action(&service.journal, &fixture.attempt_id(), "acknowledge", false).unwrap();
        let (report, _) = sweep(&fixture, &service);
        assert_eq!((report.released.len(), report.needs_attention.len()), (1, 0));
        assert_ended(&fixture, &service, "region_changed");

        // An unknown submission whose notice was dismissed is not reported
        // again, and is still the user's to decide: nothing ends it.
        let fixture = RenderFixture::new("sweep-dismissed-unknown");
        let service = cached_fixture(&fixture);
        let mut record = service.journal.get_record(&fixture.attempt_id()).unwrap();
        record.phase = AttemptPhase::Dispatching;
        std::fs::write(fixture.journal_dir.join("attempts").join(fixture.attempt_id()).join("journal.json"),
            serde_json::to_vec(&record).unwrap()).unwrap();
        crate::inference::commands::apply_recovery_action(&service.journal, &fixture.attempt_id(), "acknowledge", false).unwrap();
        let (report, _) = sweep(&fixture, &service);
        assert!(report.needs_attention.is_empty() && report.released.is_empty());
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Unknown { .. }));
    }

    /// Row `unknown`: the one case left to the user. It is reported, never
    /// resent and never ended, and it still holds its region.
    #[test]
    fn sweep_leaves_an_unknown_submission_to_the_user() {
        let fixture = RenderFixture::new("sweep-unknown");
        let service = cached_fixture(&fixture);
        let mut record = service.journal.get_record(&fixture.attempt_id()).unwrap();
        record.phase = AttemptPhase::Dispatching;
        std::fs::write(fixture.journal_dir.join("attempts").join(fixture.attempt_id()).join("journal.json"),
            serde_json::to_vec(&record).unwrap()).unwrap();
        for _ in 0..2 {
            let (report, _) = sweep(&fixture, &service);
            assert_eq!(report.needs_attention.len(), 1);
            assert_eq!(report.needs_attention[0].reason, Some("ambiguous"));
            assert!(report.released.is_empty() && report.attached.is_empty());
            assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Unknown { .. }));
        }
        assert_eq!(service.journal.unresolved_region_attempt(Some("chapter"), Some(0), "reg-1").unwrap(),
            Some(fixture.attempt_id()));
    }

    /// Row `accepted`: the gateway is asked. A job it failed, no longer knows,
    /// or that sits on an endpoint no longer configured ends as failed, with
    /// the reason kept. (A job it completed is downloaded and attached:
    /// `a_job_that_outlives_the_app_is_attached_after_a_restart`.)
    #[test]
    fn sweep_ends_an_accepted_job_the_gateway_failed_or_no_longer_knows() {
        for (script, code) in [(JobScript::Fails, FAKE_FAILURE.0), (JobScript::Forgotten, "remote_job_not_found")] {
            let fixture = RenderFixture::new(&format!("sweep-accepted-{code}"));
            let gateway = FakeGateway::start(script);
            let service = accepted_fixture(&fixture, &gateway);
            let (report, waits) = sweep(&fixture, &service);
            assert_eq!((report.still_running.len(), report.released.len()), (1, 0), "the gateway is asked first");
            assert_eq!(waits, [(fixture.attempt_id(), fixture.manifest.clone())]);
            // The wait the sweep starts for it.
            let decision = service.recover_cloud_attempt(&fixture.attempt_id(), &fixture.manifest, &fixture.config, &fast_polls()).unwrap();
            assert!(matches!(decision, RecoveryDecision::Terminal { .. }), "{decision:?}");
            assert_ended(&fixture, &service, code);
            assert_eq!(FakeGateway::count(&gateway.seen.submits), 0, "nothing is sent again");
            let (report, waits) = sweep(&fixture, &service);
            assert!(report.still_running.is_empty() && report.needs_attention.is_empty() && waits.is_empty());
        }
        let fixture = RenderFixture::new("sweep-accepted-failed-reason");
        let gateway = FakeGateway::start(JobScript::Fails);
        let service = accepted_fixture(&fixture, &gateway);
        service.recover_cloud_attempt(&fixture.attempt_id(), &fixture.manifest, &fixture.config, &fast_polls()).unwrap();
        assert!(matches!(fixture.phase_on_disk(&service),
            AttemptPhase::Failed { message, .. } if message == FAKE_FAILURE.1), "the gateway's own message is kept");

        let fixture = RenderFixture::new("sweep-accepted-profile-removed");
        let gateway = FakeGateway::start(JobScript::Completes);
        let service = accepted_fixture(&fixture, &gateway);
        let mut config = fixture.config.clone();
        config.modal_profiles.remove("modal-prof-1");
        let decision = service.recover_cloud_attempt(&fixture.attempt_id(), &fixture.manifest, &config, &fast_polls()).unwrap();
        assert!(matches!(decision, RecoveryDecision::Terminal { .. }), "{decision:?}");
        assert_ended(&fixture, &service, "profile_removed");
        assert_eq!(FakeGateway::count(&gateway.seen.polls), 0);
    }

    /// A live render is not a recovery: one "no such job" answer to a job
    /// just accepted does not end it.
    #[test]
    fn a_live_render_is_not_failed_by_a_gateway_that_does_not_know_its_job_yet() {
        let fixture = RenderFixture::new("live-not-found");
        let gateway = FakeGateway::start(JobScript::Forgotten);
        let service = fixture.service(&gateway);
        let short = PollOptions { poll_interval: Duration::from_millis(10), timeout: Duration::from_millis(200), max_polls: 1000 };
        assert!(matches!(fixture.render(&service, &short), Err(InferenceServiceError::PollingTimeout(_))));
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Accepted { .. }));
    }

    /// A damaged cached result ends its attempt and is kept as evidence. A
    /// chapter that cannot be read just now ends nothing: the result waits for
    /// the next sweep that can read it.
    #[test]
    fn sweep_ends_a_damaged_result_and_keeps_one_whose_chapter_cannot_be_read() {
        let fixture = RenderFixture::new("sweep-corrupt");
        let service = cached_fixture(&fixture);
        let png = fixture.journal_dir.join("attempts").join(fixture.attempt_id()).join("result.png");
        std::fs::write(&png, b"not the result").unwrap();
        let (report, _) = sweep(&fixture, &service);
        assert_eq!((report.released.len(), report.needs_attention.len()), (1, 0));
        assert_ended(&fixture, &service, "result_corrupt");
        assert_eq!(cached_files(&fixture), [true, true]);

        let fixture = RenderFixture::new("sweep-unreadable-chapter");
        let service = cached_fixture(&fixture);
        let manifest = std::fs::read(&fixture.manifest).unwrap();
        std::fs::write(&fixture.manifest, b"invalid manifest").unwrap();
        let (report, _) = sweep(&fixture, &service);
        assert_eq!(report.needs_attention.len(), 1);
        assert_eq!(report.needs_attention[0].reason, Some("load_error"));
        assert!(report.released.is_empty());
        assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::ResultCached { .. }));
        assert_eq!(cached_files(&fixture), [true, true]);
        // Readable again: the same result is applied.
        std::fs::write(&fixture.manifest, manifest).unwrap();
        crate::library::invalidate_manifest_cache(&fixture.manifest);
        let (report, _) = sweep(&fixture, &service);
        assert_eq!((report.attached.len(), report.needs_attention.len()), (1, 0));
    }

    /// The sweep before a cloud clean looks only at attempts that have not
    /// ended: it does not audit results already applied.
    #[test]
    fn the_sweep_before_a_clean_ends_what_is_open_and_audits_nothing() {
        let fixture = RenderFixture::new("sweep-before-clean");
        let service = cached_fixture(&fixture);
        change_underlay(&fixture);
        let (report, _) = crate::inference::commands::end_unresolved_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert_eq!(report.released.len(), 1);
        assert_ended(&fixture, &service, "region_changed");

        let fixture = RenderFixture::new("sweep-before-clean-committed");
        let service = cached_fixture(&fixture);
        service.recover_attempt_locally(&fixture.attempt_id(), Some(&fixture.manifest)).unwrap();
        // A lost commit is the start sweep's to find, not this one's.
        std::fs::write(&fixture.manifest, b"invalid manifest").unwrap();
        let (report, waits) = crate::inference::commands::end_unresolved_attempts(&service, &|_| Some(fixture.manifest.clone()));
        assert!(report.needs_attention.is_empty() && report.released.is_empty() && waits.is_empty());
    }

    /// A first attempt for a region carries the recipe's seed. A retry of the
    /// region carries a later one, so it is a new render; every request and
    /// its journal record agree. A gateway from before `jobs.retry_seed`
    /// refuses any seed but the recipe's, so it is sent the recipe's.
    #[test]
    fn a_retry_of_a_region_carries_a_new_seed_and_a_first_attempt_the_base_seed() {
        let fixture = RenderFixture::new("retry-seed");
        let gateway = FakeGateway::start(JobScript::Fails);
        let service = fixture.service(&gateway);
        let base = fixture.recipe.sampling().seed;
        let sent = |nonce: &str| {
            let outcome = service.execute_cloud_render(nonce, &fixture.manifest, 0, "reg-1", &fixture.target,
                &fixture.recipe, &fixture.intent, &fixture.config, true, &fast_polls());
            assert!(matches!(&outcome, Err(InferenceServiceError::RemoteJobFailed { error_code, .. })
                if error_code == FAKE_FAILURE.0), "{outcome:?}");
            let on_wire = gateway.seen.metadata.lock().unwrap().clone().unwrap()["seed"].as_u64().unwrap();
            let record = service.journal.get_record(&attempt_id_for_nonce(nonce)).unwrap();
            assert_eq!(record.seed, on_wire, "the journal records the seed that was sent");
            // Part 3 on the live path: the journal keeps the gateway's reason.
            assert!(matches!(&record.phase, AttemptPhase::Failed { error_code, message, .. }
                if error_code == FAKE_FAILURE.0 && message == FAKE_FAILURE.1), "{:?}", record.phase);
            on_wire
        };
        assert_eq!(sent(&fixture.nonce), base, "a first attempt is the render it always was");
        assert_eq!(sent(&fixture.grant()), base + 1);
        assert_eq!(sent(&fixture.grant()), base + 2);
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 3);

        // The Qwen worker itself renders three seeds in a row from the one it
        // is sent, so its retry steps past all three.
        let mut qwen = fixture.recipe.clone();
        qwen.recipe_id = "mc-qwen-image-edit-2511-v4".into();
        assert_eq!((fixture.recipe.retry_seed_stride(), qwen.retry_seed_stride()), (1, 3));

        let fixture = RenderFixture::new("retry-seed-old-gateway");
        let gateway = FakeGateway::start(JobScript::Fails);
        gateway.seen.old.store(true, Ordering::SeqCst);
        let service = fixture.service(&gateway);
        for nonce in [fixture.nonce.clone(), fixture.grant()] {
            let _ = service.execute_cloud_render(&nonce, &fixture.manifest, 0, "reg-1", &fixture.target,
                &fixture.recipe, &fixture.intent, &fixture.config, true, &fast_polls());
            assert_eq!(gateway.seen.metadata.lock().unwrap().clone().unwrap()["seed"].as_u64(), Some(base));
        }
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 2);
    }

    #[test]
    fn recovery_without_consent_or_existing_attempt_never_uploads() {
        let fixture = RenderFixture::new("recovery-no-consent");
        let gateway = FakeGateway::start(JobScript::Completes);
        let service = fixture.service(&gateway);
        assert!(service.recover_cloud_attempt("no-existing-attempt", &fixture.manifest,
            &fixture.config, &fast_polls()).is_err());
        assert!(service.execute_cloud_render("not-approved", &fixture.manifest, 0, "reg-1",
            &fixture.target, &fixture.recipe, &fixture.intent, &fixture.config, true, &fast_polls()).is_err());
        let _ = service.recover_cloud_attempt(&attempt_id_for_nonce("not-approved"), &fixture.manifest,
            &fixture.config, &fast_polls());
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 0);
        assert_eq!(FakeGateway::count(&gateway.seen.polls), 0);
    }

    /// A chapter with region `reg-1`, a grant to clean it in the cloud, and
    /// every `cloud://attempt` event the renders below report.
    struct RenderFixture {
        journal_dir: PathBuf,
        manifest: PathBuf,
        config: InferenceConfig,
        target: ExecutionTarget,
        recipe: RenderRecipe,
        intent: OperationIntent,
        nonce: String,
        consent: Arc<ConsentService>,
        grants: Arc<GrantService>,
        events: Arc<Mutex<Vec<CloudAttemptProgress>>>,
    }

    impl RenderFixture {
        fn new(name: &str) -> RenderFixture {
            let scratch = test_scratch(name);
            let (manifest, _bounds) = setup_test_job(&scratch, "reg-1");
            let config = test_config();
            let grants = Arc::new(GrantService::new());
            let consent = Arc::new(ConsentService::new_isolated(Arc::clone(&grants)));
            let target = ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            };
            let recipe = RenderRecipe::new(
                "sdnq-v1",
                "1.0.0",
                "flux-schnell",
                "0123456789abcdef0123456789abcdef01234567",
                false,
            );
            let intent = OperationIntent::CleanAnyway;
            let mut fixture = RenderFixture {
                journal_dir: scratch.join("journal"),
                manifest,
                config,
                target,
                recipe,
                intent,
                nonce: String::new(),
                consent,
                grants,
                events: Arc::default(),
            };
            fixture.nonce = fixture.grant();
            fixture
        }

        /// A new grant to clean `reg-1`: what the user confirms before each
        /// request, a retry included.
        fn grant(&self) -> String {
            let proposal = self.consent
                .prepare_proposal(
                    PrepareProposalRequest {
                        chapter_id: "chapter".into(),
                        page_index: 0,
                        region_id: Some("reg-1".into()),
                        target: self.target.clone(),
                        recipe: self.recipe.clone(),
                        intent: self.intent.clone(),
                    },
                    &self.config,
                    true,
                    &self.manifest,
                )
                .unwrap();
            self.consent
                .confirm_proposal(ConfirmProposalOptions {
                    proposal_id: &proposal.proposal_id,
                    intent: &self.intent,
                    config: &self.config,
                    cloud_allowed: true,
                    job_path: &self.manifest,
                    grant_ttl: Duration::from_secs(120),
                    max_attempts: 1,
                })
                .unwrap()
                .nonce
        }

        /// A service on this fixture's journal, as a fresh process would build it.
        fn service(&self, gateway: &FakeGateway) -> InferenceService {
            let events = Arc::clone(&self.events);
            InferenceService::new_isolated(
                &self.journal_dir,
                cleaner_core::cloud_wire::provisional_fixture_limits(),
                Arc::clone(&self.consent),
                Arc::clone(&self.grants),
                Arc::new(SecretManager::new_in_memory()),
            )
            .with_custom_client(gateway.client())
            .with_progress(Arc::new(move |progress: &CloudAttemptProgress| {
                events.lock().unwrap().push(progress.clone())
            }))
        }

        fn render(
            &self,
            service: &InferenceService,
            poll: &PollOptions,
        ) -> Result<ApiRegion, InferenceServiceError> {
            service.execute_cloud_render(
                &self.nonce,
                &self.manifest,
                0,
                "reg-1",
                &self.target,
                &self.recipe,
                &self.intent,
                &self.config,
                true,
                poll,
            )
        }

        fn attempt_id(&self) -> String {
            attempt_id_for_nonce(&self.nonce)
        }

        /// The phases reported so far, then forget them.
        fn take_phases(&self) -> Vec<RenderPhase> {
            std::mem::take(&mut *self.events.lock().unwrap())
                .into_iter()
                .map(|event| event.phase)
                .collect()
        }

        fn last_event(&self) -> CloudAttemptProgress {
            self.events
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("an event")
        }

        fn phase_on_disk(&self, service: &InferenceService) -> AttemptPhase {
            service
                .journal()
                .get_record(&self.attempt_id())
                .unwrap()
                .phase
        }

        fn committed_patch(&self) -> Patch {
            let job = Job::open(&self.manifest).unwrap();
            let record = job
                .project
                .patches
                .iter()
                .find(|r| r.id == "reg-1")
                .unwrap();
            job.load_patch(record).unwrap()
        }
    }

    fn fast_polls() -> PollOptions {
        PollOptions {
            poll_interval: Duration::from_millis(20),
            timeout: Duration::from_secs(20),
            max_polls: 1000,
        }
    }

    /// Wait (bounded) until `counter` reaches `at_least`.
    fn wait_for(counter: &AtomicUsize, at_least: usize) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while counter.load(Ordering::SeqCst) < at_least {
            assert!(
                Instant::now() < deadline,
                "the gateway never saw the request"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_cloud_render_runs_end_to_end_and_reports_every_phase() {
        let fixture = RenderFixture::new("e2e-happy");
        let gateway = FakeGateway::start(JobScript::Completes);
        let service = fixture.service(&gateway);

        let region = fixture.render(&service, &fast_polls()).unwrap();

        assert_eq!(region.outcome, "cleaned");
        assert_eq!(
            fixture.take_phases(),
            [
                RenderPhase::Preparing,
                RenderPhase::Submitting,
                RenderPhase::Queued,
                RenderPhase::Running,
                RenderPhase::Downloading,
                RenderPhase::Compositing,
                RenderPhase::Committed,
            ]
        );
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);
        assert_eq!(FakeGateway::count(&gateway.seen.results), 1);

        // Every event names the attempt, region, chapter and page (IC-3).
        let attempt_id = fixture.attempt_id();
        assert!(attempt_id.starts_with("att-") && attempt_id.len() == 28);

        // The wire carried the local recipe's sampling (IC-6).
        let submitted = gateway.seen.metadata.lock().unwrap().clone().unwrap();
        let sampling = wire_sampling();
        assert_eq!(submitted["seed"], sampling.seed);
        assert_eq!(submitted["steps"], sampling.steps);
        assert_eq!(submitted["guidance_scaled"], sampling.guidance_scaled);
        assert_eq!(submitted["attempt_id"], attempt_id.as_str());

        // Provenance of the committed patch.
        let patch = fixture.committed_patch();
        assert_eq!(patch.provenance.engine, Engine::Flux);
        let cloud = patch.provenance.cloud.expect("cloud record");
        assert_eq!(cloud.provider, "modal");
        assert_eq!(cloud.profile_id.as_deref(), Some("modal-prof-1"));
        assert_eq!(cloud.job_id.as_deref(), submitted["job_id"].as_str());
        assert_eq!(cloud.attempt_id.as_deref(), Some(attempt_id.as_str()));
        assert_eq!(cloud.recipe_id.as_deref(), Some("sdnq-v1"));
        assert_eq!(
            cloud.model_revision.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(cloud.cost, None);
        assert!(matches!(
            fixture.phase_on_disk(&service),
            AttemptPhase::Committed { .. }
        ));
    }

    #[test]
    fn batch_submission_callback_follows_durable_acceptance_before_polling() {
        for (name, script, accepted) in [
            ("queued-callback", JobScript::Completes, true),
            ("rejected-callback", JobScript::NotEnqueued(503), false),
            ("unknown-callback", JobScript::DropsSubmission, false),
        ] {
            let fixture = RenderFixture::new(name);
            let gateway = FakeGateway::start(script);
            let service = fixture.service(&gateway);
            let callbacks = AtomicUsize::new(0);
            let outcome = service.execute_cloud_render_queued(&fixture.nonce, &fixture.manifest, 0, "reg-1",
                &fixture.target, &fixture.recipe, &fixture.intent, &fixture.config, true, &fast_polls(), &|| {
                    callbacks.fetch_add(1, Ordering::SeqCst);
                    assert!(matches!(fixture.phase_on_disk(&service), AttemptPhase::Accepted { .. }));
                    assert_eq!(FakeGateway::count(&gateway.seen.polls), 0);
                    assert_eq!(FakeGateway::count(&gateway.seen.results), 0);
                    assert_eq!(fixture.last_event().phase, RenderPhase::Queued);
                });
            assert_eq!(callbacks.load(Ordering::SeqCst), usize::from(accepted), "{name}");
            assert_eq!(outcome.is_ok(), accepted, "{name}");
        }
    }

    #[test]
    fn reused_batch_client_still_enforces_permission_target_and_profile_epoch() {
        let gateway = FakeGateway::start(JobScript::Completes);
        let client = gateway.client();
        // Two regions use clones of the same authenticated batch client.
        for name in ["batch-client-first", "batch-client-second"] {
            let fixture = RenderFixture::new(name);
            let service = fixture.service(&gateway).with_custom_client(client.clone());
            assert!(fixture.render(&service, &fast_polls()).is_ok());
        }
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 2);

        for refusal in ["permission", "profile", "epoch", "client-target"] {
            let fixture = RenderFixture::new(refusal);
            let mut service = fixture.service(&gateway).with_custom_client(client.clone());
            if refusal == "profile" {
                service = service.with_dispatch_guard(Box::new(|_, _| false));
            }
            if refusal == "epoch" {
                fixture.grants.invalidate_profile(CloudProvider::Modal, "modal-prof-1");
            }
            if refusal == "client-target" {
                assert!(matches!(service.build_http_client(CloudProvider::Beam, "other-profile", &fixture.config),
                    Err(InferenceServiceError::ProfileChanged)));
                continue;
            }
            let outcome = service.execute_cloud_render(&fixture.nonce, &fixture.manifest, 0, "reg-1",
                &fixture.target, &fixture.recipe, &fixture.intent, &fixture.config,
                refusal != "permission", &fast_polls());
            assert!(outcome.is_err(), "{refusal}");
            assert_eq!(FakeGateway::count(&gateway.seen.submits), 2, "{refusal} still submitted");
            assert_eq!(Job::open(&fixture.manifest).unwrap().project.patches.len(), 1);
        }
    }

    /// A text group as grouping records it on a detection.
    fn test_group(bounds: Rect) -> cleaner_core::text_groups::GroupRecord {
        cleaner_core::text_groups::GroupRecord {
            version: cleaner_core::text_groups::GROUPING_VERSION,
            id: "tg-0123456789ab".into(),
            origin: cleaner_core::text_groups::GroupOrigin::MaskLayout,
            bounds,
            lettering_pixels: bounds.w * bounds.h,
            lettering_sha256: "a".repeat(64),
            components: vec!["sam-00001".into(), "sam-00002".into()],
            boxes: Vec::new(),
            bubble: None,
            models: Vec::new(),
            reasons: Vec::new(),
            split: None,
            estimated: false,
            evidence_ref: None,
            evidence_sha256: None,
            reading: None,
        }
    }

    /// A stored detection renders through the same path as a patch, the way a
    /// batch cloud clean drives it: the region grant minted from
    /// [`read_render_input`]'s own scope is the one the dispatch spends, and the
    /// result replaces the detection with a patch in one write.
    #[test]
    fn a_stored_detection_renders_and_is_replaced_by_its_cloud_patch() {
        let scratch = test_scratch("e2e-detection");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let bbox = Rect::new(30, 30, 24, 16);
        {
            let mut job = Job::open(&manifest).unwrap();
            job.store_detection(
                cleaner_core::project::DetectedRegion {
                    id: "p-d0".into(),
                    source_idx: 0,
                    bbox,
                    mask_ref: String::new(),
                    inside: true,
                    balloon_color: None,
                    script: Some("ja".into()),
                    pick: "lama".into(),
                    detector: "local".into(),
                    created: 1_760_000_000,
                    mask_sha256: String::new(),
                    order: 0,
                    review_state: Some("review.reason.unusuallyLarge".into()),
                    fit: None,
                    group: Some(test_group(bbox)),
                    padding_from_seed: false,
                    padding_px: 0,
                },
                &Mask::filled(bbox),
                &Mask::filled(bbox),
            )
            .unwrap();
        }
        let detected = Job::open(&manifest).unwrap().project.detections.clone();
        let detected_mask = std::fs::read(cleaner_core::project::sidecar_dir(&manifest).join("detections/p-d0.mask")).unwrap();
        let config = test_config();
        let grants = Arc::new(GrantService::new());
        let target = ExecutionTarget::Modal { profile_id: "modal-prof-1".into() };
        let recipe = RenderRecipe::new(
            "sdnq-v1",
            "1.0.0",
            "flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        );
        let intent = OperationIntent::CloudClean;
        let endpoint = compute_canonical_endpoint_fingerprint(
            &config.modal_profiles.get("modal-prof-1").unwrap().endpoint_url,
        )
        .unwrap();
        let input = read_render_input(&manifest, 0, "p-d0", &recipe).unwrap();
        let scope = input.scope(
            CloudProvider::Modal,
            "modal-prof-1",
            &endpoint,
            compute_operation_digest(&intent).unwrap(),
            &recipe,
            "p-d0",
        );
        let grant = grants
            .issue_grant_with_epoch(scope, 0, Duration::from_secs(60), 1)
            .unwrap();
        let gateway = FakeGateway::start(JobScript::Completes);
        let service = InferenceService::new_isolated(
            scratch.join("journal"),
            cleaner_core::cloud_wire::provisional_fixture_limits(),
            Arc::new(ConsentService::new_isolated(Arc::clone(&grants))),
            Arc::clone(&grants),
            Arc::new(SecretManager::new_in_memory()),
        )
        .with_custom_client(gateway.client());

        let region = service
            .execute_cloud_render(
                &grant.nonce,
                &manifest,
                0,
                "p-d0",
                &target,
                &recipe,
                &intent,
                &config,
                true,
                &fast_polls(),
            )
            .unwrap();

        assert_eq!(region.id, "p-d0");
        assert_eq!(region.outcome, "cleaned");
        let job = Job::open(&manifest).unwrap();
        assert!(job.project.detections.is_empty(), "the detection left in the same write");
        assert_eq!(job.project.cleaned_detections, detected, "and is kept beside its patch");
        assert!(job.sidecar().join("detections/p-d0.mask").exists());
        let record = job.project.patches.iter().find(|r| r.id == "p-d0").unwrap();
        assert_eq!(record.review_state.as_deref(), Some("review.reason.unusuallyLarge"));
        let patch = job.load_patch(record).unwrap();
        assert_eq!(patch.order, 0);
        assert_eq!(patch.provenance.engine, Engine::Flux);
        assert_eq!(patch.provenance.execution_provider, "cloud");
        assert_eq!(patch.provenance.params_snapshot["source"], "auto");
        // Source mask, logical group, request and output stay one chain: the
        // patch names the detection's group and the attempt that rendered it.
        assert_eq!(patch.provenance.params_snapshot["group"], test_group(bbox).snapshot());
        assert_eq!(patch.provenance.params_snapshot["group"]["id"], "tg-0123456789ab");
        let cloud = patch.provenance.cloud.expect("cloud record");
        assert_eq!(cloud.attempt_id.as_deref(), Some(attempt_id_for_nonce(&grant.nonce).as_str()));
        assert_eq!(service.journal().get_record(cloud.attempt_id.as_deref().unwrap()).unwrap().region_id, "p-d0");
        assert_eq!(cloud.recipe_id.as_deref(), Some("sdnq-v1"));
        assert!(matches!(
            service.journal().get_record(&attempt_id_for_nonce(&grant.nonce)).unwrap().phase,
            AttemptPhase::Committed { .. }
        ));
        // Spent: the grant does not start a second render.
        assert!(grants.get_grant(&grant.nonce).is_none_or(|grant| grant.attempts_used == 1));

        // The commit wrote one undo step, with the detected region on one
        // side and its result on the other.
        let journal = crate::history::load(&manifest);
        assert_eq!((journal.entries.len(), journal.cursor), (1, 1));
        let entry = &journal.entries[0];
        assert_eq!((entry.op.as_str(), entry.label.as_str(), entry.region_id.as_str()),
            (crate::history::CLEAN_OP, crate::history::CLEAN_LABEL, "p-d0"));
        assert_eq!(entry.before.region.as_ref().unwrap()["outcome"], "detected");
        assert_eq!(entry.after.region.as_ref().unwrap()["outcome"], "cleaned");

        // Undo: the detection is back as stored, and the attempt is still a
        // committed, resolved one, so the region can be sent again.
        let asked = |seen: &GatewayCounters| [&seen.submits, &seen.polls, &seen.results, &seen.cancels]
            .map(|count| count.load(Ordering::SeqCst));
        let before_undo = asked(&gateway.seen);
        let attempt = attempt_id_for_nonce(&grant.nonce);
        let patches = job.project.patches.clone();
        drop(job);
        assert_eq!(crate::history::move_at(&manifest, "chapter", "undo").unwrap().entry.unwrap().seq, entry.seq);
        {
            let _lock = run::lock_job(&manifest).unwrap();
            Job::open(&manifest).unwrap().restore_detection("p-d0").unwrap();
        }
        let job = Job::open(&manifest).unwrap();
        assert_eq!(job.project.detections, detected);
        assert_eq!(std::fs::read(job.sidecar().join("detections/p-d0.mask")).unwrap(), detected_mask);
        assert!(job.project.patches.iter().all(|row| row.id != "p-d0"));
        assert!(matches!(
            service.recover_attempt_locally(&attempt, Some(&manifest)).unwrap(),
            RecoveryDecision::AlreadyCommitted { .. }
        ), "a result set aside is not a result lost");
        assert_eq!(service.journal().unresolved_region_attempt(Some("chapter"), Some(0), "p-d0").unwrap(), None);
        let again = read_render_input(&manifest, 0, "p-d0", &recipe).unwrap();
        assert_eq!((again.revision, &again.revision_hash), (input.revision, &input.revision_hash),
            "the next request binds the same detection");

        // Redo: the same patch, and the gateway is asked nothing.
        assert!(crate::history::move_at(&manifest, "chapter", "redo").unwrap().entry.is_some());
        {
            let _lock = run::lock_job(&manifest).unwrap();
            Job::open(&manifest).unwrap().restore_patch("p-d0").unwrap();
        }
        let job = Job::open(&manifest).unwrap();
        assert_eq!(job.project.patches, patches);
        assert!(job.load_patch(&job.project.patches[job.project.patches.len() - 1]).is_ok());
        assert!(job.project.detections.is_empty());
        assert_eq!(asked(&gateway.seen), before_undo, "redo renders nothing");
    }

    #[test]
    fn every_event_of_a_render_carries_its_identity() {
        let fixture = RenderFixture::new("e2e-identity");
        let gateway = FakeGateway::start(JobScript::Completes);
        let service = fixture.service(&gateway);

        fixture.render(&service, &fast_polls()).unwrap();

        let events = fixture.events.lock().unwrap().clone();
        assert!(!events.is_empty());
        for event in &events {
            assert_eq!(event.attempt_id, fixture.attempt_id());
            assert_eq!(event.region_id, "reg-1");
            assert_eq!(event.chapter_id, "chapter");
            assert_eq!(event.provider, fixture.target.provider());
            assert_eq!(event.profile_id.as_deref(), fixture.target.profile_id());
            assert_eq!(event.page_index, 0);
            assert_eq!(event.error_code, None);
        }
        assert!(events
            .windows(2)
            .all(|pair| pair[0].elapsed_ms <= pair[1].elapsed_ms));
    }

    #[test]
    fn a_render_cancelled_mid_poll_stops_and_ends_cancelled() {
        let fixture = RenderFixture::new("e2e-cancel-flag");
        let gateway = FakeGateway::start(JobScript::RunsUntilCancelled);
        let service = fixture.service(&gateway);
        let attempt_id = fixture.attempt_id();

        let started = Instant::now();
        let outcome = thread::scope(|scope| {
            scope.spawn(|| {
                wait_for(&gateway.seen.polls, 2);
                assert!(
                    request_render_cancel(&attempt_id),
                    "the render was not live"
                );
            });
            fixture.render(&service, &fast_polls())
        });

        assert!(
            matches!(outcome, Err(InferenceServiceError::Cancelled)),
            "{outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        let last = fixture.last_event();
        assert_eq!(last.phase, RenderPhase::Cancelled);
        assert_eq!(last.error_code, Some("cancelled"));
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);
        assert_eq!(FakeGateway::count(&gateway.seen.cancels), 1);
        assert!(matches!(
            fixture.phase_on_disk(&service),
            AttemptPhase::Cancelled { .. }
        ));
        assert!(!is_render_live(&attempt_id));
    }

    #[test]
    fn a_cancel_recorded_by_another_command_stops_the_render() {
        let fixture = RenderFixture::new("e2e-cancel-journal");
        let gateway = FakeGateway::start(JobScript::RunsUntilCancelled);
        let service = fixture.service(&gateway);
        let attempt_id = fixture.attempt_id();

        let outcome = thread::scope(|scope| {
            scope.spawn(|| {
                wait_for(&gateway.seen.polls, 2);
                // What `cancel_cloud_attempt` writes before its own request.
                let deadline = Instant::now() + Duration::from_secs(10);
                let guard = loop {
                    match service.journal().acquire_lock(&attempt_id) {
                        Ok(guard) => break guard,
                        Err(JournalError::AttemptLocked { .. }) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(err) => panic!("attempt lock never came free: {err:?}"),
                    }
                };
                service.journal().record_cancel_requested(&guard).unwrap();
            });
            fixture.render(&service, &fast_polls())
        });

        assert!(
            matches!(outcome, Err(InferenceServiceError::Cancelled)),
            "{outcome:?}"
        );
        assert_eq!(fixture.last_event().phase, RenderPhase::Cancelled);
        assert_eq!(FakeGateway::count(&gateway.seen.cancels), 1);
        assert!(matches!(
            fixture.phase_on_disk(&service),
            AttemptPhase::Cancelled { .. }
        ));
    }

    #[test]
    fn predecessor_change_mid_poll_stales_one_paid_job_without_attaching() {
        let fixture = RenderFixture::new("predecessor-mid-poll");
        let gateway = FakeGateway::start(JobScript::RunsUntilFinished);
        let service = fixture.service(&gateway);
        let outcome = thread::scope(|scope| {
            scope.spawn(|| {
                wait_for(&gateway.seen.polls, 2);
                let mut job = Job::open(&fixture.manifest).unwrap();
                let mut earlier = job.load_patch(&job.project.patches[0]).unwrap();
                earlier.id = "earlier-mid-poll".into();
                earlier.order = 0;
                earlier.mask = Mask::filled(Rect::new(0, 0, 16, 16));
                earlier.ink = earlier.mask.clone();
                earlier.pixels.width = 16;
                earlier.pixels.height = 16;
                earlier.pixels.data = vec![100; 16 * 16];
                job.complete_region(0, &earlier, None).unwrap();
                gateway.finish();
            });
            fixture.render(&service, &fast_polls())
        });
        assert!(matches!(outcome, Err(InferenceServiceError::StaleAttachment(_))), "{outcome:?}");
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);
        assert!(fixture.committed_patch().provenance.cloud.is_none());
        // The journal's own snapshot passes `prepare_attachment`; the project
        // re-read refuses the drift, so the result stays cached under a pending
        // attachment that recovery will refuse the same way.
        let phase = fixture.phase_on_disk(&service);
        assert!(matches!(phase, AttemptPhase::AttachmentPending { .. }), "{phase:?}");
    }

    #[test]
    fn a_submission_lost_in_transport_ends_unknown_and_is_never_resent() {
        let fixture = RenderFixture::new("e2e-submit-lost");
        let gateway = FakeGateway::start(JobScript::DropsSubmission);
        let service = fixture.service(&gateway);

        let outcome = fixture.render(&service, &fast_polls());

        assert!(
            matches!(outcome, Err(InferenceServiceError::AmbiguousSubmission(_))),
            "{outcome:?}"
        );
        let last = fixture.last_event();
        assert_eq!(last.phase, RenderPhase::Unknown);
        assert_eq!(last.error_code, Some("submission_unknown"));
        assert_eq!(
            fixture.take_phases(),
            [
                RenderPhase::Preparing,
                RenderPhase::Submitting,
                RenderPhase::Unknown
            ]
        );
        assert!(matches!(
            fixture.phase_on_disk(&service),
            AttemptPhase::Unknown { .. }
        ));

        // Neither recovery nor a second try with the same grant sends it again.
        let decision = service
            .recover_cloud_attempt(
                &fixture.attempt_id(),
                &fixture.manifest,
                &fixture.config,
                &fast_polls(),
            )
            .unwrap();
        assert!(
            matches!(decision, RecoveryDecision::AmbiguousUnknown { .. }),
            "{decision:?}"
        );
        assert!(fixture.render(&service, &fast_polls()).is_err());
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);
    }

    #[test]
    fn a_job_that_outlives_the_app_is_attached_after_a_restart() {
        let fixture = RenderFixture::new("e2e-restart");
        let gateway = FakeGateway::start(JobScript::RunsUntilFinished);

        // First run: the wait runs out while the job still runs remotely.
        {
            let service = fixture.service(&gateway);
            let short = PollOptions {
                poll_interval: Duration::from_millis(20),
                timeout: Duration::from_millis(300),
                max_polls: 1000,
            };
            let outcome = fixture.render(&service, &short);
            assert!(
                matches!(outcome, Err(InferenceServiceError::PollingTimeout(_))),
                "{outcome:?}"
            );
            assert_eq!(fixture.last_event().error_code, Some("poll_timeout"));
            assert!(matches!(
                fixture.phase_on_disk(&service),
                AttemptPhase::Accepted { .. }
            ));
        }
        fixture.take_phases();
        gateway.finish();

        // Restart: a new service on the same journal finds the job accepted.
        let service = fixture.service(&gateway);
        let manifest = fixture.manifest.clone();
        let chapter_path =
            move |chapter_id: &str| (chapter_id == "chapter").then(|| manifest.clone());
        let (report, waits) =
            crate::inference::commands::recover_all_attempts(&service, &chapter_path);
        assert!(report.attached.is_empty() && report.needs_attention.is_empty());
        assert_eq!(report.still_running.len(), 1);
        let entry = &report.still_running[0];
        assert_eq!(entry.attempt_id, fixture.attempt_id());
        assert_eq!(entry.chapter_id.as_deref(), Some("chapter"));
        assert_eq!(entry.page_index, Some(0));
        assert_eq!(entry.region_id, "reg-1");
        assert_eq!(waits, [(fixture.attempt_id(), fixture.manifest.clone())]);

        // The wait `reconcileCloudRecovery({apply: true})` starts for it.
        let decision = service
            .recover_cloud_attempt(
                &fixture.attempt_id(),
                &fixture.manifest,
                &fixture.config,
                &fast_polls(),
            )
            .unwrap();
        assert!(
            matches!(decision, RecoveryDecision::AlreadyCommitted { .. }),
            "{decision:?}"
        );
        assert_eq!(
            fixture.take_phases(),
            [
                RenderPhase::Queued,
                RenderPhase::Downloading,
                RenderPhase::Compositing,
                RenderPhase::Committed,
            ]
        );
        let patch = fixture.committed_patch();
        assert_eq!(patch.provenance.engine, Engine::Flux);
        assert_eq!(
            patch.provenance.cloud.and_then(|cloud| cloud.attempt_id),
            Some(fixture.attempt_id())
        );
        assert_eq!(FakeGateway::count(&gateway.seen.submits), 1);

        // Settled: the next recovery has nothing to report.
        let (report, waits) =
            crate::inference::commands::recover_all_attempts(&service, &chapter_path);
        assert!(report.attached.is_empty() && report.still_running.is_empty());
        assert!(report.needs_attention.is_empty() && waits.is_empty());
    }
}
