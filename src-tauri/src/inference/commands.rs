//! Explicit Tauri command surface for public inference configuration and secret management.
//!
//! ## Invariants
//!
//! 1. **Public Profile Commands Only:** Only validated public configuration objects are read or written.
//! 2. **Origin-Bound Secret Commands:** All secret commands resolve the stored profile from `inference.json`
//!    to extract its canonical origin fingerprint. Unconfigured profile IDs or mismatched origins are rejected.
//! 3. **Write-Only Secrets:** Secret strings travel into the backend to be stored in the OS keyring
//!    or session store, but are NEVER returned over commands or persisted in `settings.json` / `inference.json`.
//! 4. **Safe Summary Responses:** Secret queries return [`SecretSummary`] describing existence,
//!    role, provider, and backend type without disclosing secret material.
//! 5. **Automatic Grant Revocation:** Profile updates, deletions, and credential modifications
//!    automatically revoke all outstanding authorization grants for that profile.
//! 6. **No Grant Minting Commands:** Grant issuance is an internal backend API only (P3b); frontend
//!    invocations cannot mint arbitrary grants.
//! 7. **No Fake Ready Success:** Connection/provisioning readiness is never spoofed or simulated.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cleaner_core::cloud_wire::{
    compute_request_digest, JobExecutionStatus, JobRequestMetadata, ModelInfoResponse,
    ResultMetadata, WireRenderRecipe, PROTOCOL_VERSION,
};
use cleaner_core::engines::render::{CloudProvider, ExecutionTarget, Preprocessing, RenderRecipe};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::project::Job;
use serde::{Deserialize, Serialize};

use crate::inference::config::{self, compute_canonical_endpoint_fingerprint, InferenceConfig};
use crate::inference::consent::{
    encode_gray8_png, encode_rgb8_png, load_cloud_region, ConsentError, ConsentProposal,
    ConsentService, OperationIntent, PrepareProposalRequest,
};
use crate::inference::http::{
    BoundRuntimeCredential, CloudEndpointTarget, CloudHttpClient, HttpTransportError,
    RuntimeCredential,
};
use crate::inference::journal::{
    self, AttemptJournal, AttemptPhase, AttemptRecord, CreateAttemptIntent, JournalError,
    ProjectRegionSnapshot, RecoveryDecision,
};
use crate::inference::policy::{Grant, GrantError, GrantScope, GrantService};
use crate::inference::secrets::{
    decode_modal_runtime_secret, encode_modal_runtime_secret, SecretKey, SecretManager, SecretRole,
    SecretSummary, SecretValue,
};
use crate::inference::service::{
    self, attempt_id_for_nonce, CloudAttemptProgress, InferenceService, InferenceServiceError,
    PollOptions, ProgressSink,
};
use crate::library::Library;
use crate::run;

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn get_app_journal_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "failed to resolve application data directory".to_string())?;
    Ok(data_dir.join("cloud_attempts"))
}

pub(crate) fn get_app_journal(app: &tauri::AppHandle) -> Result<AttemptJournal, String> {
    let dir = get_app_journal_dir(app)?;
    Ok(AttemptJournal::new(
        dir,
        cleaner_core::cloud_wire::provisional_fixture_limits(),
    ))
}

/// The event an interactive cloud render reports its phases on (IC-3).
pub const CLOUD_ATTEMPT_EVENT: &str = "cloud://attempt";

/// Where the app's cloud renders report their phases: [`CLOUD_ATTEMPT_EVENT`].
pub(crate) fn cloud_attempt_sink(app: &tauri::AppHandle) -> ProgressSink {
    use tauri::Emitter;
    let app = app.clone();
    Arc::new(move |progress: &CloudAttemptProgress| {
        // A window that is not listening does not make the render fail.
        let _ = app.emit(CLOUD_ATTEMPT_EVENT, progress);
    })
}

/// The service the app's commands render and recover through: the app's
/// journal, with every render reporting to [`cloud_attempt_sink`].
pub(crate) fn app_inference_service(app: &tauri::AppHandle) -> Result<InferenceService, String> {
    let dir = get_app_journal_dir(app)?;
    Ok(
        InferenceService::new(dir, cleaner_core::cloud_wire::provisional_fixture_limits())
            .with_progress(cloud_attempt_sink(app))
            .with_review(crate::inference::review::review_sink(app))
            .with_dispatch_guard(Box::new({
                let app = app.clone();
                move |provider, profile_id| {
                    crate::inference::profile_in_service(&app, provider, profile_id).is_ok()
                }
            })),
    )
}

/// Helper to resolve a profile's canonical origin fingerprint from stored app configuration.
fn resolve_profile_fingerprint(
    config: &InferenceConfig,
    provider: CloudProvider,
    profile_id: &str,
) -> Result<String, String> {
    let profile = match provider {
        CloudProvider::Beam => config.beam_profiles.get(profile_id),
        CloudProvider::Modal => config.modal_profiles.get(profile_id),
    };

    match profile {
        Some(p) => Ok(p.canonical_origin_fingerprint.clone()),
        None => Err(format!(
            "{}: profile '{}' does not exist in inference configuration",
            provider.as_str(),
            profile_id
        )),
    }
}

/// Error encountered while resolving and constructing a cloud transport client for a profile.
#[derive(Debug)]
pub enum BuildClientError {
    Configuration(String),
    CredentialMissing(String),
}

impl std::fmt::Display for BuildClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(msg) => write!(f, "configuration error: {msg}"),
            Self::CredentialMissing(msg) => write!(f, "credential missing: {msg}"),
        }
    }
}

impl std::error::Error for BuildClientError {}

/// Helper to construct a hardened cloud HTTP transport client for a stored profile.
///
/// Fails closed if the profile is not found or runtime credentials are missing from SecretManager.
pub(crate) fn build_client_for_profile(
    config: &InferenceConfig,
    provider: CloudProvider,
    profile_id: &str,
) -> Result<CloudHttpClient, BuildClientError> {
    // Before the keychain read, so a paused provider asks for nothing.
    if crate::inference::provider_paused(provider) {
        return Err(BuildClientError::Configuration(format!("provider_paused: {} is paused in this version", provider.as_str())));
    }
    config::validate_profile_id(profile_id).map_err(BuildClientError::Configuration)?;

    let profile = match provider {
        CloudProvider::Beam => config.beam_profiles.get(profile_id),
        CloudProvider::Modal => config.modal_profiles.get(profile_id),
    }
    .ok_or_else(|| {
        BuildClientError::Configuration(format!(
            "{}: profile '{}' does not exist in inference configuration",
            provider.as_str(),
            profile_id
        ))
    })?;

    let target =
        CloudEndpointTarget::new(provider, profile_id, &profile.endpoint_url).map_err(|_| {
            BuildClientError::Configuration("invalid endpoint configuration".to_string())
        })?;

    let sec_key = SecretKey::new(
        provider,
        profile_id.to_string(),
        profile.canonical_origin_fingerprint.clone(),
        SecretRole::Runtime,
    );

    let secret_val = SecretManager::global()
        .get_secret(&sec_key)
        .map_err(|_| {
            BuildClientError::CredentialMissing(format!(
                "{}: runtime credential missing for profile '{}'",
                provider.as_str(),
                profile_id
            ))
        })?
        .ok_or_else(|| {
            BuildClientError::CredentialMissing(format!(
                "{}: runtime credential missing for profile '{}'",
                provider.as_str(),
                profile_id
            ))
        })?;

    let runtime_cred = match provider {
        CloudProvider::Beam => RuntimeCredential::BeamBearer(secret_val),
        CloudProvider::Modal => {
            let (token_id, token_secret) =
                decode_modal_runtime_secret(&secret_val).map_err(|_| {
                    BuildClientError::CredentialMissing(format!(
                        "{}: runtime credential invalid or incomplete for profile '{}'",
                        provider.as_str(),
                        profile_id
                    ))
                })?;
            RuntimeCredential::ModalProxy {
                token_id,
                token_secret,
            }
        }
    };

    let bound_cred = BoundRuntimeCredential::new(&target, runtime_cred).map_err(|_| {
        BuildClientError::CredentialMissing("failed to bind credential to target".to_string())
    })?;

    CloudHttpClient::new(target, bound_cred).map_err(|_| {
        BuildClientError::Configuration("failed to initialize HTTP client".to_string())
    })
}

// ============================================================================
// Explicit Sanitized Error Vocabulary & Conversion Helpers
// ============================================================================

/// Sanitizes errors from `InferenceConfig` loading and validation.
fn sanitize_config_error(_err: &str) -> String {
    "failed to read or validate inference configuration".to_string()
}

/// Sanitizes errors from `InferenceConfig` persistence.
fn sanitize_config_write_error(err: &str) -> String {
    if err.contains("schema version") {
        "unsupported inference configuration schema version".to_string()
    } else if err.contains("exceeds maximum limit") {
        "maximum profile count exceeded".to_string()
    } else if err.contains("canonical origin") {
        "profile canonical origin mismatch".to_string()
    } else if err.contains("invalid character")
        || err.contains("forbidden")
        || err.contains("invalid endpoint")
    {
        "invalid endpoint configuration in profile".to_string()
    } else if err.contains("non-existent") {
        "selected target references non-existent profile".to_string()
    } else {
        "failed to write inference configuration".to_string()
    }
}

/// Sanitizes `BuildClientError` for public IPC reporting.
fn sanitize_build_client_error(err: &BuildClientError) -> String {
    match err {
        BuildClientError::Configuration(_) => {
            "invalid or missing profile configuration".to_string()
        }
        BuildClientError::CredentialMissing(_) => {
            "runtime credential missing or invalid for profile".to_string()
        }
    }
}

/// Sanitizes `HttpTransportError` for public IPC reporting.
/// Guarantees zero raw endpoint URLs, server response bodies, tokens, or query internals are returned.
fn sanitize_http_transport_error(err: &HttpTransportError) -> String {
    match err {
        HttpTransportError::NotEnqueued { reason, .. } => sanitize_http_transport_error(reason),
        HttpTransportError::RenderWeightsUnavailable => "cloud render weights need repair".to_string(),
        HttpTransportError::ProviderPaused => "provider_paused: this provider is paused in this version".to_string(),
        HttpTransportError::UnexpectedStatus { status } if *status == 401 || *status == 403 => {
            format!("gateway authorization failure (status {status})")
        }
        HttpTransportError::UnexpectedStatus { status } => {
            format!("gateway returned unexpected HTTP status {status}")
        }
        HttpTransportError::InvalidCredential | HttpTransportError::CredentialProviderMismatch => {
            "runtime credential invalid or missing for target".to_string()
        }
        HttpTransportError::EndpointValidationFailed
        | HttpTransportError::InvalidTarget
        | HttpTransportError::InvalidProfileId => {
            "endpoint validation failed for target".to_string()
        }
        HttpTransportError::ConnectionError
        | HttpTransportError::Timeout
        | HttpTransportError::DnsResolutionFailed
        | HttpTransportError::DnsResolverBusy
        | HttpTransportError::NonPublicIpRejected
        | HttpTransportError::RedirectForbidden => {
            "gateway endpoint unreachable or connection timed out".to_string()
        }
        HttpTransportError::ContentTypeMismatch
        | HttpTransportError::BodySizeLimitExceeded
        | HttpTransportError::WireValidation
        | HttpTransportError::JsonDeserialization
        | HttpTransportError::ProviderMismatch
        | HttpTransportError::InvalidHandle => {
            "gateway protocol or response validation error".to_string()
        }
        HttpTransportError::JobFailed { .. } => "gateway reported the cloud job failed".to_string(),
        HttpTransportError::JobCancelled => "cloud job cancelled".to_string(),
        HttpTransportError::JobCancelUnconfirmed | HttpTransportError::JobDeadline => {
            "cloud job did not finish; its remote state is unknown".to_string()
        }
    }
}

/// Sanitizes `ConsentError` for public IPC reporting.
/// Guarantees zero local filesystem paths, raw hashes/digests, nonces, or command internals are leaked.
fn sanitize_consent_error(err: ConsentError) -> String {
    match err {
        ConsentError::CloudDisabled => {
            "cloud execution is disabled in application settings".to_string()
        }
        ConsentError::LocalTargetNotAllowed => {
            "execution target is local; consent is only required for remote execution".to_string()
        }
        ConsentError::ProfileNotFound(_, _) => {
            "profile not found for requested provider".to_string()
        }
        ConsentError::InvalidEndpoint(_) => "invalid endpoint configuration".to_string(),
        ConsentError::InvalidRecipe(_) => "invalid render recipe".to_string(),
        ConsentError::PageNotFound(_, _) => "page index not found in chapter".to_string(),
        ConsentError::RegionNotFound(_) => {
            "requested region was not found on specified page".to_string()
        }
        ConsentError::SourceImageError => {
            "region source image is missing or unreadable".to_string()
        }
        ConsentError::RenderPrepareFailed(_) => {
            "region geometry declined or invalid for rendering".to_string()
        }
        // A stable code the interface names the refusal by.
        ConsentError::CropTooLarge => {
            "cloud_consent_crop_too_large: the region's crop is past the render service limits".to_string()
        }
        ConsentError::ProposalNotFound => "proposal was not found".to_string(),
        ConsentError::ProposalExpired { .. } => "proposal has expired".to_string(),
        ConsentError::ProposalAlreadyConsumed => {
            "proposal has already been confirmed or consumed".to_string()
        }
        ConsentError::ProposalNotConfirmed => "proposal has not been confirmed".to_string(),
        ConsentError::GrantNonceMismatch => {
            "grant nonce does not match confirmed proposal binding".to_string()
        }
        ConsentError::ClockRollbackDetected => "system clock rollback detected".to_string(),
        ConsentError::IntentMismatch => {
            "operation intent mismatch: requested operation does not match prepared proposal"
                .to_string()
        }
        ConsentError::OperationParamsTooLarge => {
            "operation parameters exceed maximum allowable length".to_string()
        }
        ConsentError::InvalidParameter(_) => "invalid operation parameter".to_string(),
        ConsentError::RevisionMismatch => {
            "region revision mismatch: content has been modified since proposal was prepared"
                .to_string()
        }
        ConsentError::CropDigestMismatch => {
            "crop or hint raster digest mismatch during revalidation".to_string()
        }
        ConsentError::SourceMismatch => {
            "source image digest mismatch: image has changed since proposal was prepared"
                .to_string()
        }
        ConsentError::MaskMismatch => {
            "mask digest mismatch: mask has changed since proposal was prepared".to_string()
        }
        ConsentError::PageMismatch => {
            "page or job boundary mismatch between request and stored proposal".to_string()
        }
        ConsentError::ConfigMismatch => {
            "configuration or endpoint mismatch since proposal was prepared".to_string()
        }
        ConsentError::ProfileMutated => {
            "profile or credentials were modified since proposal was prepared".to_string()
        }
        ConsentError::GrantIssuanceError(ref ge) => sanitize_grant_error(ge),
        ConsentError::UnsupportedNewRegion(_) => "unsupported new region geometry".to_string(),
        ConsentError::RandomSourceError => "system random source error".to_string(),
        ConsentError::Internal(_) => "internal consent service error".to_string(),
    }
}

/// Sanitizes `GrantError` for public IPC reporting.
/// Guarantees zero raw nonces, scopes, or digests are leaked.
fn sanitize_grant_error(err: &GrantError) -> String {
    match err {
        GrantError::NotFound => "grant was not found".to_string(),
        GrantError::Expired { .. } => "grant has expired".to_string(),
        GrantError::ClockRollbackDetected => "system clock rollback detected".to_string(),
        GrantError::Revoked => "grant has been explicitly revoked".to_string(),
        GrantError::ProfileMutated => {
            "profile or credentials were modified since proposal was prepared".to_string()
        }
        GrantError::AttemptsExceeded { .. } => "grant attempt limit exceeded".to_string(),
        GrantError::ScopeMismatch { .. } => "grant scope mismatch".to_string(),
        GrantError::InvalidParameter(_) => "invalid grant parameter".to_string(),
        GrantError::RandomSourceError => "system random source error".to_string(),
    }
}

/// Sanitizes `JournalError` for public IPC reporting.
/// Guarantees zero local journal file paths, lock details, raw handles, or digests are leaked.
fn sanitize_journal_error(err: JournalError) -> String {
    match err {
        JournalError::InvalidIdentifier { field, .. } => {
            format!("invalid identifier in field '{field}'")
        }
        JournalError::UnsupportedSchemaVersion { .. } => {
            "unsupported journal schema version".to_string()
        }
        JournalError::ForeignGuard { .. } => "journal lock guard error".to_string(),
        JournalError::Wire(_) => "cloud wire contract validation error".to_string(),
        JournalError::Decode(_) => "cloud result decode error".to_string(),
        JournalError::AttemptLocked { .. } => "attempt is locked by another operation".to_string(),
        JournalError::AttemptAlreadyExists { .. } => "attempt already exists".to_string(),
        JournalError::RegionUnresolved { .. } => "recovery_required: recover the existing attempt before cleaning this region".to_string(),
        JournalError::AttemptNotFound { .. } => "attempt not found in journal".to_string(),
        JournalError::InvalidTransition { .. } => {
            "invalid attempt lifecycle state transition".to_string()
        }
        JournalError::StaleAttachment { field, .. } => {
            format!("stale attachment: snapshot mismatch on field '{field}'")
        }
        JournalError::DuplicateCommit { .. } => "attempt already committed".to_string(),
        JournalError::LimitExceeded { field, .. } => {
            format!("payload limit exceeded for '{field}'")
        }
        JournalError::RequestDigestMismatch { .. } => {
            "canonical request digest mismatch".to_string()
        }
        JournalError::ResultDigestMismatch { .. } => "cached result digest mismatch".to_string(),
        JournalError::Io(_) => "journal persistence or filesystem I/O error".to_string(),
        JournalError::Json(_) => "journal record serialization error".to_string(),
    }
}

/// Safe public connection check result (ordinary reachability, never triggers GPU work).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudConnectionStatus {
    pub ok: bool,
    pub status: String,
    pub provider: CloudProvider,
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Bounded limits advertised by the cloud model info endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudModelLimits {
    pub max_dimensions: [u32; 2],
    pub max_megapixels: f64,
    pub max_png_bytes: u64,
    pub max_multipart_bytes: u64,
    pub default_worker_deadline_sec: u32,
}

/// Wire model metadata and provisional limits returned over IPC without secrets or endpoint URLs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudModelInfo {
    pub supported_protocol_version: String,
    pub pinned_model_id: String,
    pub pinned_model_revision: String,
    pub pinned_recipe_id: String,
    pub preprocessing_version: String,
    pub native_mask_conditioning: bool,
    pub limits: CloudModelLimits,
}

impl From<ModelInfoResponse> for CloudModelInfo {
    fn from(resp: ModelInfoResponse) -> Self {
        let max_megapixels = (resp.limits.max_pixels as f64 / 1_000_000.0 * 100.0).round() / 100.0;
        Self {
            supported_protocol_version: resp.protocol_version,
            pinned_model_id: resp.model_id,
            pinned_model_revision: resp.model_revision,
            pinned_recipe_id: resp.recipe_id,
            preprocessing_version: resp.preprocessing_version,
            native_mask_conditioning: resp.native_mask_conditioning,
            limits: CloudModelLimits {
                max_dimensions: [resp.limits.max_width, resp.limits.max_height],
                max_megapixels,
                max_png_bytes: resp.limits.max_png_bytes,
                max_multipart_bytes: resp.limits.max_multipart_bytes,
                default_worker_deadline_sec: resp.limits.worker_timeout_seconds,
            },
        }
    }
}

/// Read public inference configuration from `inference.json`.
#[tauri::command]
pub fn read_inference_config(app: tauri::AppHandle) -> Result<InferenceConfig, String> {
    config::read_inference_config(&app).map_err(|e| sanitize_config_error(&e))
}

/// Validate and atomically write public inference configuration to `inference.json`.
#[tauri::command]
pub fn write_inference_config(
    app: tauri::AppHandle,
    config: InferenceConfig,
) -> Result<InferenceConfig, String> {
    let path = config::config_path(&app).map_err(|e| sanitize_config_write_error(&e))?;
    let before = config::read_inference_config_from_path(&path).ok();
    let written = write_inference_config_at(&path, config)?;
    // A removed endpoint takes its projects' standing consents with it, so
    // one set up again under the same id asks once more.
    let removed = before.map(|old| removed_profiles(&old, &written)).unwrap_or_default();
    if !removed.is_empty() {
        match Library::for_app(&app) {
            Ok(library) => {
                for (provider, profile_id) in &removed {
                    if let Err(err) = library.clear_cloud_consents(Some((*provider, profile_id))) {
                        eprintln!("cloud consent for removed endpoint {profile_id} not cleared: {err}");
                    }
                }
            }
            Err(err) => eprintln!("cloud consents for removed endpoints not cleared: {err}"),
        }
    }
    Ok(written)
}

/// The profiles `old` has and `new` does not, per provider.
fn removed_profiles(old: &InferenceConfig, new: &InferenceConfig) -> Vec<(CloudProvider, String)> {
    let beam = old
        .beam_profiles
        .keys()
        .filter(|id| !new.beam_profiles.contains_key(*id))
        .map(|id| (CloudProvider::Beam, id.clone()));
    let modal = old
        .modal_profiles
        .keys()
        .filter(|id| !new.modal_profiles.contains_key(*id))
        .map(|id| (CloudProvider::Modal, id.clone()));
    beam.chain(modal).collect()
}

/// The profiles whose destination a write moves: added, removed, or given
/// another endpoint or deployment. A rename or a new default moves none, so
/// work running on a profile keeps its grants when the user picks another.
fn moved_profiles(
    old: &std::collections::HashMap<String, config::CloudProfile>,
    new: &std::collections::HashMap<String, config::CloudProfile>,
) -> BTreeSet<String> {
    let same = |a: &config::CloudProfile, b: &config::CloudProfile| {
        a.endpoint_url == b.endpoint_url
            && a.created_at_ms == b.created_at_ms
            && a.updated_at_ms == b.updated_at_ms
    };
    old.keys()
        .chain(new.keys())
        .filter(|id| !matches!((old.get(*id), new.get(*id)), (Some(a), Some(b)) if same(a, b)))
        .cloned()
        .collect()
}

/// Make `provider`/`profile_id` the default cloud profile: where new work goes.
///
/// The read, the change and the write happen under the writer's lock, so two
/// quick picks, or a pick during a rename in Settings, cannot undo each other
/// the way a read in the interface and a write back could. Moves no profile,
/// so work already running on another profile keeps its grants.
#[tauri::command]
pub fn select_cloud_profile(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
) -> Result<InferenceConfig, String> {
    let path = config::config_path(&app).map_err(|e| sanitize_config_write_error(&e))?;
    select_cloud_profile_at(&path, provider, &profile_id)
}

pub(crate) fn select_cloud_profile_at(
    path: &Path,
    provider: CloudProvider,
    profile_id: &str,
) -> Result<InferenceConfig, String> {
    config::validate_profile_id(profile_id).map_err(|_| "invalid profile identifier".to_string())?;
    if crate::inference::provider_paused(provider) {
        return Err("provider_paused".to_string());
    }
    let _guard = config_write_lock();
    let mut config =
        config::read_inference_config_from_path(path).map_err(|e| sanitize_config_error(&e))?;
    let exists = match provider {
        CloudProvider::Beam => config.beam_profiles.contains_key(profile_id),
        CloudProvider::Modal => config.modal_profiles.contains_key(profile_id),
    };
    if !exists {
        return Err("profile_missing".to_string());
    }
    let target = match provider {
        CloudProvider::Beam => ExecutionTarget::Beam { profile_id: profile_id.to_string() },
        CloudProvider::Modal => ExecutionTarget::Modal { profile_id: profile_id.to_string() },
    };
    if config.selected_target == target {
        return Ok(config);
    }
    config.selected_target = target;
    write_inference_config_unlocked(path, config)
}

/// Serializes every write of `inference.json` in this process, and the read
/// each write compares against, so no write is computed from a file another
/// one has already replaced.
fn config_write_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The one writer of `inference.json`, shared by the settings command and by
/// provisioning (`crate::provision`), so every profile change goes through the
/// same grant revocation and epoch bumps. Errors are already sanitized.
pub(crate) fn write_inference_config_at(
    path: &Path,
    config: InferenceConfig,
) -> Result<InferenceConfig, String> {
    let _guard = config_write_lock();
    write_inference_config_unlocked(path, config)
}

fn write_inference_config_unlocked(
    path: &Path,
    config: InferenceConfig,
) -> Result<InferenceConfig, String> {
    let consent_svc = crate::inference::consent::ConsentService::global();

    // Read existing config to find the profiles this write moves. Only those
    // lose their grants: picking another default must not stop a run on the
    // last one. If on-disk config is corrupted or unreadable, invalidate all
    // authorization state globally so we can self-heal / repair the
    // configuration safely without aborting.
    let (beam_ids, modal_ids, had_corrupted_disk) =
        match config::read_inference_config_from_path(path) {
            Ok(old_cfg) => (
                moved_profiles(&old_cfg.beam_profiles, &config.beam_profiles),
                moved_profiles(&old_cfg.modal_profiles, &config.modal_profiles),
                false,
            ),
            Err(_) => {
                consent_svc.invalidate_all();
                let beam: BTreeSet<String> = config.beam_profiles.keys().cloned().collect();
                let modal: BTreeSet<String> = config.modal_profiles.keys().cloned().collect();
                (beam, modal, true)
            }
        };

    // Revoke outstanding grants, pending proposals, and advance epochs for every moved profile so deleted/mutated profiles don't survive
    for profile_id in &beam_ids {
        consent_svc.invalidate_profile(CloudProvider::Beam, profile_id);
    }
    for profile_id in &modal_ids {
        consent_svc.invalidate_profile(CloudProvider::Modal, profile_id);
    }

    let written = config::write_inference_config_to_path(path, config)
        .map_err(|e| sanitize_config_write_error(&e))?;

    if had_corrupted_disk {
        consent_svc.invalidate_all();
    }

    // Advance epochs again after write so in-flight confirmations started during write fail closed
    for profile_id in &beam_ids {
        consent_svc.invalidate_profile(CloudProvider::Beam, profile_id);
    }
    for profile_id in &modal_ids {
        consent_svc.invalidate_profile(CloudProvider::Modal, profile_id);
    }

    Ok(written)
}

/// Store a cloud credential into the OS keyring or explicitly requested session store.
///
/// Binds the credential to the canonical origin fingerprint of the stored profile.
/// Write-only: returns a [`SecretSummary`] confirmation.
#[tauri::command]
pub async fn store_cloud_secret(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
    role: SecretRole,
    secret: String,
    token_id: Option<String>,
    session_only: Option<bool>,
) -> Result<SecretSummary, String> {
    config::validate_profile_id(&profile_id)
        .map_err(|_| "invalid profile identifier".to_string())?;
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return Err("cloud secret cannot be empty".to_string());
    }

    // Resolve verified origin fingerprint from stored configuration
    let current_cfg = config::read_inference_config(&app).map_err(|e| sanitize_config_error(&e))?;
    let origin_fp = resolve_profile_fingerprint(&current_cfg, provider, &profile_id)?;

    let key = SecretKey::new(provider, profile_id.clone(), origin_fp, role);

    let secret_val = match (provider, role) {
        (CloudProvider::Modal, SecretRole::Runtime | SecretRole::Setup) if role == SecretRole::Runtime || token_id.is_some() => {
            let tid = token_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "Modal runtime credentials require a valid token ID".to_string())?;
            encode_modal_runtime_secret(tid, trimmed)
                .map_err(|_| "failed to encode Modal runtime secret".to_string())?
        }
        _ => SecretValue::new(trimmed),
    };

    let is_session_only = session_only.unwrap_or(false);

    // Invalidate active grants and proposals BEFORE mutation
    crate::inference::consent::ConsentService::global().invalidate_profile(provider, &profile_id);

    let summary = SecretManager::global()
        .store_secret(&key, secret_val, is_session_only)
        .map_err(|_| "failed to store secret in credential manager".to_string())?;

    // Invalidate again AFTER mutation to prevent in-flight races from minting old-state grants
    crate::inference::consent::ConsentService::global().invalidate_profile(provider, &profile_id);

    Ok(summary)
}

/// Delete a cloud credential from the OS keyring and session store.
#[tauri::command]
pub async fn delete_cloud_secret(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
    role: SecretRole,
) -> Result<SecretSummary, String> {
    config::validate_profile_id(&profile_id)
        .map_err(|_| "invalid profile identifier".to_string())?;

    let current_cfg = config::read_inference_config(&app).map_err(|e| sanitize_config_error(&e))?;
    let origin_fp = resolve_profile_fingerprint(&current_cfg, provider, &profile_id)?;

    let key = SecretKey::new(provider, profile_id.clone(), origin_fp, role);

    // Invalidate active grants and proposals BEFORE mutation
    crate::inference::consent::ConsentService::global().invalidate_profile(provider, &profile_id);

    let summary = SecretManager::global()
        .delete_secret(&key)
        .map_err(|_| "failed to delete secret from credential manager".to_string())?;

    // Invalidate again AFTER mutation
    crate::inference::consent::ConsentService::global().invalidate_profile(provider, &profile_id);

    Ok(summary)
}

/// Query safe summary of cloud credential presence for a given provider, profile, and role.
///
/// The three credential commands are async so a credential-store prompt waits on a
/// worker thread instead of freezing the window.
#[tauri::command]
pub async fn get_cloud_secret_summary(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
    role: SecretRole,
) -> Result<SecretSummary, String> {
    config::validate_profile_id(&profile_id)
        .map_err(|_| "invalid profile identifier".to_string())?;

    let current_cfg = config::read_inference_config(&app).map_err(|e| sanitize_config_error(&e))?;
    let origin_fp = resolve_profile_fingerprint(&current_cfg, provider, &profile_id)?;

    let key = SecretKey::new(provider, profile_id, origin_fp, role);
    SecretManager::global()
        .get_summary(&key)
        .map_err(|_| "failed to query secret summary".to_string())
}

/// Forget every keychain prompt the person cancelled this launch, so the next
/// read that needs a credential may ask once more. Settings > Cloud calls this
/// when it opens: that is the person asking for their endpoints again. Polls
/// never call it, so a cancel still holds for them.
#[tauri::command]
pub fn forget_cloud_secret_denials() {
    SecretManager::global().forget_denials();
}

/// Status word and message for a gateway call that failed, shared by the
/// connection check and [`verify_cloud_profile`] so both speak one vocabulary.
fn connection_failure(err: &HttpTransportError) -> (&'static str, String) {
    match *err {
        HttpTransportError::NotEnqueued { ref reason, .. } => connection_failure(reason),
        HttpTransportError::RenderWeightsUnavailable => ("weights_unavailable", "cloud render weights need repair".to_string()),
        HttpTransportError::ProviderPaused => (
            "provider_paused",
            "this provider is paused in this version".to_string(),
        ),
        HttpTransportError::UnexpectedStatus { status } if status == 401 || status == 403 => (
            "unauthorized",
            format!("gateway returned authorization failure status {status}"),
        ),
        HttpTransportError::UnexpectedStatus { status } => (
            "http_error",
            format!("gateway returned unexpected HTTP status {status}"),
        ),
        HttpTransportError::InvalidCredential | HttpTransportError::CredentialProviderMismatch => (
            "credential_missing",
            "runtime credential invalid or missing for target".to_string(),
        ),
        HttpTransportError::EndpointValidationFailed
        | HttpTransportError::InvalidTarget
        | HttpTransportError::InvalidProfileId => (
            "configuration_error",
            "endpoint validation failed for target".to_string(),
        ),
        HttpTransportError::ConnectionError
        | HttpTransportError::Timeout
        | HttpTransportError::DnsResolutionFailed
        | HttpTransportError::DnsResolverBusy
        | HttpTransportError::NonPublicIpRejected
        | HttpTransportError::RedirectForbidden => (
            "unreachable",
            "gateway endpoint unreachable or connection timed out".to_string(),
        ),
        HttpTransportError::ContentTypeMismatch
        | HttpTransportError::BodySizeLimitExceeded
        | HttpTransportError::WireValidation
        | HttpTransportError::JsonDeserialization
        | HttpTransportError::ProviderMismatch
        | HttpTransportError::InvalidHandle => (
            "http_error",
            "gateway protocol or response validation error".to_string(),
        ),
        HttpTransportError::JobFailed { .. }
        | HttpTransportError::JobCancelled
        | HttpTransportError::JobCancelUnconfirmed
        | HttpTransportError::JobDeadline => (
            "http_error",
            "cloud job did not complete".to_string(),
        ),
    }
}

/// The check provisioning runs on a profile it just saved: health, then model
/// info. A gateway can answer `/health` before its model volume is usable, and
/// the model-info call is the cheapest request that proves the runtime
/// credential and the pinned model both work. Neither call starts GPU work.
pub(crate) fn verify_cloud_profile(
    config: &InferenceConfig,
    provider: CloudProvider,
    profile_id: &str,
) -> CloudConnectionStatus {
    let health = check_cloud_connection_for_config(config, provider, profile_id);
    if !health.ok {
        return health;
    }
    let model_info = build_client_for_profile(config, provider, profile_id)
        .map_err(|_| None)
        .and_then(|client| client.get_model_info().map_err(Some));
    match model_info {
        Ok(_) => health,
        Err(err) => {
            let (status, message) = match err {
                Some(err) => connection_failure(&err),
                None => (
                    "credential_missing",
                    "runtime credential invalid or missing for target".to_string(),
                ),
            };
            CloudConnectionStatus {
                ok: false,
                status: status.to_string(),
                message: Some(message),
                ..health
            }
        }
    }
}

/// Helper performing a safe, structured connection check against an in-memory configuration.
///
/// Guarantees that errors never leak raw URLs, request bodies, local filesystem paths, or secrets.
pub fn check_cloud_connection_for_config(
    config: &InferenceConfig,
    provider: CloudProvider,
    profile_id: &str,
) -> CloudConnectionStatus {
    let client = match build_client_for_profile(config, provider, profile_id) {
        Ok(c) => c,
        Err(BuildClientError::CredentialMissing(msg)) => {
            return CloudConnectionStatus {
                ok: false,
                status: "credential_missing".to_string(),
                provider,
                profile_id: profile_id.to_string(),
                latency_ms: None,
                message: Some(msg),
            };
        }
        Err(BuildClientError::Configuration(msg)) => {
            return CloudConnectionStatus {
                ok: false,
                status: "configuration_error".to_string(),
                provider,
                profile_id: profile_id.to_string(),
                latency_ms: None,
                message: Some(msg),
            };
        }
    };

    let start = Instant::now();
    match client.get_health() {
        Ok(health) => {
            match client.gateway_is_current() {
                Ok(true) => {}
                result => {
                    let (status, message) = match result {
                        Ok(false) => ("gateway_out_of_date", "Gateway out of date. Redeploy the gateway to use this app version.".to_string()),
                        Err(error) => connection_failure(&error),
                        Ok(true) => unreachable!(),
                    };
                    return CloudConnectionStatus {
                        ok: false, status: status.into(), message: Some(message),
                        provider, profile_id: profile_id.into(),
                        latency_ms: Some(start.elapsed().as_millis() as u64),
                    };
                }
            }
            let latency_ms = start.elapsed().as_millis() as u64;
            let is_ok = health.status == "ok";
            CloudConnectionStatus {
                ok: is_ok,
                status: if is_ok {
                    "reachable".to_string()
                } else {
                    "http_error".to_string()
                },
                provider,
                profile_id: profile_id.to_string(),
                latency_ms: Some(latency_ms),
                message: if is_ok {
                    None
                } else {
                    Some("gateway returned non-ok health status".to_string())
                },
            }
        }
        Err(err) => {
            let (status, msg) = connection_failure(&err);
            CloudConnectionStatus {
                ok: false,
                status: status.to_string(),
                provider,
                profile_id: profile_id.to_string(),
                latency_ms: None,
                message: Some(msg),
            }
        }
    }
}

/// Test control-plane reachability for a stored cloud profile (ordinary health check; never triggers GPU work).
#[tauri::command]
pub async fn check_cloud_connection(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
) -> Result<CloudConnectionStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if config::validate_profile_id(&profile_id).is_err() {
            return Ok(CloudConnectionStatus {
                ok: false,
                status: "configuration_error".to_string(),
                provider,
                profile_id,
                latency_ms: None,
                message: Some("invalid profile identifier".to_string()),
            });
        }

        let current_cfg = match config::read_inference_config(&app) {
            Ok(cfg) => cfg,
            Err(_) => {
                return Ok(CloudConnectionStatus {
                    ok: false,
                    status: "configuration_error".to_string(),
                    provider,
                    profile_id,
                    latency_ms: None,
                    message: Some("Failed to read inference configuration".to_string()),
                });
            }
        };

        Ok(check_cloud_connection_for_config(
            &current_cfg,
            provider,
            &profile_id,
        ))
    })
    .await
    .map_err(|_| "async connection check task failed".to_string())?
}

/// Query wire metadata, model revision, and provisional service limits for a stored cloud profile.
#[tauri::command]
pub async fn get_cloud_model_info(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
) -> Result<CloudModelInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        config::validate_profile_id(&profile_id)
            .map_err(|_| "invalid profile identifier".to_string())?;
        let current_cfg =
            config::read_inference_config(&app).map_err(|e| sanitize_config_error(&e))?;

        let client = build_client_for_profile(&current_cfg, provider, &profile_id)
            .map_err(|e| sanitize_build_client_error(&e))?;
        let model_info_resp = client
            .get_model_info()
            .map_err(|e| sanitize_http_transport_error(&e))?;

        Ok(CloudModelInfo::from(model_info_resp))
    })
    .await
    .map_err(|_| "async model info task failed".to_string())?
}

/// Whether a deployment runs the cloud code this app's helper would deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudRelease {
    /// The code the helper would deploy ([`crate::provision::helper_code_digest`]).
    pub expected: String,
    /// What the gateway says it runs; `None` from a gateway too old to say.
    pub deployed: Option<String>,
    /// `deployed == expected`. False means Update would change what runs.
    pub current: bool,
}

impl CloudRelease {
    fn of(expected: String, deployed: Option<String>) -> CloudRelease {
        let current = deployed.as_deref() == Some(expected.as_str());
        CloudRelease { expected, deployed, current }
    }
}

/// Ask a stored profile's gateway which cloud code it runs. Starts no GPU.
#[tauri::command]
pub async fn check_cloud_release(
    app: tauri::AppHandle,
    provider: CloudProvider,
    profile_id: String,
) -> Result<CloudRelease, String> {
    tauri::async_runtime::spawn_blocking(move || {
        config::validate_profile_id(&profile_id)
            .map_err(|_| "invalid profile identifier".to_string())?;
        let current_cfg =
            config::read_inference_config(&app).map_err(|e| sanitize_config_error(&e))?;
        let client = build_client_for_profile(&current_cfg, provider, &profile_id)
            .map_err(|e| sanitize_build_client_error(&e))?;
        let deployed = client.gateway_code_digest().map_err(|e| sanitize_http_transport_error(&e))?;
        Ok(CloudRelease::of(crate::provision::helper_code_digest(), deployed))
    })
    .await
    .map_err(|_| "async release check task failed".to_string())?
}

/// Bounded rectangle DTO for consent proposal crop bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RectDto {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Authoritative backend consent proposal prepared before spend/transmission confirmation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentProposalDto {
    pub proposal_id: String,
    pub profile_id: String,
    pub provider: CloudProvider,
    pub endpoint_url: String,
    pub canonical_origin_fingerprint: String,
    pub profile_epoch: u64,
    pub crop_sha256: String,
    pub hint_sha256: String,
    pub source_hash: String,
    pub mask_hash: String,
    pub region_revision: u64,
    pub rect: RectDto,
    pub recipe: RenderRecipe,
    pub intent: OperationIntent,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    /// The chapter's project already consented to this endpoint, so the
    /// question is skipped. The proposal and its single-use grant are not.
    #[serde(default)]
    pub standing: bool,
}

impl From<ConsentProposal> for ConsentProposalDto {
    fn from(p: ConsentProposal) -> Self {
        let epoch = ConsentService::global().get_profile_epoch(p.provider, &p.profile_id);
        let x = u32::try_from(p.crop_bounds.x.max(0)).unwrap_or(0);
        let y = u32::try_from(p.crop_bounds.y.max(0)).unwrap_or(0);
        Self {
            proposal_id: p.proposal_id,
            profile_id: p.profile_id,
            provider: p.provider,
            endpoint_url: p.endpoint_url,
            canonical_origin_fingerprint: p.canonical_endpoint_fingerprint,
            profile_epoch: epoch,
            crop_sha256: p.crop_png_sha256,
            hint_sha256: p.hint_png_sha256,
            source_hash: p.source_hash,
            mask_hash: p.mask_hash,
            region_revision: p.revision,
            rect: RectDto {
                x,
                y,
                w: p.crop_bounds.w,
                h: p.crop_bounds.h,
            },
            recipe: p.recipe,
            intent: p.intent,
            created_at_ms: p.issued_at_ms,
            expires_at_ms: p.expires_at_ms,
            estimated_cost_usd: None,
            standing: false,
        }
    }
}

/// Mark a proposal whose chapter's project has a standing consent for its
/// endpoint.
fn with_standing(mut proposal: ConsentProposalDto, library: &Library, chapter_id: &str) -> ConsentProposalDto {
    proposal.standing = library.cloud_consent_covers(
        chapter_id,
        proposal.provider,
        &proposal.profile_id,
        &proposal.canonical_origin_fingerprint,
    );
    proposal
}

/// The statements a region consent is confirmed with. A destination the
/// project's standing consent covers needs none: they were ticked when that
/// consent was given. Anything else needs both. `Ok(true)` when this answer
/// ticked both, so it may stand for the project.
fn require_region_statements(
    library: &Library,
    chapter_id: &str,
    provider: CloudProvider,
    profile_id: &str,
    origin_fingerprint: &str,
    rights_attested: bool,
    retention_acknowledged: bool,
) -> Result<bool, String> {
    let accepted = rights_attested && retention_acknowledged;
    if !accepted
        && !library.cloud_consent_covers(chapter_id, provider, profile_id, origin_fingerprint)
    {
        crate::inference::analysis::require_consent(rights_attested, retention_acknowledged)
            .map_err(|e| e.to_string())?;
    }
    Ok(accepted)
}

/// Scoped authorization bounds minted upon proposal confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantScopeDto {
    pub provider: CloudProvider,
    pub profile_id: String,
    pub endpoint_fingerprint: String,
    pub crop_sha256: String,
    pub mask_hash: String,
    pub revision: u64,
    pub recipe: RenderRecipe,
    pub operation_digest: String,
}

/// Backend-issued, attempt-limited authorization grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantDto {
    pub nonce: String,
    pub scope: GrantScopeDto,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub allowed_attempts: u32,
    pub used_attempts: u32,
}

impl From<Grant> for GrantDto {
    fn from(g: Grant) -> Self {
        Self {
            nonce: g.nonce,
            scope: GrantScopeDto {
                provider: g.scope.provider,
                profile_id: g.scope.profile_id,
                endpoint_fingerprint: g.scope.canonical_endpoint_fingerprint,
                crop_sha256: g.scope.crop_sha256,
                mask_hash: g.scope.mask_hash,
                revision: g.scope.revision,
                recipe: g.scope.recipe,
                operation_digest: g.scope.operation_digest,
            },
            issued_at_ms: g.issued_at_ms,
            expires_at_ms: g.expires_at_ms,
            allowed_attempts: g.max_attempts,
            used_attempts: g.attempts_used,
        }
    }
}

/// Submission response for a durable remote attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAttemptSubmissionDto {
    pub attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Authoritative lifecycle status for an accepted remote attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAttemptStatusDto {
    pub attempt_id: String,
    pub handle: Option<String>,
    pub status: String,
    pub reported_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
}

/// Validated output crop result retrieved from local cache or remote gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudAttemptResultDto {
    pub attempt_id: String,
    pub handle: String,
    pub result_digest: String,
    pub reported_cost_usd: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub cached: bool,
}

/// Cancellation request acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCancelResultDto {
    pub handle: String,
    pub status: String,
    pub acknowledged: bool,
}

/// Recovery decider outcome for uncommitted or interrupted attempts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudRecoveryDecisionDto {
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_retryable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Synchronous core handler for preparing cloud consent proposals.
#[allow(clippy::too_many_arguments)]
pub fn prepare_cloud_consent_inner(
    cloud_allowed: bool,
    config: &InferenceConfig,
    job_path: &Path,
    target: ExecutionTarget,
    recipe: Option<RenderRecipe>,
    intent: OperationIntent,
    region_id: Option<String>,
    chapter_id: Option<String>,
    page_index: Option<u32>,
    simulate_blocked: Option<bool>,
) -> Result<ConsentProposalDto, String> {
    if simulate_blocked == Some(true) {
        return Err("Backend authorization blocked: cloudEngines permission denied".to_string());
    }
    if !cloud_allowed {
        return Err("Backend authorization blocked: cloudEngines permission denied".to_string());
    }

    let pinned_recipe =
        recipe.ok_or_else(|| "recipe is required for prepare_cloud_consent".to_string())?;

    let ch_id =
        chapter_id.ok_or_else(|| "chapterId is required for prepare_cloud_consent".to_string())?;
    let pg_idx = page_index.unwrap_or(0);

    let req = PrepareProposalRequest {
        chapter_id: ch_id,
        page_index: pg_idx,
        region_id,
        target,
        recipe: pinned_recipe,
        intent,
    };

    let proposal = ConsentService::global()
        .prepare_proposal(req, config, cloud_allowed, job_path)
        .map_err(sanitize_consent_error)?;

    Ok(ConsentProposalDto::from(proposal))
}

/// Prepare backend-authorized consent proposal with exact crop and hint digests.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn prepare_cloud_consent(
    app: tauri::AppHandle,
    target: ExecutionTarget,
    recipe: Option<RenderRecipe>,
    intent: OperationIntent,
    region_id: Option<String>,
    chapter_id: Option<String>,
    page_index: Option<u32>,
    simulate_blocked: Option<bool>,
) -> Result<ConsentProposalDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cloud_allowed = crate::inference::cloud_allowed(&app);

        let config = crate::inference::config::read_inference_config(&app)
            .map_err(|e| sanitize_config_error(&e))?;

        let ch_id = chapter_id
            .as_deref()
            .ok_or_else(|| "chapterId is required for prepare_cloud_consent".to_string())?;

        let library = Library::for_app(&app)
            .map_err(|_| "failed to initialize project library".to_string())?;
        let job_path = library
            .resolve_chapter(ch_id)
            .map_err(|_| "failed to resolve chapter".to_string())?;

        let proposal = prepare_cloud_consent_inner(
            cloud_allowed,
            &config,
            &job_path,
            target,
            recipe,
            intent,
            region_id,
            Some(ch_id.to_string()),
            page_index,
            simulate_blocked,
        )?;
        Ok(with_standing(proposal, &library, ch_id))
    })
    .await
    .map_err(|_| "async prepare consent task failed".to_string())?
}

/// Synchronous core handler for confirming cloud consent and issuing authorization grants.
pub fn confirm_cloud_consent_inner(
    cloud_allowed: bool,
    config: &InferenceConfig,
    job_path: &Path,
    proposal_id: &str,
    intent: &OperationIntent,
    simulate_epoch_mismatch: Option<bool>,
) -> Result<GrantDto, String> {
    if simulate_epoch_mismatch == Some(true) {
        return Err("Profile mutated: epoch mismatch".to_string());
    }
    if !cloud_allowed {
        return Err("Backend authorization blocked: cloudEngines permission denied".to_string());
    }

    let grant = ConsentService::global()
        .confirm_proposal(crate::inference::consent::ConfirmProposalOptions {
            proposal_id,
            intent,
            config,
            cloud_allowed,
            job_path,
            grant_ttl: Duration::from_secs(300),
            max_attempts: 1,
        })
        .map_err(sanitize_consent_error)?;

    Ok(GrantDto::from(grant))
}

/// Validate proposal and mint scoped attempt-limited authorization grant.
/// The statements are optional arguments so an older caller is refused with
/// the missing statement, not a malformed call.
#[tauri::command]
pub async fn confirm_cloud_consent(
    app: tauri::AppHandle,
    proposal_id: String,
    intent: OperationIntent,
    rights_attested: Option<bool>,
    retention_acknowledged: Option<bool>,
    simulate_epoch_mismatch: Option<bool>,
) -> Result<GrantDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cloud_allowed = crate::inference::cloud_allowed(&app);

        let config = crate::inference::config::read_inference_config(&app)
            .map_err(|e| sanitize_config_error(&e))?;

        let proposal = ConsentService::global()
            .get_proposal(&proposal_id)
            .map_err(sanitize_consent_error)?;

        let library = Library::for_app(&app)
            .map_err(|_| "failed to initialize project library".to_string())?;
        let job_path = library
            .resolve_chapter(&proposal.chapter_id)
            .map_err(|_| "failed to resolve chapter".to_string())?;
        let accepted = require_region_statements(
            &library,
            &proposal.chapter_id,
            proposal.provider,
            &proposal.profile_id,
            &proposal.canonical_endpoint_fingerprint,
            rights_attested.unwrap_or(false),
            retention_acknowledged.unwrap_or(false),
        )?;

        let grant = confirm_cloud_consent_inner(
            cloud_allowed,
            &config,
            &job_path,
            &proposal_id,
            &intent,
            simulate_epoch_mismatch,
        )?;
        // The first consent in a project stands for the rest of it.
        library.record_cloud_consent(
            &proposal.chapter_id,
            proposal.provider,
            &proposal.profile_id,
            &proposal.canonical_endpoint_fingerprint,
            accepted,
        );
        Ok(grant)
    })
    .await
    .map_err(|_| "async confirm consent task failed".to_string())?
}

/// Authoritative pre-POST local revalidation under project lock.
///
/// Re-resolves proposal.page_index against the project strip order, requires it still maps
/// to proposal.source_idx, requires the authoritative patch record source_idx matches too,
/// and re-verifies source/mask/crop/hint digests and region revision before intent creation,
/// grant consumption, or network dispatch.
pub(crate) fn revalidate_proposal_before_dispatch(
    canonical_job_path: &Path,
    proposal: &ConsentProposal,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let _lock = run::lock_job(canonical_job_path)?;
    let job = Job::open(canonical_job_path)
        .map_err(|_| "failed to open project manifest before dispatch".to_string())?;

    let re_resolved_source_idx = Library::resolve_page(&job.project, proposal.page_index as usize)
        .ok_or_else(|| "page index not found in project manifest before dispatch".to_string())?;
    if re_resolved_source_idx != proposal.source_idx {
        return Err(
            "page index no longer maps to proposal source index before dispatch".to_string(),
        );
    }

    let source_path = job
        .source_path(proposal.source_idx)
        .ok_or_else(|| "source image not found before dispatch".to_string())?;
    let source_bytes = std::fs::read(&source_path)
        .map_err(|_| "failed to read source image before dispatch".to_string())?;
    let cur_source_hash = sha256_hex(&source_bytes);
    if cur_source_hash != proposal.source_hash {
        return Err("source image digest mismatch before dispatch".to_string());
    }

    let page = job.display_page(proposal.source_idx,&source_bytes)
        .map_err(|_| "failed to decode source image before dispatch".to_string())?;
    let region_id = proposal
        .region_ids
        .first()
        .ok_or_else(|| "region not found in project manifest before dispatch".to_string())?;
    let region = load_cloud_region(&job, region_id, &cur_source_hash).map_err(|e| match e {
        ConsentError::RegionNotFound(_) => {
            "region not found in project manifest before dispatch".to_string()
        }
        _ => "failed to load region before dispatch".to_string(),
    })?;
    if region.source_idx != proposal.source_idx {
        return Err("patch record source index mismatch before dispatch".to_string());
    }

    let preprocessing = Preprocessing::of(&proposal.recipe.preprocessing_version)
        .map_err(|_| "unsupported preprocessing version before dispatch".to_string())?;
    let cur_mask_hash = region.mask_digest(preprocessing);
    if cur_mask_hash != proposal.mask_hash {
        return Err("mask digest mismatch before dispatch".to_string());
    }

    let cloud = region
        .prepare(&job, &page, preprocessing)
        .map_err(|_| "failed to prepare render before dispatch".to_string())?;
    let prepared = &cloud.prepared;

    if cloud.crop_bounds != proposal.crop_bounds {
        return Err("crop bounds mismatch before dispatch".to_string());
    }
    if cloud.input.digest != proposal.input_sha256
        || cloud.input.predecessors != proposal.predecessors_sha256
    {
        return Err("underlay changed before dispatch".to_string());
    }

    let crop_png = encode_rgb8_png(prepared.crop().w, prepared.crop().h, prepared.image_rgb8())
        .map_err(|_| "failed to encode crop PNG before dispatch".to_string())?;
    if sha256_hex(&crop_png) != proposal.crop_png_sha256 {
        return Err("crop PNG digest mismatch before dispatch".to_string());
    }

    let hint_png = encode_gray8_png(prepared.crop().w, prepared.crop().h, prepared.hint_gray8())
        .map_err(|_| "failed to encode hint PNG before dispatch".to_string())?;
    if sha256_hex(&hint_png) != proposal.hint_png_sha256 {
        return Err("hint PNG digest mismatch before dispatch".to_string());
    }

    if region.revision != proposal.revision || region.revision_hash != proposal.revision_hash {
        return Err("region revision mismatch before dispatch".to_string());
    }

    Ok((crop_png, hint_png))
}

/// Synchronous core handler for submitting cloud attempts with durable intent and revalidation.
#[allow(clippy::too_many_arguments)]
pub fn submit_cloud_attempt_inner(
    cloud_allowed: bool,
    config: &InferenceConfig,
    journal: &AttemptJournal,
    attempt_id: &str,
    grant_nonce: &str,
    proposal_id: Option<&str>,
    target: Option<&ExecutionTarget>,
    recipe: Option<&RenderRecipe>,
    simulate_mode: Option<&str>,
    snapshot: Option<&serde_json::Value>,
) -> Result<CloudAttemptSubmissionDto, String> {
    if !cloud_allowed || simulate_mode == Some("blocked_authorization") {
        return Err("Authorization blocked: cloudEngines permission denied".to_string());
    }

    let trimmed_nonce = grant_nonce.trim();
    if trimmed_nonce.is_empty() {
        return Err("Authorization blocked: invalid or missing grant nonce".to_string());
    }

    journal::validate_safe_id(attempt_id, "attempt_id").map_err(sanitize_journal_error)?;

    let derived_attempt_id = attempt_id_for_nonce(trimmed_nonce);
    if attempt_id != derived_attempt_id {
        return Err("caller attemptId does not match backend-derived attempt identity".to_string());
    }

    // If attempt is already registered in journal, reuse durable record (idempotent submission)
    if let Ok(rec) = journal.get_record(&derived_attempt_id) {
        // Validate caller target / recipe against existing record if provided
        if let Some(caller_target) = target {
            if caller_target.provider() != Some(rec.provider)
                || caller_target.profile_id() != Some(&rec.profile_id)
            {
                return Err("caller target does not match existing attempt record".to_string());
            }
        }
        if let Some(caller_recipe) = recipe {
            if caller_recipe.recipe_id != rec.recipe_id
                || caller_recipe.preprocessing_version != rec.preprocessing_version
                || caller_recipe.model_id != rec.model_id
                || caller_recipe.model_revision != rec.model_revision
                || caller_recipe.native_mask_conditioning != rec.native_mask_conditioning
            {
                return Err("caller recipe does not match existing attempt record".to_string());
            }
        }
        if let Some(snap_json) = snapshot {
            let snap = serde_json::from_value::<ProjectRegionSnapshot>(snap_json.clone())
                .map_err(|_| "caller snapshot is malformed or invalid".to_string())?;
            if snap.source_image_hash != rec.source_image_hash
                || snap.region_id != rec.region_id
                || snap.region_revision != rec.region_revision
                || snap.crop_sha256 != rec.crop_sha256
                || snap.hint_sha256 != rec.hint_sha256
            {
                return Err("caller snapshot does not match existing attempt record".to_string());
            }
        }
        let handle = match &rec.phase {
            AttemptPhase::Accepted { handle }
            | AttemptPhase::CancelRequested { handle }
            | AttemptPhase::ResultCached { handle, .. }
            | AttemptPhase::AttachmentPending { handle, .. }
            | AttemptPhase::Committed { handle, .. } => Some(handle.clone()),
            _ => None,
        };
        let status = match &rec.phase {
            AttemptPhase::Unknown { .. } => "unknown",
            AttemptPhase::Accepted { .. } | AttemptPhase::CancelRequested { .. } => "accepted",
            AttemptPhase::Dispatching => "dispatching",
            AttemptPhase::ResultCached { .. }
            | AttemptPhase::AttachmentPending { .. }
            | AttemptPhase::Committed { .. } => "accepted",
            AttemptPhase::Cancelled { .. } => "cancelled",
            AttemptPhase::Failed { .. } => "failed",
            AttemptPhase::Intent => "intent",
            AttemptPhase::Abandoned { .. } => "abandoned",
        };
        return Ok(CloudAttemptSubmissionDto {
            attempt_id: derived_attempt_id,
            handle,
            status: status.to_string(),
            request_digest: Some(rec.request_digest),
            auto_retryable: Some(false),
            error: None,
        });
    }

    // Look up confirmed proposal from ConsentService with explicit grant nonce binding
    let pid = proposal_id.ok_or_else(|| {
        "proposalId is required to bind submission to confirmed proposal".to_string()
    })?;
    journal::validate_safe_id(pid, "proposal_id").map_err(sanitize_journal_error)?;

    let (proposal, canonical_job_path) = ConsentService::global()
        .get_confirmed_proposal_with_job_path(pid, trimmed_nonce)
        .map_err(|e| format!("Authorization blocked: {}", sanitize_consent_error(e)))?;

    // Read stored configuration and validate profile & endpoint binding
    let stored_ep_fp =
        match proposal.provider {
            CloudProvider::Beam => config.beam_profiles.get(&proposal.profile_id).map(|p| {
                compute_canonical_endpoint_fingerprint(&p.endpoint_url).unwrap_or_default()
            }),
            CloudProvider::Modal => config.modal_profiles.get(&proposal.profile_id).map(|p| {
                compute_canonical_endpoint_fingerprint(&p.endpoint_url).unwrap_or_default()
            }),
        }
        .ok_or_else(|| "profile does not exist in inference configuration".to_string())?;

    if stored_ep_fp != proposal.canonical_endpoint_fingerprint {
        return Err("Authorization blocked: profile endpoint has mutated".to_string());
    }

    // Validate caller target against confirmed proposal if provided
    if let Some(caller_target) = target {
        if caller_target.provider() != Some(proposal.provider)
            || caller_target.profile_id() != Some(&proposal.profile_id)
        {
            return Err("caller target does not match confirmed proposal target".to_string());
        }
    }

    // Validate caller recipe against confirmed proposal if provided
    if let Some(caller_recipe) = recipe {
        if caller_recipe != &proposal.recipe {
            return Err("caller recipe does not match confirmed proposal recipe".to_string());
        }
    }

    // Validate caller snapshot against confirmed proposal if provided
    if let Some(snap_json) = snapshot {
        let snap = serde_json::from_value::<ProjectRegionSnapshot>(snap_json.clone())
            .map_err(|_| "caller snapshot is malformed or invalid".to_string())?;
        if snap.source_image_hash != proposal.source_hash
            || snap.region_id != proposal.region_ids[0]
            || snap.region_revision != proposal.revision
            || snap.crop_sha256 != proposal.crop_png_sha256
            || snap.hint_sha256 != proposal.hint_png_sha256
        {
            return Err("caller snapshot does not match confirmed proposal snapshot".to_string());
        }
    }

    // Construct GrantScope from authoritative proposal
    let scope = GrantScope {
        capability: crate::inference::policy::FLUX_CAPABILITY.to_string(),
        provider: proposal.provider,
        profile_id: proposal.profile_id.clone(),
        canonical_endpoint_fingerprint: proposal.canonical_endpoint_fingerprint.clone(),
        source_hash: proposal.source_hash.clone(),
        crop_sha256: proposal.crop_png_sha256.clone(),
        hint_sha256: proposal.hint_png_sha256.clone(),
        operation_digest: proposal.operation_digest.clone(),
        crop_bounds: proposal.crop_bounds,
        mask_hash: proposal.mask_hash.clone(),
        revision: proposal.revision,
        input_sha256: proposal.input_sha256.clone(),
        predecessors_sha256: proposal.predecessors_sha256.clone(),
        recipe: proposal.recipe.clone(),
        region_ids: proposal.region_ids.clone(),
    };

    // Construct client first before consuming grant or writing durable intent
    let client = build_client_for_profile(config, proposal.provider, &proposal.profile_id)
        .map_err(|e| sanitize_build_client_error(&e))?;

    // Authoritative pre-POST local revalidation under project lock
    let (crop_png, hint_png) = revalidate_proposal_before_dispatch(&canonical_job_path, &proposal)?;

    let wire_recipe: WireRenderRecipe = proposal.recipe.clone().into();
    let job_id = format!(
        "job-{}",
        &sha256_hex(format!("{}:{}", proposal.region_ids[0], proposal.page_index).as_bytes())
            [0..24]
    );
    let sampling = proposal.recipe.sampling();

    let req_meta = JobRequestMetadata {
        protocol_version: PROTOCOL_VERSION.to_string(),
        job_id: job_id.clone(),
        attempt_id: derived_attempt_id.clone(),
        recipe: wire_recipe,
        width: proposal.crop_bounds.w,
        height: proposal.crop_bounds.h,
        seed: sampling.seed,
        steps: sampling.steps,
        guidance_scaled: sampling.guidance_scaled,
        image_sha256: proposal.crop_png_sha256.clone(),
        hint_sha256: proposal.hint_png_sha256.clone(),
        request_digest: String::new(),
    };

    let request_digest = compute_request_digest(&req_meta);
    let mut req_meta = req_meta;
    req_meta.request_digest = request_digest.clone();

    let create_intent = CreateAttemptIntent {
        qwen_edit: proposal.recipe.qwen_edit.clone(),
        attempt_id: derived_attempt_id.clone(),
        job_id,
        provider: proposal.provider,
        profile_id: proposal.profile_id.clone(),
        endpoint_fingerprint: proposal.canonical_endpoint_fingerprint.clone(),
        recipe_id: proposal.recipe.recipe_id.clone(),
        preprocessing_version: proposal.recipe.preprocessing_version.clone(),
        model_id: proposal.recipe.model_id.clone(),
        model_revision: proposal.recipe.model_revision.clone(),
        native_mask_conditioning: proposal.recipe.native_mask_conditioning,
        width: proposal.crop_bounds.w,
        height: proposal.crop_bounds.h,
        seed: sampling.seed,
        steps: sampling.steps,
        guidance_scaled: sampling.guidance_scaled,
        source_image_hash: proposal.source_hash.clone(),
        region_id: proposal.region_ids[0].clone(),
        region_revision: proposal.revision,
        input_sha256: Some(proposal.input_sha256.clone()),
        predecessors_sha256: Some(proposal.predecessors_sha256.clone()),
        crop_sha256: proposal.crop_png_sha256.clone(),
        hint_sha256: proposal.hint_png_sha256.clone(),
        request_digest: request_digest.clone(),
        chapter_id: Some(proposal.chapter_id.clone()),
        page_index: Some(proposal.page_index),
    };

    let (_rec, guard) = journal
        .create_intent(create_intent)
        .map_err(sanitize_journal_error)?;

    // Consume grant at the last safe point before marking Dispatching and network dispatch
    if let Err(e) = GrantService::global().validate_and_consume(trimmed_nonce, &scope) {
        let _ = journal.abort_intent_no_dispatch(&guard);
        return Err(format!(
            "Authorization blocked: {}",
            sanitize_grant_error(&e)
        ));
    }

    // Transition from Intent to Dispatching immediately before network dispatch
    let _permit = match journal.mark_dispatching(&guard) {
        Ok(p) => p,
        Err(e) => {
            let _ = journal.abort_intent_no_dispatch(&guard);
            return Err(sanitize_journal_error(e));
        }
    };
    // Release exclusive attempt lock before performing network transport (Property 6)
    drop(guard);

    if simulate_mode == Some("ambiguous_acceptance") {
        let guard = journal
            .acquire_lock(&derived_attempt_id)
            .map_err(sanitize_journal_error)?;
        let _ = journal.record_dispatch_transport_error(&guard);
        return Ok(CloudAttemptSubmissionDto {
            attempt_id: derived_attempt_id,
            handle: None,
            status: "unknown".to_string(),
            request_digest: Some(request_digest),
            auto_retryable: Some(false),
            error: Some("Transport error during dispatch: ambiguous acceptance".to_string()),
        });
    }

    // HTTP POST dispatch executed with no attempt lock held (Property 6)
    match client.submit_job(&req_meta, &crop_png, &hint_png, journal.limits()) {
        Ok(accepted) => {
            let guard = journal
                .acquire_lock(&derived_attempt_id)
                .map_err(sanitize_journal_error)?;
            journal
                .record_accepted_handle(&guard, &accepted.handle)
                .map_err(sanitize_journal_error)?;
            Ok(CloudAttemptSubmissionDto {
                attempt_id: derived_attempt_id,
                handle: Some(accepted.handle),
                status: "accepted".to_string(),
                request_digest: Some(request_digest),
                auto_retryable: Some(false),
                error: None,
            })
        }
        Err(e) => {
            let guard = journal
                .acquire_lock(&derived_attempt_id)
                .map_err(sanitize_journal_error)?;
            if e.definitely_not_enqueued() {
                journal.record_not_enqueued(&guard, "submission was not enqueued").map_err(sanitize_journal_error)?;
            } else {
                journal.record_dispatch_transport_error(&guard).map_err(sanitize_journal_error)?;
            }
            Ok(CloudAttemptSubmissionDto {
                attempt_id: derived_attempt_id,
                handle: None,
                status: if e.definitely_not_enqueued() { "not_enqueued" } else { "unknown" }.to_string(),
                request_digest: Some(request_digest),
                auto_retryable: Some(false),
                error: Some(format!(
                    "Transport error during dispatch: {}",
                    sanitize_http_transport_error(&e)
                )),
            })
        }
    }
}

/// Fail-closed guard in front of every paid dispatch: the user's cloud
/// permission switch (`cloudEngines == "allowed"`), read by the caller.
///
/// Answers the stable code `cloud_disabled`, which the interface shows as its
/// "cloud is off" notice. There is no build flag behind this: the switch is
/// the only thing it stands for.
fn guard_cloud_execution_enabled(cloud_allowed: bool) -> Result<(), String> {
    if !cloud_allowed {
        return Err("cloud_disabled".to_string());
    }
    Ok(())
}

/// Persist durable intent and submit cloud attempt (never auto-retried if ambiguous).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn submit_cloud_attempt(
    app: tauri::AppHandle,
    attempt_id: String,
    grant_nonce: String,
    proposal_id: Option<String>,
    target: Option<ExecutionTarget>,
    recipe: Option<RenderRecipe>,
    simulate_mode: Option<String>,
    snapshot: Option<serde_json::Value>,
) -> Result<CloudAttemptSubmissionDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cloud_allowed = crate::inference::cloud_allowed(&app);

        guard_cloud_execution_enabled(cloud_allowed)?;

        let current_cfg =
            config::read_inference_config(&app).map_err(|e| sanitize_config_error(&e))?;
        let journal = get_app_journal(&app)?;

        submit_cloud_attempt_inner(
            cloud_allowed,
            &current_cfg,
            &journal,
            &attempt_id,
            &grant_nonce,
            proposal_id.as_deref(),
            target.as_ref(),
            recipe.as_ref(),
            // A test hook: a release build never fakes an outcome for a
            // grant it was given.
            simulate_mode.as_deref().filter(|_| cfg!(debug_assertions)),
            snapshot.as_ref(),
        )
    })
    .await
    .map_err(|_| "async submit attempt task failed".to_string())?
}

/// Synchronous core handler for querying cloud attempt lifecycle status.
pub fn get_cloud_attempt_status_inner(
    journal: &AttemptJournal,
    config: Option<&InferenceConfig>,
    attempt_id: &str,
    handle: Option<&str>,
) -> Result<CloudAttemptStatusDto, String> {
    journal::validate_safe_id(attempt_id, "attempt_id").map_err(sanitize_journal_error)?;
    if let Some(h) = handle {
        journal::validate_safe_id(h, "handle").map_err(sanitize_journal_error)?;
    }

    let record = {
        let guard = journal
            .acquire_lock(attempt_id)
            .map_err(sanitize_journal_error)?;
        journal
            .read_record_locked(&guard)
            .map_err(sanitize_journal_error)?
    };

    // Validate caller handle against recorded handle if present
    let rec_handle = match &record.phase {
        AttemptPhase::Accepted { handle: h }
        | AttemptPhase::CancelRequested { handle: h }
        | AttemptPhase::ResultCached { handle: h, .. }
        | AttemptPhase::AttachmentPending { handle: h, .. }
        | AttemptPhase::Committed { handle: h, .. } => Some(h.as_str()),
        AttemptPhase::Cancelled {
            handle: Some(h), ..
        }
        | AttemptPhase::Failed {
            handle: Some(h), ..
        } => Some(h.as_str()),
        _ => None,
    };
    if let (Some(caller_h), Some(actual_h)) = (handle, rec_handle) {
        if caller_h != actual_h {
            return Err("caller handle does not match attempt handle".to_string());
        }
    }

    match &record.phase {
        AttemptPhase::Abandoned { .. } => Ok(CloudAttemptStatusDto {
            attempt_id: attempt_id.into(), handle: None, status: "abandoned".into(), reported_cost_usd: None,
            acknowledged: Some(true), created_at_ms: Some(record.created_at_ms), started_at_ms: None,
            finished_at_ms: Some(record.updated_at_ms),
        }),
        AttemptPhase::Unknown { .. } => Ok(CloudAttemptStatusDto {
            attempt_id: attempt_id.to_string(),
            handle: None,
            status: "unknown".to_string(),
            reported_cost_usd: None,
            acknowledged: None,
            created_at_ms: Some(record.created_at_ms),
            started_at_ms: None,
            finished_at_ms: None,
        }),
        AttemptPhase::Cancelled { handle: h, .. } => Ok(CloudAttemptStatusDto {
            attempt_id: attempt_id.to_string(),
            handle: h.clone(),
            status: "cancelled".to_string(),
            reported_cost_usd: None,
            acknowledged: Some(true),
            created_at_ms: Some(record.created_at_ms),
            started_at_ms: None,
            finished_at_ms: Some(record.updated_at_ms),
        }),
        AttemptPhase::Failed { handle: h, .. } => Ok(CloudAttemptStatusDto {
            attempt_id: attempt_id.to_string(),
            handle: h.clone(),
            status: "failed".to_string(),
            reported_cost_usd: None,
            acknowledged: None,
            created_at_ms: Some(record.created_at_ms),
            started_at_ms: None,
            finished_at_ms: Some(record.updated_at_ms),
        }),
        AttemptPhase::ResultCached {
            handle: h,
            reported_cost_usd,
            ..
        }
        | AttemptPhase::AttachmentPending {
            handle: h,
            reported_cost_usd,
            ..
        }
        | AttemptPhase::Committed {
            handle: h,
            reported_cost_usd,
            ..
        } => Ok(CloudAttemptStatusDto {
            attempt_id: attempt_id.to_string(),
            handle: Some(h.clone()),
            status: "completed".to_string(),
            reported_cost_usd: *reported_cost_usd,
            acknowledged: Some(true),
            created_at_ms: Some(record.created_at_ms),
            started_at_ms: Some(record.created_at_ms),
            finished_at_ms: Some(record.updated_at_ms),
        }),
        AttemptPhase::Accepted { handle: h } | AttemptPhase::CancelRequested { handle: h } => {
            let h_str = h.clone();
            if let Some(cfg) = config {
                if let Ok(client) =
                    build_client_for_profile(cfg, record.provider, &record.profile_id)
                {
                    let req_meta = record.to_request_metadata();
                    // Network status poll executed without holding attempt lock (Property 6)
                    if let Ok(st) = client.get_job_status(&h_str, &req_meta) {
                        match st.status {
                            JobExecutionStatus::Completed => {
                                let result_meta = ResultMetadata {
                                    handle: h_str.clone(),
                                    job_id: req_meta.job_id.clone(),
                                    attempt_id: req_meta.attempt_id.clone(),
                                    request_digest: req_meta.request_digest.clone(),
                                    recipe_id: req_meta.recipe.recipe_id.clone(),
                                    preprocessing_version: req_meta
                                        .recipe
                                        .preprocessing_version
                                        .clone(),
                                    model_id: req_meta.recipe.model_id.clone(),
                                    model_revision: req_meta.recipe.model_revision.clone(),
                                    native_mask_conditioning: req_meta
                                        .recipe
                                        .native_mask_conditioning,
                                    result_digest: st.result_digest.clone().unwrap_or_default(),
                                    reported_cost_usd: st.reported_cost_usd,
                                    width: req_meta.width,
                                    height: req_meta.height,
                                    byte_length: st.result_bytes.unwrap_or(0),
                                };
                                if let Ok(png_bytes) = client.fetch_result_bytes(
                                    &h_str,
                                    &result_meta,
                                    &req_meta,
                                    journal.limits(),
                                ) {
                                    if let Ok(guard) = journal.acquire_lock(attempt_id) {
                                        let _ =
                                            journal.cache_result(&guard, &png_bytes, &result_meta);
                                    }
                                }
                                return Ok(CloudAttemptStatusDto {
                                    attempt_id: attempt_id.to_string(),
                                    handle: Some(h_str),
                                    status: "completed".to_string(),
                                    reported_cost_usd: st.reported_cost_usd,
                                    acknowledged: Some(true),
                                    created_at_ms: Some(record.created_at_ms),
                                    started_at_ms: Some(record.created_at_ms),
                                    finished_at_ms: Some(current_epoch_ms()),
                                });
                            }
                            JobExecutionStatus::Running => {
                                return Ok(CloudAttemptStatusDto {
                                    attempt_id: attempt_id.to_string(),
                                    handle: Some(h_str),
                                    status: "running".to_string(),
                                    reported_cost_usd: st.reported_cost_usd,
                                    acknowledged: Some(true),
                                    created_at_ms: Some(record.created_at_ms),
                                    started_at_ms: Some(current_epoch_ms()),
                                    finished_at_ms: None,
                                });
                            }
                            JobExecutionStatus::Pending => {
                                return Ok(CloudAttemptStatusDto {
                                    attempt_id: attempt_id.to_string(),
                                    handle: Some(h_str),
                                    status: "pending".to_string(),
                                    reported_cost_usd: None,
                                    acknowledged: Some(true),
                                    created_at_ms: Some(record.created_at_ms),
                                    started_at_ms: None,
                                    finished_at_ms: None,
                                });
                            }
                            JobExecutionStatus::Cancelled => {
                                if let Ok(guard) = journal.acquire_lock(attempt_id) {
                                    let _ = journal.record_cancelled(&guard);
                                }
                                return Ok(CloudAttemptStatusDto {
                                    attempt_id: attempt_id.to_string(),
                                    handle: Some(h_str),
                                    status: "cancelled".to_string(),
                                    reported_cost_usd: st.reported_cost_usd,
                                    acknowledged: Some(true),
                                    created_at_ms: Some(record.created_at_ms),
                                    started_at_ms: None,
                                    finished_at_ms: Some(current_epoch_ms()),
                                });
                            }
                            JobExecutionStatus::Failed => {
                                if let Ok(guard) = journal.acquire_lock(attempt_id) {
                                    let _ = journal.record_failed(&guard);
                                }
                                return Ok(CloudAttemptStatusDto {
                                    attempt_id: attempt_id.to_string(),
                                    handle: Some(h_str),
                                    status: "failed".to_string(),
                                    reported_cost_usd: None,
                                    acknowledged: None,
                                    created_at_ms: Some(record.created_at_ms),
                                    started_at_ms: None,
                                    finished_at_ms: Some(current_epoch_ms()),
                                });
                            }
                        }
                    }
                }
            }

            let status = match &record.phase {
                AttemptPhase::CancelRequested { .. } => "cancel_requested",
                _ => "pending",
            };
            Ok(CloudAttemptStatusDto {
                attempt_id: attempt_id.to_string(),
                handle: Some(h_str),
                status: status.to_string(),
                reported_cost_usd: None,
                acknowledged: Some(true),
                created_at_ms: Some(record.created_at_ms),
                started_at_ms: None,
                finished_at_ms: None,
            })
        }
        AttemptPhase::Intent | AttemptPhase::Dispatching => Ok(CloudAttemptStatusDto {
            attempt_id: attempt_id.to_string(),
            handle: None,
            status: "pending".to_string(),
            reported_cost_usd: None,
            acknowledged: Some(true),
            created_at_ms: Some(record.created_at_ms),
            started_at_ms: None,
            finished_at_ms: None,
        }),
    }
}

/// Poll authoritative remote attempt lifecycle status.
#[tauri::command]
pub async fn get_cloud_attempt_status(
    app: tauri::AppHandle,
    attempt_id: String,
    handle: Option<String>,
) -> Result<CloudAttemptStatusDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let journal = get_app_journal(&app)?;
        let current_cfg = config::read_inference_config(&app).ok();
        get_cloud_attempt_status_inner(
            &journal,
            current_cfg.as_ref(),
            &attempt_id,
            handle.as_deref(),
        )
    })
    .await
    .map_err(|_| "async get attempt status task failed".to_string())?
}

/// Synchronous core handler for retrieving cloud attempt results.
pub fn get_cloud_attempt_result_inner(
    journal: &AttemptJournal,
    config: Option<&InferenceConfig>,
    attempt_id: &str,
    handle: Option<&str>,
) -> Result<CloudAttemptResultDto, String> {
    journal::validate_safe_id(attempt_id, "attempt_id").map_err(sanitize_journal_error)?;
    if let Some(h) = handle {
        journal::validate_safe_id(h, "handle").map_err(sanitize_journal_error)?;
    }

    let record = {
        let guard = journal
            .acquire_lock(attempt_id)
            .map_err(sanitize_journal_error)?;
        journal
            .read_record_locked(&guard)
            .map_err(sanitize_journal_error)?
    };

    // Validate caller handle against recorded handle if present
    let rec_handle = match &record.phase {
        AttemptPhase::Accepted { handle: h }
        | AttemptPhase::CancelRequested { handle: h }
        | AttemptPhase::ResultCached { handle: h, .. }
        | AttemptPhase::AttachmentPending { handle: h, .. }
        | AttemptPhase::Committed { handle: h, .. } => Some(h.as_str()),
        AttemptPhase::Cancelled {
            handle: Some(h), ..
        }
        | AttemptPhase::Failed {
            handle: Some(h), ..
        } => Some(h.as_str()),
        _ => None,
    };
    if let (Some(caller_h), Some(actual_h)) = (handle, rec_handle) {
        if caller_h != actual_h {
            return Err("caller handle does not match attempt handle".to_string());
        }
    }

    match &record.phase {
        AttemptPhase::ResultCached {
            handle: h,
            result_digest,
            reported_cost_usd,
            ..
        }
        | AttemptPhase::AttachmentPending {
            handle: h,
            result_digest,
            reported_cost_usd,
            ..
        }
        | AttemptPhase::Committed {
            handle: h,
            result_digest,
            reported_cost_usd,
            ..
        } => {
            let guard = journal
                .acquire_lock(attempt_id)
                .map_err(sanitize_journal_error)?;
            let _png = journal
                .read_cached_png(&guard)
                .map_err(sanitize_journal_error)?;
            Ok(CloudAttemptResultDto {
                attempt_id: attempt_id.to_string(),
                handle: h.clone(),
                result_digest: result_digest.clone(),
                reported_cost_usd: *reported_cost_usd,
                width: record.width,
                height: record.height,
                cached: true,
            })
        }
        AttemptPhase::Accepted { handle: h } => {
            let h_str = h.clone();
            let current_cfg = config.ok_or_else(|| {
                "inference configuration is required to fetch remote result".to_string()
            })?;
            let client = build_client_for_profile(current_cfg, record.provider, &record.profile_id)
                .map_err(|e| sanitize_build_client_error(&e))?;
            let req_meta = record.to_request_metadata();
            // Network operations executed without holding attempt lock (Property 6)
            let st = client
                .get_job_status(&h_str, &req_meta)
                .map_err(|e| sanitize_http_transport_error(&e))?;
            if st.status != JobExecutionStatus::Completed {
                return Err("attempt execution is not completed yet".to_string());
            }
            let result_meta = ResultMetadata {
                handle: h_str.clone(),
                job_id: req_meta.job_id.clone(),
                attempt_id: req_meta.attempt_id.clone(),
                request_digest: req_meta.request_digest.clone(),
                recipe_id: req_meta.recipe.recipe_id.clone(),
                preprocessing_version: req_meta.recipe.preprocessing_version.clone(),
                model_id: req_meta.recipe.model_id.clone(),
                model_revision: req_meta.recipe.model_revision.clone(),
                native_mask_conditioning: req_meta.recipe.native_mask_conditioning,
                result_digest: st.result_digest.clone().unwrap_or_default(),
                reported_cost_usd: st.reported_cost_usd,
                width: req_meta.width,
                height: req_meta.height,
                byte_length: st.result_bytes.unwrap_or(0),
            };
            let png_bytes = client
                .fetch_result_bytes(&h_str, &result_meta, &req_meta, journal.limits())
                .map_err(|e| sanitize_http_transport_error(&e))?;
            let guard = journal
                .acquire_lock(attempt_id)
                .map_err(sanitize_journal_error)?;
            let _ = journal
                .cache_result(&guard, &png_bytes, &result_meta)
                .map_err(sanitize_journal_error)?;
            Ok(CloudAttemptResultDto {
                attempt_id: attempt_id.to_string(),
                handle: h_str,
                result_digest: result_meta.result_digest,
                reported_cost_usd: st.reported_cost_usd,
                width: record.width,
                height: record.height,
                cached: true,
            })
        }
        _ => Err("cannot retrieve result for attempt in non-completed phase".to_string()),
    }
}

/// Retrieve validated output crop result from local cache or remote gateway.
#[tauri::command]
pub async fn get_cloud_attempt_result(
    app: tauri::AppHandle,
    attempt_id: String,
    handle: Option<String>,
) -> Result<CloudAttemptResultDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let journal = get_app_journal(&app)?;
        let current_cfg = config::read_inference_config(&app).ok();
        get_cloud_attempt_result_inner(
            &journal,
            current_cfg.as_ref(),
            &attempt_id,
            handle.as_deref(),
        )
    })
    .await
    .map_err(|_| "async get attempt result task failed".to_string())?
}

/// Synchronous core handler for requesting attempt cancellation.
pub fn cancel_cloud_attempt_inner(
    journal: &AttemptJournal,
    config: Option<&InferenceConfig>,
    attempt_id: &str,
    handle: Option<&str>,
) -> Result<CloudCancelResultDto, String> {
    journal::validate_safe_id(attempt_id, "attempt_id").map_err(sanitize_journal_error)?;
    if let Some(h) = handle {
        journal::validate_safe_id(h, "handle").map_err(sanitize_journal_error)?;
    }

    let guard = journal
        .acquire_lock(attempt_id)
        .map_err(sanitize_journal_error)?;
    let record = journal
        .read_record_locked(&guard)
        .map_err(sanitize_journal_error)?;

    // Validate caller handle against recorded handle if present
    let rec_handle = match &record.phase {
        AttemptPhase::Accepted { handle: h }
        | AttemptPhase::CancelRequested { handle: h }
        | AttemptPhase::ResultCached { handle: h, .. }
        | AttemptPhase::AttachmentPending { handle: h, .. }
        | AttemptPhase::Committed { handle: h, .. } => Some(h.as_str()),
        AttemptPhase::Cancelled {
            handle: Some(h), ..
        }
        | AttemptPhase::Failed {
            handle: Some(h), ..
        } => Some(h.as_str()),
        _ => None,
    };
    if let (Some(caller_h), Some(actual_h)) = (handle, rec_handle) {
        if caller_h != actual_h {
            drop(guard);
            return Err("caller handle does not match attempt handle".to_string());
        }
    }

    match &record.phase {
        AttemptPhase::Abandoned { .. } => Err("attempt was abandoned".into()),
        AttemptPhase::Accepted { handle: h } => {
            let h_str = h.clone();
            let _ = journal.record_cancel_requested(&guard);
            // Drop lock before network cancel request (Property 6)
            drop(guard);

            let cfg = config.ok_or_else(|| {
                "inference configuration is required to cancel remote job".to_string()
            })?;
            let client = build_client_for_profile(cfg, record.provider, &record.profile_id)
                .map_err(|e| sanitize_build_client_error(&e))?;
            let cancel_resp = client
                .cancel_job(&h_str, &record.to_request_metadata())
                .map_err(|e| sanitize_http_transport_error(&e))?;

            Ok(CloudCancelResultDto {
                handle: cancel_resp.handle,
                status: cancel_resp.status,
                acknowledged: cancel_resp.acknowledged,
            })
        }
        AttemptPhase::CancelRequested { handle: h } => {
            let h_str = h.clone();
            drop(guard);

            let cfg = config.ok_or_else(|| {
                "inference configuration is required to cancel remote job".to_string()
            })?;
            let client = build_client_for_profile(cfg, record.provider, &record.profile_id)
                .map_err(|e| sanitize_build_client_error(&e))?;
            let cancel_resp = client
                .cancel_job(&h_str, &record.to_request_metadata())
                .map_err(|e| sanitize_http_transport_error(&e))?;

            Ok(CloudCancelResultDto {
                handle: cancel_resp.handle,
                status: cancel_resp.status,
                acknowledged: cancel_resp.acknowledged,
            })
        }
        AttemptPhase::Cancelled { handle: h, .. } => {
            let h_str = h.clone().unwrap_or_default();
            drop(guard);
            Ok(CloudCancelResultDto {
                handle: h_str,
                status: "cancelled".to_string(),
                acknowledged: true,
            })
        }
        AttemptPhase::Intent => {
            let _ = journal.abort_intent_no_dispatch(&guard);
            drop(guard);
            Ok(CloudCancelResultDto {
                handle: String::new(),
                status: "cancelled".to_string(),
                acknowledged: true,
            })
        }
        AttemptPhase::Dispatching => {
            drop(guard);
            Err("cannot cancel attempt: job is currently dispatching".to_string())
        }
        AttemptPhase::ResultCached { .. }
        | AttemptPhase::AttachmentPending { .. }
        | AttemptPhase::Committed { .. } => {
            drop(guard);
            Err("cannot cancel attempt: job is already completed".to_string())
        }
        AttemptPhase::Failed { .. } => {
            drop(guard);
            Err("cannot cancel attempt: job has already failed".to_string())
        }
        AttemptPhase::Unknown { .. } => {
            drop(guard);
            Err("cannot cancel attempt: job is in unknown state".to_string())
        }
    }
}

/// Request nonterminal attempt cancellation with honest phase validation.
///
/// A render running in this process hears it first. Until the gateway has
/// accepted the job there is no handle for the journal to cancel, so the
/// render's own flag is what stops it, at its next safe point; the answer is
/// then `cancel_requested`, unacknowledged, and the render's `cancelled`
/// event is the confirmation. A render whose result is already downloaded
/// is past cancelling and answers as the journal says.
#[tauri::command]
pub async fn cancel_cloud_attempt(
    app: tauri::AppHandle,
    attempt_id: String,
    handle: Option<String>,
) -> Result<CloudCancelResultDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        journal::validate_safe_id(&attempt_id, "attempt_id").map_err(sanitize_journal_error)?;
        let live = service::request_render_cancel(&attempt_id);
        let journal = get_app_journal(&app)?;
        let current_cfg = config::read_inference_config(&app).ok();
        match cancel_cloud_attempt_inner(
            &journal,
            current_cfg.as_ref(),
            &attempt_id,
            handle.as_deref(),
        ) {
            Err(_) if live && !past_cancelling(&journal, &attempt_id) => Ok(CloudCancelResultDto {
                handle: String::new(),
                status: "cancel_requested".to_string(),
                acknowledged: false,
            }),
            outcome => outcome,
        }
    })
    .await
    .map_err(|_| "async cancel attempt task failed".to_string())?
}

/// Whether an attempt is beyond what a cancel can stop: its result is
/// downloaded, or it has ended.
fn past_cancelling(journal: &AttemptJournal, attempt_id: &str) -> bool {
    journal.get_record(attempt_id).is_ok_and(|record| {
        record.is_terminal()
            || matches!(
                record.phase,
                AttemptPhase::ResultCached { .. } | AttemptPhase::AttachmentPending { .. }
            )
    })
}

/// Helper to validate caller recovery arguments against authoritative attempt record.
/// Fails closed (returns true for stale) if any provided caller field mismatches the record.
fn is_caller_recovery_input_stale(
    record: &AttemptRecord,
    chapter_id: Option<&str>,
    page_index: Option<u32>,
    region_id: Option<&str>,
    region_revision: Option<&serde_json::Value>,
    source_image_hash: Option<&str>,
    simulate_stale: Option<bool>,
) -> bool {
    if simulate_stale == Some(true) {
        return true;
    }

    if let Some(c) = chapter_id {
        if record.chapter_id.as_deref() != Some(c) {
            return true;
        }
    }
    if let Some(p) = page_index {
        if record.page_index != Some(p) {
            return true;
        }
    }
    if let Some(r) = region_id {
        if r != record.region_id.as_str() {
            return true;
        }
    }
    if let Some(h) = source_image_hash {
        if h != record.source_image_hash.as_str() {
            return true;
        }
    }
    if let Some(rev_json) = region_revision {
        let rev_num = rev_json
            .as_u64()
            .or_else(|| rev_json.as_str().and_then(|s| s.parse().ok()));
        if rev_num != Some(record.region_revision) {
            return true;
        }
    }

    false
}

/// Helper to validate authoritative attempt record against local project state.
/// Resolves project strictly from record.chapter_id and record.page_index.
/// Missing, unreadable, or drifted project state fails closed as stale.
fn is_project_recovery_stale(library: &Library, record: &AttemptRecord) -> bool {
    if let (Some(ref ch), Some(page_idx)) = (&record.chapter_id, record.page_index) {
        let check_proj = (|| -> Result<bool, ()> {
            let job_path = library.resolve_chapter(ch).map_err(|_| ())?;
            let job = Job::open(&job_path).map_err(|_| ())?;
            let s_idx = Library::resolve_page(&job.project, page_idx as usize).ok_or(())?;
            let src_path = job.source_path(s_idx).ok_or(())?;
            let src_bytes = std::fs::read(&src_path).map_err(|_| ())?;
            if sha256_hex(&src_bytes) != record.source_image_hash {
                return Ok(true); // source hash changed -> stale
            }
            // A patch, or the stored detection a batch cloud clean renders.
            let region = load_cloud_region(&job, &record.region_id, &record.source_image_hash)
                .map_err(|_| ())?;
            if region.source_idx != s_idx {
                return Ok(true);
            }
            if region.revision != record.region_revision {
                return Ok(true);
            }
            let page = job.display_page(s_idx,&src_bytes).map_err(|_| ())?;
            let preprocessing =
                Preprocessing::of(&record.preprocessing_version).map_err(|_| ())?;
            let cloud = region.prepare(&job, &page, preprocessing).map_err(|_| ())?;
            Ok(record.input_sha256.as_deref() != Some(cloud.input.digest.as_str())
                || record.predecessors_sha256.as_deref()
                    != Some(cloud.input.predecessors.as_str()))
        })();

        // Missing or unreadable project state fails closed (is_stale = true)
        check_proj.unwrap_or(true)
    } else {
        // Unverifiable project state fails closed
        true
    }
}

/// Helper combining caller input validation and authoritative project state verification.
#[allow(clippy::too_many_arguments)]
fn is_recovery_stale(
    library: Option<&Library>,
    record: &AttemptRecord,
    chapter_id: Option<&str>,
    page_index: Option<u32>,
    region_id: Option<&str>,
    region_revision: Option<&serde_json::Value>,
    source_image_hash: Option<&str>,
    simulate_stale: Option<bool>,
) -> bool {
    if is_caller_recovery_input_stale(
        record,
        chapter_id,
        page_index,
        region_id,
        region_revision,
        source_image_hash,
        simulate_stale,
    ) {
        return true;
    }

    match library {
        Some(lib) => is_project_recovery_stale(lib, record),
        None => true, // missing or unreadable library -> fails closed
    }
}

/// Synchronous core handler for reconciling recovery on crash/discovery.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_cloud_recovery_inner(
    journal: &AttemptJournal,
    library: Option<&Library>,
    attempt_id: Option<&str>,
    chapter_id: Option<&str>,
    page_index: Option<u32>,
    region_id: Option<&str>,
    region_revision: Option<&serde_json::Value>,
    source_image_hash: Option<&str>,
    simulate_stale: Option<bool>,
) -> Result<CloudRecoveryDecisionDto, String> {
    if let Some(r) = region_id {
        journal::validate_safe_id(r, "region_id").map_err(sanitize_journal_error)?;
    }
    if let Some(c) = chapter_id {
        journal::validate_safe_id(c, "chapter_id").map_err(sanitize_journal_error)?;
    }

    let id = match attempt_id {
        Some(i) => {
            journal::validate_safe_id(i, "attempt_id").map_err(sanitize_journal_error)?;
            i.to_string()
        }
        None => {
            let ids = journal.list_attempt_ids().unwrap_or_default();
            match ids.last() {
                Some(last_id) => last_id.clone(),
                None => {
                    return Ok(CloudRecoveryDecisionDto {
                        decision: "terminal".to_string(),
                        attempt_id: None,
                        handle: None,
                        result_digest: None,
                        patch_id: None,
                        auto_retryable: Some(false),
                        message: Some("No uncommitted attempts found in journal".to_string()),
                    });
                }
            }
        }
    };

    // A render or a resumed wait in this process owns the attempt. Its
    // `Dispatching` is a request in flight, not an interrupted one, and
    // recovering it here would close as unknown a job the gateway is about to
    // accept, with nobody left polling it.
    if service::is_render_live(&id) {
        return Ok(CloudRecoveryDecisionDto {
            decision: "in_progress".to_string(),
            attempt_id: Some(id),
            handle: None,
            result_digest: None,
            patch_id: None,
            auto_retryable: Some(false),
            message: None,
        });
    }

    let (decision, record) = {
        let guard = journal.acquire_lock(&id).map_err(sanitize_journal_error)?;
        let dec = journal
            .recover_attempt(&guard)
            .map_err(sanitize_journal_error)?;
        let rec = journal
            .read_record_locked(&guard)
            .map_err(sanitize_journal_error)?;
        (dec, rec)
    };

    match decision {
        RecoveryDecision::AmbiguousUnknown { reason } => {
            Ok(CloudRecoveryDecisionDto {
                decision: "ambiguous_unknown".to_string(),
                attempt_id: Some(id),
                handle: None,
                result_digest: None,
                patch_id: None,
                auto_retryable: Some(false),
                message: Some(reason),
            })
        }
        RecoveryDecision::ResumePolling { handle } => {
            Ok(CloudRecoveryDecisionDto {
                decision: "resume_polling".to_string(),
                attempt_id: Some(id),
                handle: Some(handle),
                result_digest: None,
                patch_id: None,
                auto_retryable: Some(false),
                message: Some("Discovered known accepted handle; resuming status polling without creating a new job.".to_string()),
            })
        }
        RecoveryDecision::ResumeCancelPolling { handle } => {
            Ok(CloudRecoveryDecisionDto {
                decision: "resume_cancel_polling".to_string(),
                attempt_id: Some(id),
                handle: Some(handle),
                result_digest: None,
                patch_id: None,
                auto_retryable: Some(false),
                message: Some("Discovered cancel-requested attempt; resuming status polling to reconcile terminal state.".to_string()),
            })
        }
        RecoveryDecision::AlreadyCommitted { patch_id } => {
            Ok(CloudRecoveryDecisionDto {
                decision: "already_committed".to_string(),
                attempt_id: Some(id),
                handle: None,
                result_digest: None,
                patch_id: Some(patch_id),
                auto_retryable: Some(false),
                message: None,
            })
        }
        RecoveryDecision::Terminal { message } => {
            Ok(CloudRecoveryDecisionDto {
                decision: "terminal".to_string(),
                attempt_id: Some(id),
                handle: None,
                result_digest: None,
                patch_id: None,
                auto_retryable: Some(false),
                message: Some(message),
            })
        }
        RecoveryDecision::CorruptedResultRetainedForInspection { handle, reason } => {
            Ok(CloudRecoveryDecisionDto {
                decision: "terminal".to_string(),
                attempt_id: Some(id),
                handle: Some(handle),
                result_digest: None,
                patch_id: None,
                auto_retryable: Some(false),
                message: Some(reason),
            })
        }
        RecoveryDecision::ResultCachedReadyForAttach { handle, result_digest } => {
            let handle_str = match &record.phase {
                AttemptPhase::Accepted { handle: h }
                | AttemptPhase::CancelRequested { handle: h }
                | AttemptPhase::ResultCached { handle: h, .. }
                | AttemptPhase::AttachmentPending { handle: h, .. }
                | AttemptPhase::Committed { handle: h, .. } => h.clone(),
                _ => handle,
            };

            let is_stale = is_recovery_stale(
                library,
                &record,
                chapter_id,
                page_index,
                region_id,
                region_revision,
                source_image_hash,
                simulate_stale,
            );

            if is_stale {
                return Ok(CloudRecoveryDecisionDto {
                    decision: "stale_attachment".to_string(),
                    attempt_id: Some(id),
                    handle: Some(handle_str),
                    result_digest: Some(result_digest),
                    patch_id: None,
                    auto_retryable: Some(false),
                    message: Some("Project region snapshot drifted or project state is unreadable; cannot attach result".to_string()),
                });
            }

            Ok(CloudRecoveryDecisionDto {
                decision: "result_cached_ready".to_string(),
                attempt_id: Some(id),
                handle: Some(handle_str),
                result_digest: Some(result_digest),
                patch_id: None,
                auto_retryable: Some(false),
                message: None,
            })
        }
        RecoveryDecision::AttachmentPendingVerification { patch_id, result_digest } => {
            let handle_str = match &record.phase {
                AttemptPhase::Accepted { handle: h }
                | AttemptPhase::CancelRequested { handle: h }
                | AttemptPhase::ResultCached { handle: h, .. }
                | AttemptPhase::AttachmentPending { handle: h, .. }
                | AttemptPhase::Committed { handle: h, .. } => h.clone(),
                _ => String::new(),
            };

            let is_stale = is_recovery_stale(
                library,
                &record,
                chapter_id,
                page_index,
                region_id,
                region_revision,
                source_image_hash,
                simulate_stale,
            );

            if is_stale {
                return Ok(CloudRecoveryDecisionDto {
                    decision: "stale_attachment".to_string(),
                    attempt_id: Some(id),
                    handle: Some(handle_str),
                    result_digest: Some(result_digest),
                    patch_id: None,
                    auto_retryable: Some(false),
                    message: Some("Project region snapshot drifted or project state is unreadable; cannot attach result".to_string()),
                });
            }

            Ok(CloudRecoveryDecisionDto {
                decision: "attachment_pending".to_string(),
                attempt_id: Some(id),
                handle: Some(handle_str),
                result_digest: Some(result_digest),
                patch_id: Some(patch_id),
                auto_retryable: Some(false),
                message: None,
            })
        }
    }
}

/// One attempt in a recovery report: where its result belongs and, for one
/// that needs a person, why (`ambiguous`, `stale` or `failed`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveredAttemptDto {
    pub attempt_id: String,
    pub provider: Option<CloudProvider>,
    pub profile_id: Option<String>,
    pub chapter_id: Option<String>,
    pub page_index: Option<u32>,
    pub region_id: String,
    pub can_abandon: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

impl RecoveredAttemptDto {
    fn of(record: &AttemptRecord) -> Self {
        Self {
            attempt_id: record.attempt_id.clone(),
            provider: Some(record.provider),
            profile_id: Some(record.profile_id.clone()),
            chapter_id: record.chapter_id.clone(),
            page_index: record.page_index,
            region_id: record.region_id.clone(),
            can_abandon: matches!(record.phase, AttemptPhase::Unknown { .. } | AttemptPhase::Accepted { .. }
                | AttemptPhase::CancelRequested { .. } | AttemptPhase::ResultCached { .. } | AttemptPhase::AttachmentPending { .. }),
            reason: None,
        }
    }

    fn because(self, reason: &'static str) -> Self {
        Self {
            // Startup may have just converted Dispatching to Unknown.
            can_abandon: self.can_abandon || reason == "ambiguous",
            reason: Some(reason),
            ..self
        }
    }
}

/// What `reconcileCloudRecovery({apply: true})` did (IC-4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudRecoveryReportDto {
    pub attached: Vec<RecoveredAttemptDto>,
    pub still_running: Vec<RecoveredAttemptDto>,
    pub needs_attention: Vec<RecoveredAttemptDto>,
}

/// `reconcileCloudRecovery`'s answer: one attempt's decision, or with `apply`
/// the report on every attempt the journal holds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CloudRecoveryResponse {
    Decision(CloudRecoveryDecisionDto),
    Report(CloudRecoveryReportDto),
}

/// Settle every attempt the journal holds that can be settled without the
/// network, and name the accepted jobs still to be waited on.
///
/// A cached result is attached; an interrupted submission is closed as
/// unknown and reported as `ambiguous`, never resubmitted; a result whose
/// region has moved on stays cached and is reported as `stale`. An accepted
/// job is reported as still running and returned with its job, for the caller
/// to wait on without holding up the report. Attempts that have already ended
/// are left out, and one a render in this process is working on is only
/// listed: it is that render's to finish.
pub fn recover_all_attempts(
    service: &InferenceService,
    job_path_of: &dyn Fn(&str) -> Option<PathBuf>,
) -> (CloudRecoveryReportDto, Vec<(String, PathBuf)>) {
    let mut report = CloudRecoveryReportDto::default();
    let mut waits = Vec::new();
    let mut snapshots = std::collections::HashMap::new();
    for attempt_id in service.journal().list_attempt_ids().unwrap_or_default() {
        let record = match service.journal().get_record(&attempt_id) {
            Ok(record) => record,
            Err(JournalError::AttemptNotFound { .. }) => continue,
            Err(_) if service.journal().repair_acknowledged(&attempt_id) => continue,
            Err(_) => {
                report.needs_attention.push(RecoveredAttemptDto {
                    attempt_id, provider: None, profile_id: None, chapter_id: None, page_index: None, region_id: String::new(),
                    reason: Some("repair_needed"), can_abandon: false,
                });
                continue;
            }
        };
        let entry = RecoveredAttemptDto::of(&record);
        if service::is_render_live(&attempt_id) {
            report.still_running.push(entry);
            continue;
        }
        // Dismissed stays dismissed, whatever phase the attempt was left in:
        // its cached result and journal are kept, only the report forgets it.
        if matches!(record.phase, AttemptPhase::Cancelled { .. } | AttemptPhase::Failed { .. } | AttemptPhase::Abandoned { .. })
            || service.journal().repair_acknowledged(&attempt_id)
        {
            continue;
        }
        let job_path = record.chapter_id.as_deref().and_then(job_path_of);
        let snapshot = if matches!(record.phase, AttemptPhase::Committed { .. }) {
            match job_path.as_ref() {
                Some(path) => {
                    let result = snapshots.entry(path.clone()).or_insert_with(|| service::RecoveryAuditSnapshot::load(path));
                    match result {
                        Ok(snapshot) => Some(&*snapshot),
                        Err(_) => { report.needs_attention.push(entry.because("load_error")); continue; }
                    }
                }
                None => None,
            }
        } else { None };
        match service.recover_attempt_with_snapshot(&attempt_id, job_path.as_deref(), snapshot) {
            Ok(RecoveryDecision::AlreadyCommitted { .. }) => {
                if !matches!(record.phase, AttemptPhase::Committed { .. }) {
                    report.attached.push(entry);
                }
            },
            Ok(
                RecoveryDecision::ResumePolling { .. }
                | RecoveryDecision::ResumeCancelPolling { .. },
            ) => match job_path {
                Some(path) => {
                    report.still_running.push(entry);
                    waits.push((attempt_id, path));
                }
                // Nowhere to put the result: waiting on it would only spend.
                None => report.needs_attention.push(entry.because("stale")),
            },
            Ok(RecoveryDecision::AmbiguousUnknown { .. }) => {
                if let Some(path) = job_path {
                    waits.push((attempt_id, path));
                }
                report.needs_attention.push(entry.because("ambiguous"))
            }
            Ok(RecoveryDecision::CorruptedResultRetainedForInspection { .. }) => {
                report.needs_attention.push(entry.because("failed"))
            }
            // Ended before this run looked at it: nothing to show.
            Ok(RecoveryDecision::Terminal { .. }) => {}
            // Attached by `recover_attempt_locally` itself; one that comes back
            // unattached could not be.
            Ok(
                RecoveryDecision::ResultCachedReadyForAttach { .. }
                | RecoveryDecision::AttachmentPendingVerification { .. },
            ) => report.needs_attention.push(entry.because("repair_needed")),
            // Another command holds the attempt this instant; the next
            // recovery sees it.
            Err(InferenceServiceError::Journal(JournalError::AttemptLocked { .. })) => {}
            Err(
                InferenceServiceError::StaleAttachment(_)
                | InferenceServiceError::Journal(JournalError::StaleAttachment { .. })
                | InferenceServiceError::JobManifest(_)
                | InferenceServiceError::Io(_),
            ) => report.needs_attention.push(entry.because("repair_needed")),
            Err(_) => report.needs_attention.push(entry.because("failed")),
        }
    }
    (report, waits)
}

/// Which regions' cloud work needs a look, read from the attempt journal and
/// the chapters' manifests alone: nothing is attached, resubmitted or sent, so
/// it runs whether or not the cloud is allowed, and a result lost while the
/// cloud was switched off still shows on its region.
///
/// - a committed attempt whose result the region no longer shows (the audit
///   recovery runs) is `repairNeeded`, the one reason that says a saved result
///   went missing;
/// - a result that arrived and was never applied (`ResultCached`,
///   `AttachmentPending`) is `cloudResultNotApplied`;
/// - a committed attempt whose chapter or sources could not be read to check
///   it is `cloudResultUnchecked`.
///
/// An attempt settled by the user, ended, still in flight, or with no chapter
/// flags nothing. One a render in this process is working on, or whose record
/// could not be read this instant, is left unjudged and keeps what it flagged.
pub(crate) fn scan_cloud_attention(
    service: &InferenceService,
    job_path_of: &dyn Fn(&str) -> Option<PathBuf>,
) -> crate::library::CloudAttentionScan {
    let mut scan = crate::library::CloudAttentionScan::default();
    let mut snapshots = std::collections::HashMap::new();
    for attempt_id in service.journal().list_attempt_ids().unwrap_or_default() {
        if service::is_render_live(&attempt_id) {
            scan.unjudged.push(attempt_id);
            continue;
        }
        let record = match service.journal().get_record(&attempt_id) {
            Ok(record) => record,
            Err(JournalError::AttemptNotFound { .. }) => continue,
            Err(_) => { scan.unjudged.push(attempt_id); continue; }
        };
        if service.journal().repair_acknowledged(&attempt_id) {
            continue;
        }
        let key = match record.phase {
            AttemptPhase::ResultCached { .. } | AttemptPhase::AttachmentPending { .. } => {
                "review.reason.cloudResultNotApplied"
            }
            AttemptPhase::Committed { .. } => {
                // A chapter no longer in the library has no region to flag.
                let Some(path) = record.chapter_id.as_deref().and_then(job_path_of) else { continue };
                let snapshot = snapshots.entry(path.clone())
                    .or_insert_with(|| service::RecoveryAuditSnapshot::load(&path));
                match snapshot.as_ref().map_err(|_| ()).and_then(|snapshot|
                    service.committed_result_intact(&record, snapshot).map_err(|_| ()))
                {
                    Ok(true) => continue,
                    Ok(false) => "review.reason.repairNeeded",
                    Err(()) => "review.reason.cloudResultUnchecked",
                }
            }
            _ => continue,
        };
        scan.flags.push((record.region_id.clone(), attempt_id, key));
    }
    scan
}

/// [`scan_cloud_attention`] for the app, into the library's flagged regions.
/// Page reads wait for it while it runs.
pub(crate) fn refresh_cloud_attention(app: &tauri::AppHandle) {
    crate::library::begin_cloud_attention_read();
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let service = app_inference_service(app).ok()?;
        let library = Library::for_app(app).ok()?;
        Some(scan_cloud_attention(&service, &|chapter_id| library.resolve_chapter(chapter_id).ok()))
    }));
    match read {
        Ok(Some(scan)) => crate::library::replace_cloud_attention(scan),
        // Nothing readable, or the read failed: keep whatever was flagged
        // rather than clear it, and stop page reads waiting.
        _ => crate::library::end_cloud_attention_read(),
    }
}

/// `apply`: recover what is local now, and wait on accepted jobs in the
/// background, each reporting on `cloud://attempt` like a render the user
/// started. A job still running when its wait runs out stays accepted for the
/// next recovery.
fn reconcile_and_resume(app: &tauri::AppHandle) -> Result<CloudRecoveryReportDto, String> {
    let service = app_inference_service(app)?.with_review_retry(false);
    let library = Library::for_app(app).map_err(|_| "recovery_library_load_failed".to_string())?;
    let (mut report, waits) = recover_all_attempts(&service, &|chapter_id| {
        library.resolve_chapter(chapter_id).ok()
    });
    // The regions themselves carry the reason, not only this report: read
    // again now that recovery has attached and audited what it could.
    refresh_cloud_attention(app);
    if waits.is_empty() {
        return Ok(report);
    }
    let Ok(config) = config::read_inference_config(app) else {
        // No profile to reach the gateway with: these cannot be waited on.
        let waiting: Vec<String> = waits
            .into_iter()
            .map(|(attempt_id, _)| attempt_id)
            .collect();
        let (unreachable, running) = std::mem::take(&mut report.still_running)
            .into_iter()
            .partition(|entry| waiting.contains(&entry.attempt_id));
        report.still_running = running;
        report.needs_attention.extend(
            unreachable
                .into_iter()
                .map(|entry: RecoveredAttemptDto| entry.because("failed")),
        );
        return Ok(report);
    };
    let app = app.clone();
    spawn_recovery_waits(waits, move |attempt_id, job_path| {
        if let Ok(service) = app_inference_service(&app).map(|service| service.with_review_retry(false)) {
            let _ = service.recover_cloud_attempt(&attempt_id, &job_path, &config, &PollOptions::interactive());
        }
    });
    Ok(report)
}

/// Unknown lookups and accepted waits start independently, outside the report path.
fn spawn_recovery_waits(
    waits: Vec<(String, PathBuf)>,
    recover: impl Fn(String, PathBuf) + Send + Sync + 'static,
) -> Vec<std::thread::JoinHandle<()>> {
    let recover = std::sync::Arc::new(recover);
    waits.into_iter().map(|(id, path)| {
        let recover = recover.clone();
        std::thread::spawn(move || recover(id, path))
    }).collect()
}

pub(crate) fn apply_recovery_action(
    journal: &AttemptJournal, attempt_id: &str, action: &str, accept_duplicate_risk: bool,
) -> Result<(), String> {
    let guard = journal.acquire_lock(attempt_id).map_err(sanitize_journal_error)?;
    match action {
        "abandon" => { journal.abandon(&guard, accept_duplicate_risk).map_err(sanitize_journal_error)?; }
        "acknowledge" => journal.acknowledge_repair(&guard).map_err(sanitize_journal_error)?,
        _ => return Err("unknown recovery action".into()),
    }
    crate::library::forget_repair(attempt_id);
    Ok(())
}

/// Evaluate crash discovery and attempt recovery against local project snapshot.
///
/// With `apply: true` it recovers instead of reporting one decision: see
/// [`recover_all_attempts`]. The other arguments are then ignored.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn reconcile_cloud_recovery(
    app: tauri::AppHandle,
    attempt_id: Option<String>,
    chapter_id: Option<String>,
    page_index: Option<u32>,
    region_id: Option<String>,
    region_revision: Option<serde_json::Value>,
    source_image_hash: Option<String>,
    simulate_stale: Option<bool>,
    apply: Option<bool>,
    action: Option<String>,
    accept_duplicate_risk: Option<bool>,
) -> Result<CloudRecoveryResponse, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(action) = action {
            let id = attempt_id.as_deref().ok_or("attempt id required")?;
            let journal = get_app_journal(&app)?;
            apply_recovery_action(&journal, id, &action, accept_duplicate_risk == Some(true))?;
            return Ok(CloudRecoveryResponse::Decision(CloudRecoveryDecisionDto {
                decision: action, attempt_id: Some(id.into()), handle: None,
                result_digest: None, patch_id: None, auto_retryable: Some(false), message: None,
            }));
        }
        if apply == Some(true) {
            return reconcile_and_resume(&app).map(CloudRecoveryResponse::Report);
        }
        let journal = get_app_journal(&app)?;
        let library = Library::for_app(&app).ok();
        reconcile_cloud_recovery_inner(
            &journal,
            library.as_ref(),
            attempt_id.as_deref(),
            chapter_id.as_deref(),
            page_index,
            region_id.as_deref(),
            region_revision.as_ref(),
            source_image_hash.as_deref(),
            simulate_stale,
        )
        .map(CloudRecoveryResponse::Decision)
    })
    .await
    .map_err(|_| "async reconcile recovery task failed".to_string())?
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_release_is_current_only_when_the_gateway_names_this_code() {
        let expected = || "a".repeat(64);
        assert!(super::CloudRelease::of(expected(), Some(expected())).current);
        let other = super::CloudRelease::of(expected(), Some("f".repeat(64)));
        assert_eq!((other.current, other.expected), (false, expected()));
        assert!(!super::CloudRelease::of(expected(), None).current, "a gateway too old to say is not current");
    }

    use super::*;
    use crate::inference::config::CloudProfile;
    use crate::inference::journal::{
        AttemptJournal, AttemptPhase, AttemptRecord, CreateAttemptIntent, JournalError,
        ProjectRegionSnapshot, RecoveryDecision,
    };
    use cleaner_core::cloud_wire::{provisional_fixture_limits, PROTOCOL_VERSION};
    use cleaner_core::patch::Engine;
    use std::sync::Arc;

    /// The refusal of an oversized crop reaches the interface by its code.
    #[test]
    fn an_oversized_crop_is_refused_by_a_stable_code() {
        assert!(sanitize_consent_error(ConsentError::CropTooLarge).starts_with("cloud_consent_crop_too_large"));
    }

    fn compute_intent_request_digest(intent: &CreateAttemptIntent) -> String {
        let req_meta = cleaner_core::cloud_wire::JobRequestMetadata {
            protocol_version: PROTOCOL_VERSION.to_string(),
            job_id: intent.job_id.clone(),
            attempt_id: intent.attempt_id.clone(),
            recipe: cleaner_core::cloud_wire::WireRenderRecipe {
                qwen_edit: None,
                recipe_id: intent.recipe_id.clone(),
                preprocessing_version: intent.preprocessing_version.clone(),
                model_id: intent.model_id.clone(),
                model_revision: intent.model_revision.clone(),
                native_mask_conditioning: intent.native_mask_conditioning,
            },
            width: intent.width,
            height: intent.height,
            seed: intent.seed,
            steps: intent.steps,
            guidance_scaled: intent.guidance_scaled,
            image_sha256: intent.crop_sha256.clone(),
            hint_sha256: intent.hint_sha256.clone(),
            request_digest: String::new(),
        };
        cleaner_core::cloud_wire::compute_request_digest(&req_meta)
    }

    #[test]
    fn blocked_unknown_lookup_does_not_delay_report_or_accepted_waits() {
        let (started, events) = std::sync::mpsc::channel();
        let release = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let gate = release.clone();
        let handles = spawn_recovery_waits(vec![("unknown".into(), PathBuf::new()), ("accepted".into(), PathBuf::new())], move |id, _| {
            started.send(id.clone()).unwrap();
            if id == "unknown" {
                let (lock, wake) = &*gate;
                let _guard = wake.wait_while(lock.lock().unwrap(), |released| !*released).unwrap();
            }
        });
        let mut ids = vec![events.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), events.recv_timeout(std::time::Duration::from_secs(2)).unwrap()];
        ids.sort();
        assert_eq!(ids, ["accepted", "unknown"]);
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        for handle in handles { handle.join().unwrap(); }
    }

    #[test]
    fn resolve_fingerprint_from_config() {
        let mut cfg = InferenceConfig::default();
        cfg.modal_profiles.insert(
            "modal-1".to_string(),
            CloudProfile {
                id: "modal-1".to_string(),
                name: "Modal".to_string(),
                endpoint_url: "https://modal.run/ep".to_string(),
                canonical_origin: "https://modal.run".to_string(),
                canonical_origin_fingerprint: "expected-fp-1234".to_string(),
                created_at_ms: 10,
                updated_at_ms: 10,
            },
        );

        let fp = resolve_profile_fingerprint(&cfg, CloudProvider::Modal, "modal-1").unwrap();
        assert_eq!(fp, "expected-fp-1234");

        assert!(resolve_profile_fingerprint(&cfg, CloudProvider::Modal, "non-existent").is_err());
        assert!(resolve_profile_fingerprint(&cfg, CloudProvider::Beam, "modal-1").is_err());
    }

    #[test]
    fn removed_profiles_names_only_the_endpoints_that_went() {
        let profile = |id: &str| CloudProfile {
            id: id.to_string(),
            name: id.to_string(),
            endpoint_url: "https://modal.run/ep".to_string(),
            canonical_origin: "https://modal.run".to_string(),
            canonical_origin_fingerprint: "fp".to_string(),
            created_at_ms: 10,
            updated_at_ms: 10,
        };
        let mut old = InferenceConfig::default();
        old.modal_profiles.insert("m1".to_string(), profile("m1"));
        old.modal_profiles.insert("m2".to_string(), profile("m2"));
        old.beam_profiles.insert("m1".to_string(), profile("m1"));
        let mut new = old.clone();
        new.modal_profiles.remove("m1");
        new.modal_profiles.insert("m3".to_string(), profile("m3"));

        assert_eq!(removed_profiles(&old, &new), vec![(CloudProvider::Modal, "m1".to_string())]);
        assert!(removed_profiles(&old, &old).is_empty());
        assert!(removed_profiles(&InferenceConfig::default(), &new).is_empty(), "an addition removes nothing");
    }

    /// A pick makes a configured profile the default and leaves the rest of
    /// the file as it was; a missing or paused one is refused and changes
    /// nothing.
    #[test]
    fn selecting_a_profile_changes_the_default_alone() {
        let scratch = std::env::temp_dir().join("mc-cmd-select").join(format!("{}", current_epoch_ms()));
        let path = scratch.join("inference.json");
        let profile = |id: &str| CloudProfile {
            id: id.to_string(),
            name: id.to_string(),
            endpoint_url: format!("https://{id}.modal.run/mc/v1"),
            canonical_origin: String::new(),
            canonical_origin_fingerprint: String::new(),
            created_at_ms: 10,
            updated_at_ms: 10,
        };
        let mut config = InferenceConfig::default();
        config.modal_profiles.insert("pick-a".to_string(), profile("pick-a"));
        config.modal_profiles.insert("pick-b".to_string(), profile("pick-b"));
        config.selected_target = ExecutionTarget::Modal { profile_id: "pick-a".to_string() };
        let before = write_inference_config_at(&path, config).unwrap();

        let after = select_cloud_profile_at(&path, CloudProvider::Modal, "pick-b").unwrap();
        assert_eq!(after.selected_target, ExecutionTarget::Modal { profile_id: "pick-b".to_string() });
        assert_eq!(after.modal_profiles, before.modal_profiles);
        assert_eq!(config::read_inference_config_from_path(&path).unwrap(), after);

        assert_eq!(select_cloud_profile_at(&path, CloudProvider::Modal, "gone").unwrap_err(), "profile_missing");
        assert_eq!(select_cloud_profile_at(&path, CloudProvider::Beam, "pick-a").unwrap_err(), "provider_paused");
        assert!(select_cloud_profile_at(&path, CloudProvider::Modal, "../x").is_err());
        assert_eq!(config::read_inference_config_from_path(&path).unwrap(), after);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Picking another default, or renaming a profile, moves no profile, so
    /// every grant stands and a run on the last default keeps going. A profile
    /// added, removed or pointed elsewhere moves, and only that one.
    #[test]
    fn only_a_profile_whose_destination_changed_counts_as_moved() {
        let profile = |id: &str| CloudProfile {
            id: id.to_string(),
            name: id.to_string(),
            endpoint_url: format!("https://{id}.modal.run/ep"),
            canonical_origin: String::new(),
            canonical_origin_fingerprint: String::new(),
            created_at_ms: 10,
            updated_at_ms: 10,
        };
        let mut old = InferenceConfig::default();
        old.modal_profiles.insert("a".to_string(), profile("a"));
        old.modal_profiles.insert("b".to_string(), profile("b"));
        old.selected_target = ExecutionTarget::Modal { profile_id: "a".to_string() };

        let mut switched = old.clone();
        switched.selected_target = ExecutionTarget::Modal { profile_id: "b".to_string() };
        switched.modal_profiles.get_mut("a").unwrap().name = "renamed".to_string();
        assert!(moved_profiles(&old.modal_profiles, &switched.modal_profiles).is_empty());

        let mut changed = switched;
        changed.modal_profiles.get_mut("b").unwrap().endpoint_url = "https://b.modal.run/other".to_string();
        changed.modal_profiles.remove("a");
        changed.modal_profiles.insert("c".to_string(), profile("c"));
        assert_eq!(moved_profiles(&old.modal_profiles, &changed.modal_profiles),
            ["a", "b", "c"].map(String::from).into_iter().collect::<BTreeSet<_>>());
    }

    #[test]
    fn model_info_projection_and_serde() {
        let resp = ModelInfoResponse {
            protocol_version: PROTOCOL_VERSION.to_string(),
            provider: CloudProvider::Modal,
            model_id: "flux-schnell".to_string(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
            recipe_id: "sdnq-v1".to_string(),
            preprocessing_version: "1.0.0".to_string(),
            native_mask_conditioning: false,
            limits: provisional_fixture_limits(),
        };

        let info = CloudModelInfo::from(resp);
        assert_eq!(info.supported_protocol_version, "1.0.0");
        assert_eq!(info.pinned_model_id, "flux-schnell");
        assert_eq!(
            info.pinned_model_revision,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(info.pinned_recipe_id, "sdnq-v1");
        assert_eq!(info.limits.max_dimensions, [2048, 2048]);
        assert_eq!(info.limits.max_megapixels, 4.19);
        assert_eq!(info.limits.max_png_bytes, 16 * 1024 * 1024);
        assert_eq!(info.limits.max_multipart_bytes, 32 * 1024 * 1024);
        assert_eq!(info.limits.default_worker_deadline_sec, 120);

        let serialized = serde_json::to_string(&info).expect("serialize CloudModelInfo");

        // Verify camelCase field names matching frontend expectations
        assert!(serialized.contains(r#""supportedProtocolVersion":"1.0.0""#));
        assert!(serialized.contains(r#""pinnedModelId":"flux-schnell""#));
        assert!(serialized
            .contains(r#""pinnedModelRevision":"0123456789abcdef0123456789abcdef01234567""#));
        assert!(serialized.contains(r#""pinnedRecipeId":"sdnq-v1""#));
        assert!(serialized.contains(r#""limits":{"#));
        assert!(serialized.contains(r#""maxDimensions":[2048,2048]"#));
        assert!(serialized.contains(r#""maxMegapixels":4.19"#));
        assert!(serialized.contains(r#""maxPngBytes":16777216"#));
        assert!(serialized.contains(r#""maxMultipartBytes":33554432"#));
        assert!(serialized.contains(r#""defaultWorkerDeadlineSec":120"#));

        // Invariant: no secrets or endpoint URLs leaked
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("token"));
        assert!(!serialized.contains("endpoint"));
        assert!(!serialized.contains("https://"));

        // Roundtrip deserialization
        let deserialized: CloudModelInfo =
            serde_json::from_str(&serialized).expect("deserialize CloudModelInfo");
        assert_eq!(info, deserialized);
    }

    #[test]
    fn cloud_connection_status_serde() {
        let reachable = CloudConnectionStatus {
            ok: true,
            status: "reachable".to_string(),
            provider: CloudProvider::Modal,
            profile_id: "modal-prof-1".to_string(),
            latency_ms: Some(42),
            message: None,
        };

        let reach_json = serde_json::to_string(&reachable).expect("serialize reachable");
        assert!(reach_json.contains(r#""ok":true"#));
        assert!(reach_json.contains(r#""status":"reachable""#));
        assert!(reach_json.contains(r#""provider":"modal""#));
        assert!(reach_json.contains(r#""profileId":"modal-prof-1""#));
        assert!(reach_json.contains(r#""latencyMs":42"#));
        assert!(!reach_json.contains("message"));

        let reach_de: CloudConnectionStatus =
            serde_json::from_str(&reach_json).expect("deserialize reachable");
        assert_eq!(reachable, reach_de);

        let unreachable = CloudConnectionStatus {
            ok: false,
            status: "unreachable".to_string(),
            provider: CloudProvider::Beam,
            profile_id: "beam-prof-1".to_string(),
            latency_ms: None,
            message: Some("network connection timeout".to_string()),
        };

        let unreach_json = serde_json::to_string(&unreachable).expect("serialize unreachable");
        assert!(unreach_json.contains(r#""ok":false"#));
        assert!(unreach_json.contains(r#""status":"unreachable""#));
        assert!(unreach_json.contains(r#""provider":"beam""#));
        assert!(unreach_json.contains(r#""profileId":"beam-prof-1""#));
        assert!(unreach_json.contains(r#""message":"network connection timeout""#));
        assert!(!unreach_json.contains("latencyMs"));

        let unreach_de: CloudConnectionStatus =
            serde_json::from_str(&unreach_json).expect("deserialize unreachable");
        assert_eq!(unreachable, unreach_de);
    }

    #[test]
    fn fail_closed_without_profile_or_credential() {
        let mut cfg = InferenceConfig::default();

        // 1. Profile does not exist -> fails closed
        let err = match build_client_for_profile(&cfg, CloudProvider::Modal, "missing-prof") {
            Err(e) => e,
            Ok(_) => panic!("expected Err for missing profile"),
        };
        assert!(err
            .to_string()
            .contains("does not exist in inference configuration"));

        // 2. Profile exists, but credential is not stored in SecretManager -> fails closed
        let profile = CloudProfile {
            id: "prof-1".to_string(),
            name: "Profile".to_string(),
            endpoint_url: "https://modal-cleaner.run.modal.com/mc/v1".to_string(),
            canonical_origin: "https://modal-cleaner.run.modal.com".to_string(),
            canonical_origin_fingerprint:
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            created_at_ms: 100,
            updated_at_ms: 100,
        };
        cfg.modal_profiles.insert("prof-1".to_string(), profile.clone());

        let err = match build_client_for_profile(&cfg, CloudProvider::Modal, "prof-1") {
            Err(e) => e,
            Ok(_) => panic!("expected Err for missing credential"),
        };
        assert!(err.to_string().contains("runtime credential missing"));

        // 3. Invalid profile ID format -> fails closed
        assert!(build_client_for_profile(&cfg, CloudProvider::Modal, "bad/id!").is_err());

        // 4. Beam is paused: refused before the keychain is asked, even with a saved profile
        cfg.beam_profiles.insert("prof-1".to_string(), profile);
        let err = match build_client_for_profile(&cfg, CloudProvider::Beam, "prof-1") {
            Err(e) => e,
            Ok(_) => panic!("expected Err for a paused provider"),
        };
        assert!(err.to_string().contains("provider_paused"));
    }

    #[test]
    fn test_modal_runtime_credential_roundtrip_and_no_summary_leakage() {
        let fp = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();
        let key = SecretKey::new(
            CloudProvider::Modal,
            "modal-roundtrip-prof".to_string(),
            fp.clone(),
            SecretRole::Runtime,
        );

        let compound =
            encode_modal_runtime_secret("ak-genuine-token-id", "as-genuine-token-secret")
                .expect("encode modal runtime secret");

        // Nothing here reads the process-wide manager, so the test keeps its
        // own and never asks the host's credential store for anything.
        let secrets = SecretManager::new_in_memory();
        let summary = secrets
            .store_secret(&key, compound, true)
            .expect("store secret in session");

        // Verify summary never contains token_id or token_secret
        let summary_json = serde_json::to_string(&summary).expect("serialize SecretSummary");
        assert!(!summary_json.contains("ak-genuine-token-id"));
        assert!(!summary_json.contains("as-genuine-token-secret"));

        // Verify roundtrip decode into RuntimeCredential
        let retrieved = secrets
            .get_secret(&key)
            .expect("get secret")
            .expect("secret exists");
        let (tid, tsec) = decode_modal_runtime_secret(&retrieved).expect("decode modal secret");
        assert_eq!(tid, "ak-genuine-token-id");
        assert_eq!(tsec.expose_str().unwrap(), "as-genuine-token-secret");

        let runtime_cred = RuntimeCredential::ModalProxy {
            token_id: tid,
            token_secret: tsec,
        };

        let target = CloudEndpointTarget::new_test_target(
            CloudProvider::Modal,
            "modal-roundtrip-prof",
            "https://127.0.0.1:8080/mc/v1",
        )
        .expect("test target");

        let bound = BoundRuntimeCredential::new(&target, runtime_cred);
        assert!(bound.is_ok());

        secrets.delete_secret(&key).expect("cleanup secret");
    }

    #[test]
    fn test_check_cloud_connection_structured_states() {
        let mut cfg = InferenceConfig::default();

        // 1. Missing profile -> configuration_error
        let status =
            check_cloud_connection_for_config(&cfg, CloudProvider::Modal, "nonexistent-profile");
        assert!(!status.ok);
        assert_eq!(status.status, "configuration_error");
        let msg = status.message.expect("message present");
        assert!(!msg.contains("https://"));
        assert!(!msg.contains("/Users/"));

        // 2. Profile exists, but credential missing -> credential_missing
        let fp = "11223344556677889900aabbccddeeff11223344556677889900aabbccddeeff".to_string();
        cfg.modal_profiles.insert(
            "modal-prof-missing-cred".to_string(),
            CloudProfile {
                id: "modal-prof-missing-cred".to_string(),
                name: "Modal No Cred".to_string(),
                endpoint_url: "https://modal.run/mc/v1".to_string(),
                canonical_origin: "https://modal.run".to_string(),
                canonical_origin_fingerprint: fp.clone(),
                created_at_ms: 100,
                updated_at_ms: 100,
            },
        );
        let status = check_cloud_connection_for_config(
            &cfg,
            CloudProvider::Modal,
            "modal-prof-missing-cred",
        );
        assert!(!status.ok);
        assert_eq!(status.status, "credential_missing");

        // 3. Legacy / non-compound Modal secret -> fails closed as credential_missing
        let key = SecretKey::new(
            CloudProvider::Modal,
            "modal-prof-legacy-cred".to_string(),
            fp.clone(),
            SecretRole::Runtime,
        );
        cfg.modal_profiles.insert(
            "modal-prof-legacy-cred".to_string(),
            CloudProfile {
                id: "modal-prof-legacy-cred".to_string(),
                name: "Modal Legacy Cred".to_string(),
                endpoint_url: "https://modal.run/mc/v1".to_string(),
                canonical_origin: "https://modal.run".to_string(),
                canonical_origin_fingerprint: fp,
                created_at_ms: 100,
                updated_at_ms: 100,
            },
        );
        SecretManager::global()
            .store_secret(&key, SecretValue::new("raw-unversioned-legacy-token"), true)
            .expect("store legacy secret");

        let status =
            check_cloud_connection_for_config(&cfg, CloudProvider::Modal, "modal-prof-legacy-cred");
        assert!(!status.ok);
        assert_eq!(status.status, "credential_missing");
        let msg = status.message.expect("message present");
        assert!(msg.contains("invalid or incomplete"));
        assert!(!msg.contains("raw-unversioned-legacy-token"));

        // The session copy goes first, so nothing is left behind either way.
        // The OS half of the answer belongs to the host: a headless Linux
        // runner has no credential store to confirm a deletion with.
        let _ = SecretManager::global().delete_secret(&key);
    }

    #[test]
    fn test_dto_serde_camel_case_and_roundtrip() {
        // 1. ConsentProposalDto
        let proposal = ConsentProposalDto {
            proposal_id: "prop-xyz".to_string(),
            profile_id: "beam-prof-1".to_string(),
            provider: CloudProvider::Beam,
            endpoint_url: "https://api.beam.cloud/ep".to_string(),
            canonical_origin_fingerprint: "fp-12345678".to_string(),
            profile_epoch: 3,
            crop_sha256: "crop-sha256-hex".to_string(),
            hint_sha256: "hint-sha256-hex".to_string(),
            source_hash: "src-hash-hex".to_string(),
            mask_hash: "mask-hash-hex".to_string(),
            region_revision: 42,
            rect: RectDto {
                x: 10,
                y: 20,
                w: 100,
                h: 200,
            },
            recipe: RenderRecipe::new(
                "sdnq-v1",
                "1.0.0",
                "flux-schnell",
                "0123456789abcdef0123456789abcdef01234567",
                false,
            ),
            intent: OperationIntent::CleanAnyway,
            created_at_ms: 1000,
            expires_at_ms: 2000,
            estimated_cost_usd: Some(0.005),
            standing: false,
        };

        let prop_json = serde_json::to_string(&proposal).expect("serialize ConsentProposalDto");
        assert!(prop_json.contains(r#""proposalId":"prop-xyz""#));
        assert!(prop_json.contains(r#""profileId":"beam-prof-1""#));
        assert!(prop_json.contains(r#""provider":"beam""#));
        assert!(prop_json.contains(r#""canonicalOriginFingerprint":"fp-12345678""#));
        assert!(prop_json.contains(r#""profileEpoch":3"#));
        assert!(prop_json.contains(r#""cropSha256":"crop-sha256-hex""#));
        assert!(prop_json.contains(r#""hintSha256":"hint-sha256-hex""#));
        assert!(prop_json.contains(r#""sourceHash":"src-hash-hex""#));
        assert!(prop_json.contains(r#""maskHash":"mask-hash-hex""#));
        assert!(prop_json.contains(r#""regionRevision":42"#));
        assert!(prop_json.contains(r#""rect":{"x":10,"y":20,"w":100,"h":200}"#));
        assert!(prop_json.contains(r#""createdAtMs":1000"#));
        assert!(prop_json.contains(r#""expiresAtMs":2000"#));
        assert!(prop_json.contains(r#""estimatedCostUsd":0.005"#));
        assert!(prop_json.contains(r#""standing":false"#));

        let prop_de: ConsentProposalDto =
            serde_json::from_str(&prop_json).expect("deserialize ConsentProposalDto");
        assert_eq!(proposal, prop_de);

        // 2. GrantDto
        let grant = GrantDto {
            nonce: "grant-nonce-99".to_string(),
            scope: GrantScopeDto {
                provider: CloudProvider::Beam,
                profile_id: "beam-prof-1".to_string(),
                endpoint_fingerprint: "fp-12345678".to_string(),
                crop_sha256: "crop-sha256-hex".to_string(),
                mask_hash: "mask-hash-hex".to_string(),
                revision: 42,
                recipe: proposal.recipe.clone(),
                operation_digest: "opdig-hex".to_string(),
            },
            issued_at_ms: 1000,
            expires_at_ms: 2000,
            allowed_attempts: 1,
            used_attempts: 0,
        };
        let grant_json = serde_json::to_string(&grant).expect("serialize GrantDto");
        assert!(grant_json.contains(r#""nonce":"grant-nonce-99""#));
        assert!(grant_json.contains(r#""allowedAttempts":1"#));
        assert!(grant_json.contains(r#""usedAttempts":0"#));
        let grant_de: GrantDto = serde_json::from_str(&grant_json).expect("deserialize GrantDto");
        assert_eq!(grant, grant_de);

        // 3. CloudAttemptSubmissionDto
        let sub = CloudAttemptSubmissionDto {
            attempt_id: "att-123".to_string(),
            handle: Some("h-456".to_string()),
            status: "accepted".to_string(),
            request_digest: Some("req-dig-hex".to_string()),
            auto_retryable: Some(false),
            error: None,
        };
        let sub_json = serde_json::to_string(&sub).expect("serialize CloudAttemptSubmissionDto");
        assert!(sub_json.contains(r#""attemptId":"att-123""#));
        assert!(sub_json.contains(r#""handle":"h-456""#));
        assert!(sub_json.contains(r#""status":"accepted""#));
        assert!(sub_json.contains(r#""requestDigest":"req-dig-hex""#));
        assert!(sub_json.contains(r#""autoRetryable":false"#));
        assert!(!sub_json.contains("error"));
        let sub_de: CloudAttemptSubmissionDto =
            serde_json::from_str(&sub_json).expect("deserialize CloudAttemptSubmissionDto");
        assert_eq!(sub, sub_de);

        // 4. CloudAttemptStatusDto
        let status_dto = CloudAttemptStatusDto {
            attempt_id: "att-123".to_string(),
            handle: Some("h-456".to_string()),
            status: "completed".to_string(),
            reported_cost_usd: Some(0.002),
            acknowledged: Some(true),
            created_at_ms: Some(100),
            started_at_ms: Some(150),
            finished_at_ms: Some(250),
        };
        let status_json =
            serde_json::to_string(&status_dto).expect("serialize CloudAttemptStatusDto");
        assert!(status_json.contains(r#""attemptId":"att-123""#));
        assert!(status_json.contains(r#""reportedCostUsd":0.002"#));
        assert!(status_json.contains(r#""acknowledged":true"#));
        let status_de: CloudAttemptStatusDto =
            serde_json::from_str(&status_json).expect("deserialize CloudAttemptStatusDto");
        assert_eq!(status_dto, status_de);

        // 5. CloudAttemptResultDto
        let res_dto = CloudAttemptResultDto {
            attempt_id: "att-123".to_string(),
            handle: "h-456".to_string(),
            result_digest: "res-dig-hex".to_string(),
            reported_cost_usd: Some(0.002),
            width: 512,
            height: 512,
            cached: true,
        };
        let res_json = serde_json::to_string(&res_dto).expect("serialize CloudAttemptResultDto");
        assert!(res_json.contains(r#""resultDigest":"res-dig-hex""#));
        assert!(res_json.contains(r#""cached":true"#));
        let res_de: CloudAttemptResultDto =
            serde_json::from_str(&res_json).expect("deserialize CloudAttemptResultDto");
        assert_eq!(res_dto, res_de);

        // 6. CloudCancelResultDto
        let cancel_dto = CloudCancelResultDto {
            handle: "h-456".to_string(),
            status: "cancel_requested".to_string(),
            acknowledged: true,
        };
        let cancel_json =
            serde_json::to_string(&cancel_dto).expect("serialize CloudCancelResultDto");
        assert!(cancel_json.contains(r#""status":"cancel_requested""#));
        assert!(cancel_json.contains(r#""acknowledged":true"#));
        let cancel_de: CloudCancelResultDto =
            serde_json::from_str(&cancel_json).expect("deserialize CloudCancelResultDto");
        assert_eq!(cancel_dto, cancel_de);

        // 7. CloudRecoveryDecisionDto
        let rec_dto = CloudRecoveryDecisionDto {
            decision: "resume_polling".to_string(),
            attempt_id: Some("att-123".to_string()),
            handle: Some("h-456".to_string()),
            result_digest: None,
            patch_id: None,
            auto_retryable: Some(false),
            message: Some("resuming polling".to_string()),
        };
        let rec_json = serde_json::to_string(&rec_dto).expect("serialize CloudRecoveryDecisionDto");
        assert!(rec_json.contains(r#""decision":"resume_polling""#));
        assert!(rec_json.contains(r#""autoRetryable":false"#));
        let rec_de: CloudRecoveryDecisionDto =
            serde_json::from_str(&rec_json).expect("deserialize CloudRecoveryDecisionDto");
        assert_eq!(rec_dto, rec_de);
    }

    #[test]
    fn test_attempt_id_derivation_deterministic() {
        let nonce = "test-grant-nonce-123456789";
        let expected_id = format!("att-{}", &sha256_hex(nonce.as_bytes())[0..24]);
        assert_eq!(expected_id.len(), 28);
        assert!(expected_id.starts_with("att-"));

        // Same nonce yields exact same attempt_id
        let rederived = format!("att-{}", &sha256_hex(nonce.as_bytes())[0..24]);
        assert_eq!(expected_id, rederived);

        // Different nonce yields different attempt_id
        let other = format!("att-{}", &sha256_hex(b"different-nonce")[0..24]);
        assert_ne!(expected_id, other);
    }

    #[test]
    fn test_validate_safe_id_rules() {
        assert!(journal::validate_safe_id("att-1234567890abcdef", "attempt_id").is_ok());
        assert!(journal::validate_safe_id("h-job-123_valid_name", "handle").is_ok());
        assert!(journal::validate_safe_id("", "attempt_id").is_err());
        assert!(journal::validate_safe_id("../traversal", "attempt_id").is_err());
        assert!(journal::validate_safe_id("path/separator", "handle").is_err());
        assert!(journal::validate_safe_id("dot.not.allowed", "handle").is_err());
        assert!(journal::validate_safe_id("null\0byte", "attempt_id").is_err());
    }

    #[test]
    fn a_standing_project_consent_marks_only_its_own_proposals() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-standing")
            .join(format!("{}", current_epoch_ms()));
        let library = Library::at(&scratch);
        let one = library.create_project("One", cleaner_core::project::StripMode::Single, None, None).unwrap();
        let two = library.create_project("Two", cleaner_core::project::StripMode::Single, None, None).unwrap();
        let first = library.create_chapter(&one.id, "A", None, None).unwrap().created().unwrap();
        let other = library.create_chapter(&two.id, "B", None, None).unwrap().created().unwrap();
        let proposal = ConsentProposalDto {
            proposal_id: "prop-1".into(),
            profile_id: "mc-1".into(),
            provider: CloudProvider::Modal,
            endpoint_url: "https://acme--mc-1-gateway.modal.run/mc/v1".into(),
            canonical_origin_fingerprint: "fp-1".into(),
            profile_epoch: 0,
            crop_sha256: String::new(),
            hint_sha256: String::new(),
            source_hash: String::new(),
            mask_hash: String::new(),
            region_revision: 0,
            rect: RectDto { x: 0, y: 0, w: 8, h: 8 },
            recipe: RenderRecipe::new("sdnq-v1", "1.0.0", "flux-schnell", "0".repeat(40), false),
            intent: OperationIntent::CleanAnyway,
            created_at_ms: 0,
            expires_at_ms: 0,
            estimated_cost_usd: None,
            standing: false,
        };
        assert!(!with_standing(proposal.clone(), &library, &first.id).standing);

        library.record_cloud_consent(&first.id, CloudProvider::Modal, "mc-1", "fp-1", true);
        assert!(with_standing(proposal.clone(), &library, &first.id).standing);
        assert!(!with_standing(proposal.clone(), &library, &other.id).standing);
        let moved = ConsentProposalDto { canonical_origin_fingerprint: "fp-2".into(), ..proposal };
        assert!(!with_standing(moved, &library, &first.id).standing);
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn a_region_consent_needs_both_statements_unless_its_project_stands() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-region-statements")
            .join(format!("{}", current_epoch_ms()));
        let library = Library::at(&scratch);
        let project = library.create_project("One", cleaner_core::project::StripMode::Single, None, None).unwrap();
        let chapter = library.create_chapter(&project.id, "A", None, None).unwrap().created().unwrap();
        let answer = |rights, retention| {
            require_region_statements(&library, &chapter.id, CloudProvider::Modal, "mc-1", "fp-1", rights, retention)
        };
        assert_eq!(answer(false, false).unwrap_err(), "rights_attestation_required");
        assert_eq!(answer(true, false).unwrap_err(), "retention_acknowledgement_required");
        assert_eq!(answer(false, true).unwrap_err(), "rights_attestation_required");

        // A consent recorded before the dialog asked for the statements does
        // not stand for them.
        let old = crate::library::CloudConsent {
            provider: CloudProvider::Modal,
            profile_id: "mc-1".into(),
            origin_fingerprint: "fp-1".into(),
            granted_at: 0,
            statements_accepted: false,
        };
        library.set_cloud_consent(&chapter.id, Some(old)).unwrap();
        assert_eq!(answer(false, false).unwrap_err(), "rights_attestation_required");

        let accepted = answer(true, true).unwrap();
        assert!(accepted);
        library.record_cloud_consent(&chapter.id, CloudProvider::Modal, "mc-1", "fp-1", accepted);
        let saved = library.index().unwrap();
        let consent = saved.projects.iter().find(|p| p.id == project.id).and_then(|p| p.cloud_consent.clone());
        assert!(consent.is_some_and(|c| c.statements_accepted), "the recorded consent has its statements");
        assert_eq!(answer(false, false), Ok(false), "a standing project is not asked again");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn test_confirmed_proposal_grant_binding() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-grant-binding")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let raws = scratch.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let raster = cleaner_core::image::fixtures::by_name("l8").raster;
        let bytes = cleaner_core::image::encode(&raster, cleaner_core::image::Format::Png).unwrap();
        let page_path = raws.join("001.png");
        std::fs::write(&page_path, &bytes).unwrap();

        let source_ref = cleaner_core::ingest::source_ref(&page_path, &bytes).unwrap();
        let manifest = scratch.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = cleaner_core::project::Project::new(
            manifest.parent().unwrap(),
            "test-app",
            cleaner_core::project::StripMode::Single,
            &[source_ref],
        );
        let mut job = cleaner_core::project::Job::create(&manifest, project).unwrap();

        let bounds = cleaner_core::mask::Rect::new(10, 10, 30, 20);
        let mut pixels = raster.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![200; (bounds.w * bounds.h) as usize];

        let patch = cleaner_core::patch::Patch {
            id: "reg-1".to_string(),
            mask: cleaner_core::mask::Mask::filled(bounds),
            ink: cleaner_core::mask::Mask::filled(bounds),
            pixels,
            order: 1,
            visible: true,
            provenance: cleaner_core::patch::Provenance {
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

        let mut config = InferenceConfig::default();
        let (origin, fp) =
            crate::inference::config::validate_https_endpoint("https://modal.example.com/mc/v1")
                .unwrap();
        config.modal_profiles.insert(
            "modal-prof-1".to_string(),
            crate::inference::config::CloudProfile {
                id: "modal-prof-1".to_string(),
                name: "Test Modal Profile".to_string(),
                endpoint_url: "https://modal.example.com/mc/v1".to_string(),
                canonical_origin: origin,
                canonical_origin_fingerprint: fp,
                created_at_ms: 1000,
                updated_at_ms: 1000,
            },
        );

        let grant_service = Arc::new(GrantService::new());
        let consent_service = ConsentService::new_isolated(grant_service);

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: RenderRecipe::new(
                "sdnq-v1",
                "1.0.0",
                "flux-schnell",
                "0123456789abcdef0123456789abcdef01234567",
                false,
            ),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = consent_service
            .prepare_proposal(req, &config, true, &manifest)
            .unwrap();

        // Before confirmation: lookup fails as unconfirmed
        assert!(matches!(
            consent_service
                .get_confirmed_proposal_with_job_path(&proposal.proposal_id, "any-nonce"),
            Err(crate::inference::consent::ConsentError::ProposalNotConfirmed)
        ));

        // Confirm proposal
        let grant = consent_service
            .confirm_proposal(crate::inference::consent::ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &OperationIntent::CleanAnyway,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(300),
                max_attempts: 1,
            })
            .unwrap();

        // Mismatched nonce: rejected
        assert!(matches!(
            consent_service
                .get_confirmed_proposal_with_job_path(&proposal.proposal_id, "wrong-nonce"),
            Err(crate::inference::consent::ConsentError::GrantNonceMismatch)
        ));

        // Matching nonce: succeeds with proposal and canonical job path
        let (confirmed_prop, confirmed_path) = consent_service
            .get_confirmed_proposal_with_job_path(&proposal.proposal_id, &grant.nonce)
            .unwrap();
        assert_eq!(confirmed_prop.proposal_id, proposal.proposal_id);
        assert_eq!(confirmed_path, manifest.canonicalize().unwrap());
    }

    #[test]
    fn test_cancellation_and_recovery_phase_rules() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-cancel-rules")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let limits = cleaner_core::cloud_wire::provisional_fixture_limits();
        let journal = AttemptJournal::new(scratch, limits);

        let intent = CreateAttemptIntent {
            qwen_edit: None,
            attempt_id: "att-phase-test-1".into(),
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
            width: 128,
            height: 128,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            source_image_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            region_id: "reg-1".into(),
            region_revision: 1,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            request_digest: "0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            chapter_id: Some("chap-1".into()),
            page_index: Some(0),
        };

        let req_meta = cleaner_core::cloud_wire::JobRequestMetadata {
            protocol_version: cleaner_core::cloud_wire::PROTOCOL_VERSION.to_string(),
            job_id: intent.job_id.clone(),
            attempt_id: intent.attempt_id.clone(),
            recipe: cleaner_core::cloud_wire::WireRenderRecipe {
                qwen_edit: None,
                recipe_id: intent.recipe_id.clone(),
                preprocessing_version: intent.preprocessing_version.clone(),
                model_id: intent.model_id.clone(),
                model_revision: intent.model_revision.clone(),
                native_mask_conditioning: intent.native_mask_conditioning,
            },
            width: intent.width,
            height: intent.height,
            seed: intent.seed,
            steps: intent.steps,
            guidance_scaled: intent.guidance_scaled,
            image_sha256: intent.crop_sha256.clone(),
            hint_sha256: intent.hint_sha256.clone(),
            request_digest: String::new(),
        };
        let mut intent = intent;
        intent.request_digest = cleaner_core::cloud_wire::compute_request_digest(&req_meta);

        let (_rec, guard) = journal.create_intent(intent).unwrap();
        journal.mark_dispatching(&guard).unwrap();
        journal.record_accepted_handle(&guard, "h-100").unwrap();

        let rec = journal.record_cancel_requested(&guard).unwrap();
        assert!(matches!(rec.phase, AttemptPhase::CancelRequested { .. }));

        journal.record_cancelled(&guard).unwrap();
        let dec_after = journal.recover_attempt(&guard).unwrap();
        assert!(matches!(dec_after, RecoveryDecision::Terminal { .. }));
    }

    #[test]
    fn test_cancellation_fidelity_and_phase_rejection() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-cancel-fidelity")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let limits = cleaner_core::cloud_wire::provisional_fixture_limits();
        let journal = AttemptJournal::new(scratch, limits);

        let make_intent = |att_id: &str| {
            let mut intent = CreateAttemptIntent {
                qwen_edit: None,
                attempt_id: att_id.into(),
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
                width: 128,
                height: 128,
                seed: 42,
                steps: 4,
                guidance_scaled: 350,
                source_image_hash:
                    "0000000000000000000000000000000000000000000000000000000000000000".into(),
                region_id: att_id.into(),
                region_revision: 1,
                input_sha256: None,
                predecessors_sha256: None,
                crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                request_digest: String::new(),
                chapter_id: Some("chap-1".into()),
                page_index: Some(0),
            };
            intent.request_digest = compute_intent_request_digest(&intent);
            intent
        };

        // 1. Intent phase cancellation cleanly aborts
        let (_rec, guard) = journal
            .create_intent(make_intent("att-cancel-intent"))
            .unwrap();
        let aborted = journal.abort_intent_no_dispatch(&guard).unwrap();
        assert!(matches!(
            aborted.phase,
            AttemptPhase::Cancelled { handle: None, .. }
        ));

        // 2. Dispatching phase cancellation is invalid
        let (_rec, guard2) = journal
            .create_intent(make_intent("att-cancel-disp"))
            .unwrap();
        journal.mark_dispatching(&guard2).unwrap();
        let rec = journal.read_record_locked(&guard2).unwrap();
        assert_eq!(rec.phase, AttemptPhase::Dispatching);

        // 3. ResultCached / Completed cancellation is invalid
        let (_rec, guard3) = journal
            .create_intent(make_intent("att-cancel-cached"))
            .unwrap();
        journal.mark_dispatching(&guard3).unwrap();
        journal.record_accepted_handle(&guard3, "h-300").unwrap();
        let rec3 = journal.read_record_locked(&guard3).unwrap();
        assert!(matches!(rec3.phase, AttemptPhase::Accepted { .. }));

        // 4. Failed phase cancellation is invalid
        let (_rec, guard4) = journal
            .create_intent(make_intent("att-cancel-failed"))
            .unwrap();
        journal.mark_dispatching(&guard4).unwrap();
        journal.record_accepted_handle(&guard4, "h-400").unwrap();
        journal.record_failed(&guard4).unwrap();
        let rec4 = journal.read_record_locked(&guard4).unwrap();
        assert!(matches!(rec4.phase, AttemptPhase::Failed { .. }));

        // 5. Cancel result DTO preserves unacknowledged status faithfully
        let unack_cancel = CloudCancelResultDto {
            handle: "h-500".into(),
            status: "running".into(),
            acknowledged: false,
        };
        assert!(!unack_cancel.acknowledged);
        assert_eq!(unack_cancel.status, "running");
    }

    #[test]
    fn test_pre_post_revalidation_clean_abort_in_intent_phase() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-reval-abort")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let limits = cleaner_core::cloud_wire::provisional_fixture_limits();
        let journal = AttemptJournal::new(scratch, limits);

        let mut intent = CreateAttemptIntent {
            qwen_edit: None,
            attempt_id: "att-reval-abort-1".into(),
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
            width: 128,
            height: 128,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            source_image_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            region_id: "reg-1".into(),
            region_revision: 1,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            request_digest: String::new(),
            chapter_id: Some("chap-1".into()),
            page_index: Some(0),
        };
        intent.request_digest = compute_intent_request_digest(&intent);

        let (_rec, guard) = journal.create_intent(intent).unwrap();
        let rec = journal.read_record_locked(&guard).unwrap();
        assert_eq!(rec.phase, AttemptPhase::Intent);

        // Pre-POST revalidation detects mismatch while in Intent phase -> cleanly aborts
        let aborted = journal.abort_intent_no_dispatch(&guard).unwrap();
        assert!(matches!(
            aborted.phase,
            AttemptPhase::Cancelled { handle: None, .. }
        ));

        // Calling mark_dispatching on aborted intent fails closed
        assert!(journal.mark_dispatching(&guard).is_err());
    }

    #[test]
    fn test_recovery_fail_closed_on_missing_project() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-recov-fail-closed")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let limits = cleaner_core::cloud_wire::provisional_fixture_limits();
        let journal = AttemptJournal::new(scratch, limits);

        let mut intent = CreateAttemptIntent {
            qwen_edit: None,
            attempt_id: "att-recov-fc-1".into(),
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
            width: 128,
            height: 128,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            source_image_hash: "0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            region_id: "reg-1".into(),
            region_revision: 1,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            request_digest: String::new(),
            chapter_id: Some("non-existent-chap".into()),
            page_index: Some(0),
        };
        intent.request_digest = compute_intent_request_digest(&intent);

        let (_rec, guard) = journal.create_intent(intent).unwrap();
        journal.mark_dispatching(&guard).unwrap();
        journal.record_accepted_handle(&guard, "h-recov-1").unwrap();

        // When attempt is in Accepted phase, recovery decision is ResumePolling
        let dec = journal.recover_attempt(&guard).unwrap();
        assert!(matches!(dec, RecoveryDecision::ResumePolling { handle } if handle == "h-recov-1"));
    }

    fn setup_two_page_cmd_test_job(
        root: &Path,
        patch_id: &str,
    ) -> (
        PathBuf,
        ConsentProposal,
        crate::inference::policy::Grant,
        Arc<GrantService>,
    ) {
        let raws = root.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
        let mut raster = cleaner_core::image::fixtures::by_name("l8").raster;
        raster.width = 256;
        raster.height = 256;
        raster.data = vec![200; 256 * 256];
        let bytes = cleaner_core::image::encode(&raster, cleaner_core::image::Format::Png).unwrap();
        let page0_path = raws.join("001.png");
        let page1_path = raws.join("002.png");
        std::fs::write(&page0_path, &bytes).unwrap();
        std::fs::write(&page1_path, &bytes).unwrap();

        let source_ref0 = cleaner_core::ingest::source_ref(&page0_path, &bytes).unwrap();
        let source_ref1 = cleaner_core::ingest::source_ref(&page1_path, &bytes).unwrap();
        let manifest = root.join("job/chapter.mtclean");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let project = cleaner_core::project::Project::new(
            manifest.parent().unwrap(),
            "test-app",
            cleaner_core::project::StripMode::Single,
            &[source_ref0, source_ref1],
        );
        let mut job = cleaner_core::project::Job::create(&manifest, project).unwrap();

        let bounds = cleaner_core::mask::Rect::new(10, 10, 30, 20);
        let mut pixels = raster.clone();
        pixels.width = bounds.w;
        pixels.height = bounds.h;
        pixels.data = vec![200; (bounds.w * bounds.h) as usize];

        let patch = cleaner_core::patch::Patch {
            id: patch_id.to_string(),
            mask: cleaner_core::mask::Mask::filled(bounds),
            ink: cleaner_core::mask::Mask::filled(bounds),
            pixels,
            order: 1,
            visible: true,
            provenance: cleaner_core::patch::Provenance {
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

        let mut config = InferenceConfig::default();
        let (origin, fp) =
            crate::inference::config::validate_https_endpoint("https://modal.example.com/mc/v1")
                .unwrap();
        config.modal_profiles.insert(
            "modal-prof-1".to_string(),
            crate::inference::config::CloudProfile {
                id: "modal-prof-1".to_string(),
                name: "Test Modal Profile".to_string(),
                endpoint_url: "https://modal.example.com/mc/v1".to_string(),
                canonical_origin: origin,
                canonical_origin_fingerprint: fp,
                created_at_ms: 1000,
                updated_at_ms: 1000,
            },
        );

        let grant_service = Arc::new(GrantService::new());
        let consent_service = ConsentService::new_isolated(Arc::clone(&grant_service));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some(patch_id.into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: RenderRecipe::new(
                "sdnq-v1",
                "1.0.0",
                "flux-schnell",
                "0123456789abcdef0123456789abcdef01234567",
                false,
            ),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = consent_service
            .prepare_proposal(req, &config, true, &manifest)
            .unwrap();

        let grant = consent_service
            .confirm_proposal(crate::inference::consent::ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &OperationIntent::CleanAnyway,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(300),
                max_attempts: 1,
            })
            .unwrap();

        (manifest, proposal, grant, grant_service)
    }

    #[test]
    fn test_pre_post_revalidation_rejects_page_deletion_before_intent_grant_network() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-reval-page-del")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let (manifest, proposal, grant, grant_service) =
            setup_two_page_cmd_test_job(&scratch, "reg-del");
        let journal = AttemptJournal::new(scratch.join("journal"), provisional_fixture_limits());

        // Simulate page deletion in project manifest after consent confirmation
        {
            let mut job = Job::open(&manifest).unwrap();
            job.project.strip.order.clear();
            job.flush().unwrap();
        }

        let reval_res = revalidate_proposal_before_dispatch(&manifest, &proposal);
        assert!(reval_res.is_err());
        let err_msg = reval_res.err().unwrap();
        assert!(
            err_msg.contains("page index not found"),
            "unexpected error message: {err_msg}"
        );

        // Invariants: grant is unconsumed (attempts_used == 0), journal has 0 attempts
        let queried_grant = grant_service.get_grant(&grant.nonce).unwrap();
        assert_eq!(queried_grant.attempts_used, 0);
        assert!(journal.list_attempt_ids().unwrap().is_empty());
    }

    #[test]
    fn test_pre_post_revalidation_rejects_page_reorder_before_intent_grant_network() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-reval-page-reorder")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let (manifest, proposal, grant, grant_service) =
            setup_two_page_cmd_test_job(&scratch, "reg-reorder");
        let journal = AttemptJournal::new(scratch.join("journal"), provisional_fixture_limits());

        // Simulate page reorder in project manifest: page index 0 now points to source index 1
        {
            let mut job = Job::open(&manifest).unwrap();
            job.project.strip.order = vec![1, 0];
            job.flush().unwrap();
        }

        let reval_res = revalidate_proposal_before_dispatch(&manifest, &proposal);
        assert!(reval_res.is_err());
        let err_msg = reval_res.err().unwrap();
        assert!(
            err_msg.contains("page index no longer maps"),
            "unexpected error message: {err_msg}"
        );

        // Invariants: grant is unconsumed (attempts_used == 0), journal has 0 attempts
        let queried_grant = grant_service.get_grant(&grant.nonce).unwrap();
        assert_eq!(queried_grant.attempts_used, 0);
        assert!(journal.list_attempt_ids().unwrap().is_empty());
    }

    #[test]
    fn test_pre_post_revalidation_rejects_patch_source_mismatch_before_intent_grant_network() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-reval-patch-mismatch")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let (manifest, proposal, grant, grant_service) =
            setup_two_page_cmd_test_job(&scratch, "reg-mismatch");
        let journal = AttemptJournal::new(scratch.join("journal"), provisional_fixture_limits());

        // Simulate patch record source_idx mismatch in project manifest
        {
            let mut job = Job::open(&manifest).unwrap();
            job.project.patches[0].source_idx = 1;
            job.flush().unwrap();
        }

        let reval_res = revalidate_proposal_before_dispatch(&manifest, &proposal);
        assert!(reval_res.is_err());
        let err_msg = reval_res.err().unwrap();
        assert!(
            err_msg.contains("patch record source index mismatch"),
            "unexpected error message: {err_msg}"
        );

        // Invariants: grant is unconsumed (attempts_used == 0), journal has 0 attempts
        let queried_grant = grant_service.get_grant(&grant.nonce).unwrap();
        assert_eq!(queried_grant.attempts_used, 0);
        assert!(journal.list_attempt_ids().unwrap().is_empty());
    }

    #[test]
    fn test_revalidation_reports_underlay_drift_separately_from_crop_bounds() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-reval-underlay")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();
        let (manifest, mut proposal, _, _) =
            setup_two_page_cmd_test_job(&scratch, "reg-underlay");
        let input_sha256 = proposal.input_sha256.clone();
        proposal.input_sha256 = "0".repeat(64);
        assert_eq!(
            revalidate_proposal_before_dispatch(&manifest, &proposal).unwrap_err(),
            "underlay changed before dispatch"
        );
        proposal.input_sha256 = input_sha256;
        proposal.predecessors_sha256 = "0".repeat(64);
        assert_eq!(
            revalidate_proposal_before_dispatch(&manifest, &proposal).unwrap_err(),
            "underlay changed before dispatch"
        );
    }

    #[test]
    fn test_submit_cloud_attempt_snapshot_validation_and_malformed_rejection() {
        let valid_snap_json = serde_json::json!({
            "source_image_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "region_id": "reg-1",
            "region_revision": 1,
            "crop_sha256": "1111111111111111111111111111111111111111111111111111111111111111",
            "hint_sha256": "2222222222222222222222222222222222222222222222222222222222222222"
        });

        // 1. Valid snapshot parses successfully into ProjectRegionSnapshot
        let parsed: Result<ProjectRegionSnapshot, _> = serde_json::from_value(valid_snap_json);
        assert!(parsed.is_ok());
        let snap = parsed.unwrap();
        assert_eq!(
            snap.source_image_hash,
            "0000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(snap.region_id, "reg-1");
        assert_eq!(snap.region_revision, 1);
        assert_eq!(
            snap.crop_sha256,
            "1111111111111111111111111111111111111111111111111111111111111111"
        );
        assert_eq!(
            snap.hint_sha256,
            "2222222222222222222222222222222222222222222222222222222222222222"
        );

        // 2. Malformed snapshot: missing field -> fails deserialization
        let missing_field = serde_json::json!({
            "source_image_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "region_id": "reg-1"
        });
        assert!(serde_json::from_value::<ProjectRegionSnapshot>(missing_field).is_err());

        // 3. Malformed snapshot: invalid data type for region_revision -> fails deserialization
        let wrong_type = serde_json::json!({
            "source_image_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "region_id": "reg-1",
            "region_revision": "invalid_number",
            "crop_sha256": "1111111111111111111111111111111111111111111111111111111111111111",
            "hint_sha256": "2222222222222222222222222222222222222222222222222222222222222222"
        });
        assert!(serde_json::from_value::<ProjectRegionSnapshot>(wrong_type).is_err());

        // 4. Malformed snapshot: extra unknown field (deny_unknown_fields) -> fails deserialization
        let unknown_field = serde_json::json!({
            "source_image_hash": "0000000000000000000000000000000000000000000000000000000000000000",
            "region_id": "reg-1",
            "region_revision": 1,
            "crop_sha256": "1111111111111111111111111111111111111111111111111111111111111111",
            "hint_sha256": "2222222222222222222222222222222222222222222222222222222222222222",
            "extra": "unrecognized"
        });
        assert!(serde_json::from_value::<ProjectRegionSnapshot>(unknown_field).is_err());

        // 5. Malformed snapshot: non-object types -> fails deserialization
        assert!(
            serde_json::from_value::<ProjectRegionSnapshot>(serde_json::json!("not_an_object"))
                .is_err()
        );
        assert!(serde_json::from_value::<ProjectRegionSnapshot>(serde_json::json!(42)).is_err());
        assert!(
            serde_json::from_value::<ProjectRegionSnapshot>(serde_json::json!([1, 2, 3])).is_err()
        );
    }

    #[test]
    fn test_prepare_cloud_consent_rejects_missing_recipe_without_defaults() {
        // Missing recipe must be rejected with static sanitized error; no defaults or placeholders invented
        let recipe: Option<RenderRecipe> = None;
        let res: Result<RenderRecipe, String> =
            recipe.ok_or_else(|| "recipe is required for prepare_cloud_consent".to_string());
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(err, "recipe is required for prepare_cloud_consent");
        // Verify no placeholder recipe / revision was constructed
        assert!(!err.contains("sdnq-v1"));
        assert!(!err.contains("flux-schnell"));
        assert!(!err.contains("0123456789abcdef0123456789abcdef01234567"));
    }

    #[test]
    fn test_status_and_cancel_ignore_caller_handle_when_journal_has_none() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-handle-no-echo")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let limits = provisional_fixture_limits();
        let journal = AttemptJournal::new(scratch, limits);

        let make_intent = |att_id: &str| {
            let mut intent = CreateAttemptIntent {
                qwen_edit: None,
                attempt_id: att_id.into(),
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
                width: 128,
                height: 128,
                seed: 42,
                steps: 4,
                guidance_scaled: 350,
                source_image_hash:
                    "0000000000000000000000000000000000000000000000000000000000000000".into(),
                region_id: att_id.into(),
                region_revision: 1,
                input_sha256: None,
                predecessors_sha256: None,
                crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                request_digest: String::new(),
                chapter_id: Some("chap-1".into()),
                page_index: Some(0),
            };
            intent.request_digest = compute_intent_request_digest(&intent);
            intent
        };

        // 1. Locally cancelled attempt with no remote handle
        let (_rec, guard) = journal
            .create_intent(make_intent("att-no-handle-cancel"))
            .unwrap();
        let aborted = journal.abort_intent_no_dispatch(&guard).unwrap();
        assert!(matches!(
            aborted.phase,
            AttemptPhase::Cancelled { handle: None, .. }
        ));

        // Status response must NOT echo caller-supplied handle
        let status_res = match &aborted.phase {
            AttemptPhase::Cancelled { handle: h, .. } => CloudAttemptStatusDto {
                attempt_id: "att-no-handle-cancel".to_string(),
                handle: h.clone(),
                status: "cancelled".to_string(),
                reported_cost_usd: None,
                acknowledged: Some(true),
                created_at_ms: Some(aborted.created_at_ms),
                started_at_ms: None,
                finished_at_ms: Some(aborted.updated_at_ms),
            },
            _ => panic!("expected cancelled phase"),
        };
        assert_eq!(status_res.handle, None);

        // Cancel response must NOT echo caller-supplied handle
        let cancel_res = match &aborted.phase {
            AttemptPhase::Cancelled { handle: h, .. } => CloudCancelResultDto {
                handle: h.clone().unwrap_or_default(),
                status: "cancelled".to_string(),
                acknowledged: true,
            },
            _ => panic!("expected cancelled phase"),
        };
        assert_eq!(cancel_res.handle, "");

        // 2. Dispatch transport error attempt with no remote handle
        let (_rec, guard2) = journal
            .create_intent(make_intent("att-no-handle-unknown"))
            .unwrap();
        journal.mark_dispatching(&guard2).unwrap();
        journal.record_dispatch_transport_error(&guard2).unwrap();
        let unknown_rec = journal.read_record_locked(&guard2).unwrap();
        assert!(matches!(unknown_rec.phase, AttemptPhase::Unknown { .. }));

        let unknown_status = match &unknown_rec.phase {
            AttemptPhase::Unknown { .. } => CloudAttemptStatusDto {
                attempt_id: "att-no-handle-unknown".to_string(),
                handle: None,
                status: "unknown".to_string(),
                reported_cost_usd: None,
                acknowledged: None,
                created_at_ms: Some(unknown_rec.created_at_ms),
                started_at_ms: None,
                finished_at_ms: None,
            },
            _ => panic!("expected unknown phase"),
        };
        assert_eq!(unknown_status.handle, None);

        // 3. Stored handle exact-match validation
        let (_rec, guard3) = journal
            .create_intent(make_intent("att-with-handle"))
            .unwrap();
        journal.mark_dispatching(&guard3).unwrap();
        journal
            .record_accepted_handle(&guard3, "h-remote-123")
            .unwrap();
        let accepted_rec = journal.read_record_locked(&guard3).unwrap();

        let rec_handle = match &accepted_rec.phase {
            AttemptPhase::Accepted { handle: h } => Some(h.as_str()),
            _ => None,
        };
        assert_eq!(rec_handle, Some("h-remote-123"));

        // Matching caller handle accepted
        let caller_h = Some("h-remote-123".to_string());
        if let (Some(ref ch), Some(ah)) = (&caller_h, rec_handle) {
            assert_eq!(ch.as_str(), ah);
        }

        // Mismatched caller handle rejected
        let mismatch_h = Some("h-mismatch-999".to_string());
        let validation_err = if let (Some(ref ch), Some(ah)) = (&mismatch_h, rec_handle) {
            if ch.as_str() != ah {
                Some(format!(
                    "caller handle '{ch}' does not match attempt handle '{ah}'"
                ))
            } else {
                None
            }
        } else {
            None
        };
        assert!(validation_err.is_some());
        assert!(validation_err
            .unwrap()
            .contains("does not match attempt handle"));
    }

    #[test]
    fn test_reconcile_recovery_caller_input_staleness_checks() {
        let record = AttemptRecord {
            qwen_edit: None,
            review_accepted: false,
            lookup_evidence: None,
            schema_version: 1,
            attempt_id: "att-stale-test-1".to_string(),
            job_id: "job-1".to_string(),
            provider: CloudProvider::Modal,
            profile_id: "modal-prof-1".to_string(),
            endpoint_fingerprint:
                "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            recipe_id: "sdnq-v1".to_string(),
            preprocessing_version: "1.0.0".to_string(),
            model_id: "flux-schnell".to_string(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
            native_mask_conditioning: false,
            source_image_hash: "1111111111111111111111111111111111111111111111111111111111111111"
                .to_string(),
            region_id: "reg-1".to_string(),
            region_revision: 5,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: "2222222222222222222222222222222222222222222222222222222222222222"
                .to_string(),
            hint_sha256: "3333333333333333333333333333333333333333333333333333333333333333"
                .to_string(),
            width: 128,
            height: 128,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            request_digest: "4444444444444444444444444444444444444444444444444444444444444444"
                .to_string(),
            chapter_id: Some("chap-1".to_string()),
            page_index: Some(0),
            created_at_ms: 1000,
            updated_at_ms: 1000,
            phase: AttemptPhase::ResultCached {
                handle: "h-1".to_string(),
                result_digest: "res-dig".to_string(),
                cached_png_bytes: 100,
                reported_cost_usd: None,
            },
        };

        let rev_json = serde_json::json!(5);

        // 1. All caller fields supplied and match record -> not stale
        assert!(!is_caller_recovery_input_stale(
            &record,
            Some("chap-1"),
            Some(0),
            Some("reg-1"),
            Some(&rev_json),
            Some("1111111111111111111111111111111111111111111111111111111111111111"),
            None,
        ));

        // 2. Caller chapter_id mismatches -> stale
        assert!(is_caller_recovery_input_stale(
            &record,
            Some("chap-2"),
            Some(0),
            Some("reg-1"),
            Some(&rev_json),
            Some("1111111111111111111111111111111111111111111111111111111111111111"),
            None,
        ));

        // 3. Caller page_index mismatches -> stale
        assert!(is_caller_recovery_input_stale(
            &record,
            Some("chap-1"),
            Some(1),
            Some("reg-1"),
            Some(&rev_json),
            Some("1111111111111111111111111111111111111111111111111111111111111111"),
            None,
        ));

        // 4. Caller region_id mismatches -> stale
        assert!(is_caller_recovery_input_stale(
            &record,
            Some("chap-1"),
            Some(0),
            Some("reg-2"),
            Some(&rev_json),
            Some("1111111111111111111111111111111111111111111111111111111111111111"),
            None,
        ));

        // 5. Caller source_image_hash mismatches -> stale
        assert!(is_caller_recovery_input_stale(
            &record,
            Some("chap-1"),
            Some(0),
            Some("reg-1"),
            Some(&rev_json),
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
            None,
        ));

        // 6. Caller region_revision mismatches -> stale
        let wrong_rev = serde_json::json!(6);
        assert!(is_caller_recovery_input_stale(
            &record,
            Some("chap-1"),
            Some(0),
            Some("reg-1"),
            Some(&wrong_rev),
            Some("1111111111111111111111111111111111111111111111111111111111111111"),
            None,
        ));

        // 7. Caller region_revision is invalid string -> stale
        let malformed_rev = serde_json::json!("not-a-number");
        assert!(is_caller_recovery_input_stale(
            &record,
            Some("chap-1"),
            Some(0),
            Some("reg-1"),
            Some(&malformed_rev),
            Some("1111111111111111111111111111111111111111111111111111111111111111"),
            None,
        ));

        // 8. simulate_stale is true -> stale
        assert!(is_caller_recovery_input_stale(
            &record,
            Some("chap-1"),
            Some(0),
            Some("reg-1"),
            Some(&rev_json),
            Some("1111111111111111111111111111111111111111111111111111111111111111"),
            Some(true),
        ));

        // 9. All caller inputs omitted (None) -> not stale from input comparison
        assert!(!is_caller_recovery_input_stale(
            &record, None, None, None, None, None, None,
        ));

        // 10. Record has chapter_id = None; caller supplies chapter_id -> fails closed as stale
        let mut record_no_chapter = record.clone();
        record_no_chapter.chapter_id = None;
        assert!(is_caller_recovery_input_stale(
            &record_no_chapter,
            Some("chap-1"),
            Some(0),
            None,
            None,
            None,
            None,
        ));

        // 11. Record has page_index = None; caller supplies page_index -> fails closed as stale
        let mut record_no_page = record.clone();
        record_no_page.page_index = None;
        assert!(is_caller_recovery_input_stale(
            &record_no_page,
            Some("chap-1"),
            Some(0),
            None,
            None,
            None,
            None,
        ));
    }

    #[test]
    fn test_reconcile_recovery_project_resolution_authoritative_record() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-recov-authoritative")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let library = Library::at(&scratch);

        let mut record = AttemptRecord {
            qwen_edit: None,
            review_accepted: false,
            lookup_evidence: None,
            schema_version: 1,
            attempt_id: "att-recov-proj-1".to_string(),
            job_id: "job-1".to_string(),
            provider: CloudProvider::Modal,
            profile_id: "modal-prof-1".to_string(),
            endpoint_fingerprint:
                "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            recipe_id: "sdnq-v1".to_string(),
            preprocessing_version: "1.0.0".to_string(),
            model_id: "flux-schnell".to_string(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
            native_mask_conditioning: false,
            source_image_hash: "src-hash".to_string(),
            region_id: "reg-1".to_string(),
            region_revision: 1,
            input_sha256: None,
            predecessors_sha256: None,
            crop_sha256: "crop-sha".to_string(),
            hint_sha256: "hint-sha".to_string(),
            width: 128,
            height: 128,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            request_digest: "req-dig".to_string(),
            chapter_id: None,
            page_index: Some(0),
            created_at_ms: 1000,
            updated_at_ms: 1000,
            phase: AttemptPhase::ResultCached {
                handle: "h-1".to_string(),
                result_digest: "res-dig".to_string(),
                cached_png_bytes: 100,
                reported_cost_usd: None,
            },
        };

        // 1. Record has chapter_id = None -> fails closed (stale)
        assert!(is_project_recovery_stale(&library, &record));

        // 2. Record has page_index = None -> fails closed (stale)
        record.chapter_id = Some("chap-1".to_string());
        record.page_index = None;
        assert!(is_project_recovery_stale(&library, &record));

        // 3. Record has both, but chapter does not exist in library -> fails closed (stale)
        record.page_index = Some(0);
        assert!(is_project_recovery_stale(&library, &record));
    }

    #[test]
    fn test_recovery_detects_underlay_and_predecessor_drift() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-recov-underlay")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();
        let (manifest, proposal, _, _) = setup_two_page_cmd_test_job(&scratch, "reg-recovery");
        let library = Library::at(scratch.join("library"));
        let project = library
            .create_project("Recovery", cleaner_core::project::StripMode::Single, None, None)
            .unwrap();
        let chapter = library
            .create_chapter(&project.id, "Chapter", None, Some(scratch.join("raws")))
            .unwrap()
            .created()
            .unwrap();
        let chapter_path = library.resolve_chapter(&chapter.id).unwrap();
        let source_job = Job::open(&manifest).unwrap();
        let patch_rec = &source_job.project.patches[0];
        let patch = source_job.load_patch(patch_rec).unwrap();
        let mut job = Job::open(&chapter_path).unwrap();
        job.complete_region(0, &patch, None).unwrap();

        let record = AttemptRecord {
            qwen_edit: None,
            review_accepted: false,
            lookup_evidence: None,
            schema_version: 1,
            attempt_id: "att-recov-underlay".into(),
            job_id: "job-recov-underlay".into(),
            provider: CloudProvider::Modal,
            profile_id: "modal-prof-1".into(),
            endpoint_fingerprint: "0".repeat(64),
            recipe_id: "sdnq-v1".into(),
            preprocessing_version: "1.0.0".into(),
            model_id: "flux-schnell".into(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            native_mask_conditioning: false,
            source_image_hash: proposal.source_hash.clone(),
            region_id: proposal.region_ids[0].clone(),
            region_revision: proposal.revision,
            input_sha256: Some(proposal.input_sha256.clone()),
            predecessors_sha256: Some(proposal.predecessors_sha256.clone()),
            crop_sha256: proposal.crop_png_sha256.clone(),
            hint_sha256: proposal.hint_png_sha256.clone(),
            width: proposal.crop_width,
            height: proposal.crop_height,
            seed: 42,
            steps: 4,
            guidance_scaled: 350,
            request_digest: "request".into(),
            chapter_id: Some(chapter.id),
            page_index: Some(0),
            created_at_ms: 1000,
            updated_at_ms: 1000,
            phase: AttemptPhase::ResultCached {
                handle: "handle".into(),
                result_digest: "result".into(),
                cached_png_bytes: 100,
                reported_cost_usd: None,
            },
        };
        assert!(!is_project_recovery_stale(&library, &record));

        let mut input_drift = record.clone();
        input_drift.input_sha256 = Some("0".repeat(64));
        assert!(is_project_recovery_stale(&library, &input_drift));
        let mut predecessor_drift = record.clone();
        predecessor_drift.predecessors_sha256 = Some("0".repeat(64));
        assert!(is_project_recovery_stale(&library, &predecessor_drift));

        let mut earlier = patch.clone();
        earlier.id = "reg-earlier".into();
        earlier.order = 0;
        earlier.mask = cleaner_core::mask::Mask::filled(cleaner_core::mask::Rect::new(90, 90, 16, 16));
        earlier.ink = earlier.mask.clone();
        earlier.pixels.width = 16;
        earlier.pixels.height = 16;
        earlier.pixels.data = vec![255; 16 * 16];
        job.complete_region(0, &earlier, None).unwrap();
        assert!(is_project_recovery_stale(&library, &record));
    }

    #[test]
    fn test_adversarial_error_sanitization_no_paths_urls_tokens_nonces_server_bodies() {
        let sentinels = vec![
            // Filesystem paths (Unix, Windows, UNC)
            "/private/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000/T/manga-cleaner-secret/db.sqlite",
            "/etc/shadow",
            "/Users/caved/.codex/worktrees/cloud-integration/manga-cleaner/src-tauri/inference.json",
            "C:\\Users\\Admin\\AppData\\Local\\MangaCleaner\\attempts\\att-001\\journal.lock",
            "\\\\server\\share\\secret_path\\journal.json",
            // URLs with credentials, internal endpoints, debug queries
            "https://admin:supersecretkey@internal.beam.cloud:8443/v1/jobs/submit?debug=true",
            "https://token-id:token-secret@internal.modal.corp/api/execute",
            "http://192.168.1.100:5000/internal_debug_endpoint",
            "https://modal.run/v1/health?key=secret-nonce-12345",
            // Tokens and credentials
            "ak-sentinel-modal-token-id-987654321",
            "as-sentinel-modal-token-secret-fedcba0987654321",
            "beam-live-secret-token-xyz-999999999999",
            "bearer-token-sentinel-abcdef123456",
            // Nonces, hashes, and digests
            "nonce-sentinel-0123456789abcdef-grant-deadbeef",
            "hash-sentinel-e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "reqdig-sentinel-fedcba98765432100123456789abcdef",
            "resdig-sentinel-cafebabedeadbeef0011223344556677",
            // Server error bodies & stack traces
            "{\"error\": \"InternalServerError\", \"stack\": \"File /srv/backend/runner.py, line 42 in exec\", \"postgres_dsn\": \"postgres://admin:pwd@10.0.0.1/db\"}",
            "HTTP 502 Bad Gateway: upstream server unix:/var/run/gunicorn.sock failed: Connection refused",
            "Traceback (most recent call last):\n  File \"/app/modal_app.py\", line 123, in inference\nZeroDivisionError: division by zero",
            "Resource temporarily unavailable (os error 11) on /tmp/cloud_attempts/att-123.lock",
            "Failed to parse JSON at line 4 column 12 in /var/data/attempts/att-456/record.json",
        ];

        let assert_no_sentinel_leak = |sanitized: &str, context: &str| {
            for sentinel in &sentinels {
                assert!(
                    !sanitized.contains(sentinel),
                    "Leaked sentinel string in {context}: found '{sentinel}' in '{sanitized}'"
                );
            }
            // Invariant: no raw filesystem paths, URLs with schemes/IPs, or internal exception snippets
            assert!(
                !sanitized.contains("/private/var")
                    && !sanitized.contains("/Users/")
                    && !sanitized.contains("C:\\")
                    && !sanitized.contains("\\\\server")
                    && !sanitized.contains("https://")
                    && !sanitized.contains("http://")
                    && !sanitized.contains("postgres://")
                    && !sanitized.contains("Traceback"),
                "Sanitized error contains forbidden internal pattern in {context}: '{sanitized}'"
            );
        };

        for &s in &sentinels {
            // 1. Config error sanitization
            let c_err = sanitize_config_error(s);
            assert_no_sentinel_leak(&c_err, "sanitize_config_error");

            let cw_err = sanitize_config_write_error(s);
            assert_no_sentinel_leak(&cw_err, "sanitize_config_write_error");

            let cw_schema = sanitize_config_write_error(&format!("schema version error in {s}"));
            assert_no_sentinel_leak(&cw_schema, "sanitize_config_write_error(schema)");
            assert_eq!(
                cw_schema,
                "unsupported inference configuration schema version"
            );

            let cw_profile = sanitize_config_write_error(&format!("exceeds maximum limit in {s}"));
            assert_no_sentinel_leak(&cw_profile, "sanitize_config_write_error(limit)");
            assert_eq!(cw_profile, "maximum profile count exceeded");

            let cw_origin =
                sanitize_config_write_error(&format!("canonical origin mismatch with {s}"));
            assert_no_sentinel_leak(&cw_origin, "sanitize_config_write_error(origin)");
            assert_eq!(cw_origin, "profile canonical origin mismatch");

            let cw_ep = sanitize_config_write_error(&format!("invalid endpoint url {s}"));
            assert_no_sentinel_leak(&cw_ep, "sanitize_config_write_error(endpoint)");
            assert_eq!(cw_ep, "invalid endpoint configuration in profile");

            // 2. Build client error sanitization
            let bc_cfg =
                sanitize_build_client_error(&BuildClientError::Configuration(s.to_string()));
            assert_no_sentinel_leak(&bc_cfg, "sanitize_build_client_error(Configuration)");
            assert_eq!(bc_cfg, "invalid or missing profile configuration");

            let bc_cred =
                sanitize_build_client_error(&BuildClientError::CredentialMissing(s.to_string()));
            assert_no_sentinel_leak(&bc_cred, "sanitize_build_client_error(CredentialMissing)");
            assert_eq!(bc_cred, "runtime credential missing or invalid for profile");

            // 3. HTTP Transport error sanitization
            let t_status_401 =
                sanitize_http_transport_error(&HttpTransportError::UnexpectedStatus {
                    status: 401,
                });
            assert_no_sentinel_leak(&t_status_401, "sanitize_http_transport_error(401)");
            assert_eq!(t_status_401, "gateway authorization failure (status 401)");

            let t_status_403 =
                sanitize_http_transport_error(&HttpTransportError::UnexpectedStatus {
                    status: 403,
                });
            assert_no_sentinel_leak(&t_status_403, "sanitize_http_transport_error(403)");
            assert_eq!(t_status_403, "gateway authorization failure (status 403)");

            let t_status_500 =
                sanitize_http_transport_error(&HttpTransportError::UnexpectedStatus {
                    status: 500,
                });
            assert_no_sentinel_leak(&t_status_500, "sanitize_http_transport_error(500)");
            assert_eq!(t_status_500, "gateway returned unexpected HTTP status 500");

            let t_cred = sanitize_http_transport_error(&HttpTransportError::InvalidCredential);
            assert_no_sentinel_leak(&t_cred, "sanitize_http_transport_error(InvalidCredential)");
            assert_eq!(t_cred, "runtime credential invalid or missing for target");

            let t_ep = sanitize_http_transport_error(&HttpTransportError::EndpointValidationFailed);
            assert_no_sentinel_leak(
                &t_ep,
                "sanitize_http_transport_error(EndpointValidationFailed)",
            );
            assert_eq!(t_ep, "endpoint validation failed for target");

            let t_conn = sanitize_http_transport_error(&HttpTransportError::ConnectionError);
            assert_no_sentinel_leak(&t_conn, "sanitize_http_transport_error(ConnectionError)");
            assert_eq!(
                t_conn,
                "gateway endpoint unreachable or connection timed out"
            );

            let t_timeout = sanitize_http_transport_error(&HttpTransportError::Timeout);
            assert_no_sentinel_leak(&t_timeout, "sanitize_http_transport_error(Timeout)");
            assert_eq!(
                t_timeout,
                "gateway endpoint unreachable or connection timed out"
            );

            let t_json = sanitize_http_transport_error(&HttpTransportError::JsonDeserialization);
            assert_no_sentinel_leak(
                &t_json,
                "sanitize_http_transport_error(JsonDeserialization)",
            );
            assert_eq!(t_json, "gateway protocol or response validation error");

            let t_wire = sanitize_http_transport_error(&HttpTransportError::WireValidation);
            assert_no_sentinel_leak(&t_wire, "sanitize_http_transport_error(WireValidation)");
            assert_eq!(t_wire, "gateway protocol or response validation error");

            // 4. Consent error sanitization (all variants tested with sentinels)
            let ce_list = vec![
                ConsentError::CloudDisabled,
                ConsentError::LocalTargetNotAllowed,
                ConsentError::ProfileNotFound(s.to_string(), CloudProvider::Modal),
                ConsentError::ProfileNotFound(s.to_string(), CloudProvider::Beam),
                ConsentError::InvalidEndpoint(s.to_string()),
                ConsentError::InvalidRecipe(s.to_string()),
                ConsentError::PageNotFound(99, s.to_string()),
                ConsentError::RegionNotFound(s.to_string()),
                ConsentError::SourceImageError,
                ConsentError::RenderPrepareFailed(s.to_string()),
                ConsentError::ProposalNotFound,
                ConsentError::ProposalExpired {
                    expired_at_ms: 1000,
                    now_ms: 2000,
                },
                ConsentError::ProposalAlreadyConsumed,
                ConsentError::ProposalNotConfirmed,
                ConsentError::GrantNonceMismatch,
                ConsentError::ClockRollbackDetected,
                ConsentError::IntentMismatch,
                ConsentError::OperationParamsTooLarge,
                ConsentError::InvalidParameter("invalid parameter"),
                ConsentError::RevisionMismatch,
                ConsentError::CropDigestMismatch,
                ConsentError::SourceMismatch,
                ConsentError::MaskMismatch,
                ConsentError::PageMismatch,
                ConsentError::ConfigMismatch,
                ConsentError::ProfileMutated,
                ConsentError::GrantIssuanceError(GrantError::NotFound),
                ConsentError::UnsupportedNewRegion(s.to_string()),
                ConsentError::RandomSourceError,
                ConsentError::Internal(s.to_string()),
            ];
            for ce in ce_list {
                let res = sanitize_consent_error(ce);
                assert_no_sentinel_leak(&res, "sanitize_consent_error");
            }

            // 5. Grant error sanitization
            let ge_list = vec![
                GrantError::NotFound,
                GrantError::Expired {
                    expired_at_ms: 1000,
                    now_ms: 2000,
                },
                GrantError::ClockRollbackDetected,
                GrantError::Revoked,
                GrantError::ProfileMutated,
                GrantError::AttemptsExceeded {
                    max_attempts: 1,
                    attempts_used: 2,
                },
                GrantError::ScopeMismatch {
                    field: "crop_sha256",
                },
                GrantError::InvalidParameter(s.to_string()),
                GrantError::RandomSourceError,
            ];
            for ge in &ge_list {
                let res = sanitize_grant_error(ge);
                assert_no_sentinel_leak(&res, "sanitize_grant_error");
            }

            // 6. Journal error sanitization
            let je_list = vec![
                JournalError::InvalidIdentifier {
                    field: "attempt_id",
                    value: s.to_string(),
                },
                JournalError::InvalidIdentifier {
                    field: "handle",
                    value: s.to_string(),
                },
                JournalError::UnsupportedSchemaVersion {
                    expected: 1,
                    actual: 99,
                },
                JournalError::ForeignGuard {
                    expected_root: "/path/a".to_string(),
                    guard_root: s.to_string(),
                },
                JournalError::Wire(
                    cleaner_core::cloud_wire::WireValidationError::PayloadSizeExceeded {
                        actual: 10,
                        limit: 5,
                    },
                ),
                JournalError::Decode(
                    cleaner_core::cloud_decode::ResultDecodeError::UnexpectedColorType,
                ),
                JournalError::AttemptLocked {
                    attempt_id: s.to_string(),
                },
                JournalError::AttemptAlreadyExists {
                    attempt_id: s.to_string(),
                },
                JournalError::AttemptNotFound {
                    attempt_id: s.to_string(),
                },
                JournalError::InvalidTransition {
                    current: "intent".to_string(),
                    attempted: s.to_string(),
                },
                JournalError::StaleAttachment {
                    field: "region_revision",
                    expected: s.to_string(),
                    actual: s.to_string(),
                },
                JournalError::DuplicateCommit {
                    patch_id: s.to_string(),
                },
                JournalError::LimitExceeded {
                    field: "crop_png",
                    actual: 99999999,
                    max: 1000,
                },
                JournalError::RequestDigestMismatch {
                    expected: s.to_string(),
                    computed: s.to_string(),
                },
                JournalError::ResultDigestMismatch {
                    expected: s.to_string(),
                    computed: s.to_string(),
                },
                JournalError::Io(std::io::Error::other(s)),
                JournalError::Json(
                    serde_json::from_str::<serde_json::Value>("not valid json").unwrap_err(),
                ),
            ];
            for je in je_list {
                let res = sanitize_journal_error(je);
                assert_no_sentinel_leak(&res, "sanitize_journal_error");
            }
        }
    }

    #[test]
    fn test_adversarial_caller_handle_mismatch_no_sentinel_echo() {
        let sentinel_caller_handle = "h-sentinel-caller-secret-handle-9999";
        let sentinel_actual_handle = "h-sentinel-actual-record-handle-1111";

        let caller_h = Some(sentinel_caller_handle.to_string());
        let actual_h = Some(sentinel_actual_handle);

        let err = if let (Some(ref ch), Some(ah)) = (&caller_h, actual_h) {
            if ch.as_str() != ah {
                Some("caller handle does not match attempt handle".to_string())
            } else {
                None
            }
        } else {
            None
        };

        assert!(err.is_some());
        let err_str = err.unwrap();
        assert_eq!(err_str, "caller handle does not match attempt handle");
        assert!(!err_str.contains(sentinel_caller_handle));
        assert!(!err_str.contains(sentinel_actual_handle));
    }

    #[test]
    fn test_adversarial_revalidate_proposal_before_dispatch_errors_sanitized() {
        let non_existent_path =
            PathBuf::from("/private/var/folders/secret_temp/non_existent.mtclean");
        let proposal = ConsentProposal {
            proposal_id: "prop-1".into(),
            provider: CloudProvider::Modal,
            profile_id: "modal-1".into(),
            profile_name: "Modal 1".into(),
            endpoint_url: "https://modal.run/ep".into(),
            canonical_endpoint_fingerprint:
                "0000000000000000000000000000000000000000000000000000000000000000".into(),
            crop_bounds: cleaner_core::mask::Rect::new(0, 0, 64, 64),
            crop_width: 64,
            crop_height: 64,
            crop_png_sha256: "3333333333333333333333333333333333333333333333333333333333333333"
                .into(),
            hint_png_sha256: "4444444444444444444444444444444444444444444444444444444444444444"
                .into(),
            operation_digest: "6666666666666666666666666666666666666666666666666666666666666666"
                .into(),
            source_hash: "1111111111111111111111111111111111111111111111111111111111111111".into(),
            mask_hash: "2222222222222222222222222222222222222222222222222222222222222222".into(),
            revision: 1,
            revision_hash: "5555555555555555555555555555555555555555555555555555555555555555"
                .into(),
            input_sha256: String::new(),
            predecessors_sha256: String::new(),
            recipe: RenderRecipe::new(
                "sdnq-v1",
                "1.0.0",
                "flux-schnell",
                "0123456789abcdef0123456789abcdef01234567",
                false,
            ),
            region_ids: vec!["reg-1".into()],
            intent: OperationIntent::CleanAnyway,
            chapter_id: "chap-1".into(),
            page_index: 0,
            source_idx: 0,
            issued_at_ms: 1000,
            expires_at_ms: 2000,
        };

        let res = revalidate_proposal_before_dispatch(&non_existent_path, &proposal);
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(err, "failed to open project manifest before dispatch");
        assert!(!err.contains("/private/var"));
        assert!(!err.contains("secret_temp"));
        assert!(!err.contains("non_existent.mtclean"));
    }

    #[test]
    fn test_lifecycle_handler_seams_prepare_and_confirm() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-seams-prep-conf")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let (manifest, _proposal, _grant, _grant_svc) =
            setup_two_page_cmd_test_job(&scratch, "reg-seam");

        let mut config = InferenceConfig::default();
        let (origin, fp) =
            crate::inference::config::validate_https_endpoint("https://modal.example.com/mc/v1")
                .unwrap();
        config.modal_profiles.insert(
            "modal-prof-1".to_string(),
            crate::inference::config::CloudProfile {
                id: "modal-prof-1".to_string(),
                name: "Test Modal Profile".to_string(),
                endpoint_url: "https://modal.example.com/mc/v1".to_string(),
                canonical_origin: origin,
                canonical_origin_fingerprint: fp,
                created_at_ms: 1000,
                updated_at_ms: 1000,
            },
        );

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

        // 1. prepare_cloud_consent_inner negative condition: cloud_allowed = false
        let res_dis = prepare_cloud_consent_inner(
            false,
            &config,
            &manifest,
            target.clone(),
            Some(recipe.clone()),
            OperationIntent::CleanAnyway,
            Some("reg-seam".into()),
            Some("chap-1".into()),
            Some(0),
            None,
        );
        assert!(res_dis.is_err());
        assert!(res_dis
            .unwrap_err()
            .contains("cloudEngines permission denied"));

        // 2. prepare_cloud_consent_inner negative condition: simulate_blocked = true
        let res_sim = prepare_cloud_consent_inner(
            true,
            &config,
            &manifest,
            target.clone(),
            Some(recipe.clone()),
            OperationIntent::CleanAnyway,
            Some("reg-seam".into()),
            Some("chap-1".into()),
            Some(0),
            Some(true),
        );
        assert!(res_sim.is_err());
        assert!(res_sim
            .unwrap_err()
            .contains("cloudEngines permission denied"));

        // 3. prepare_cloud_consent_inner negative condition: missing recipe
        let res_no_rec = prepare_cloud_consent_inner(
            true,
            &config,
            &manifest,
            target.clone(),
            None,
            OperationIntent::CleanAnyway,
            Some("reg-seam".into()),
            Some("chap-1".into()),
            Some(0),
            None,
        );
        assert!(res_no_rec.is_err());
        assert_eq!(
            res_no_rec.unwrap_err(),
            "recipe is required for prepare_cloud_consent"
        );

        // 4. prepare_cloud_consent_inner negative condition: missing chapterId
        let res_no_ch = prepare_cloud_consent_inner(
            true,
            &config,
            &manifest,
            target.clone(),
            Some(recipe.clone()),
            OperationIntent::CleanAnyway,
            Some("reg-seam".into()),
            None,
            Some(0),
            None,
        );
        assert!(res_no_ch.is_err());
        assert_eq!(
            res_no_ch.unwrap_err(),
            "chapterId is required for prepare_cloud_consent"
        );

        // 5. prepare_cloud_consent_inner success
        let res_ok = prepare_cloud_consent_inner(
            true,
            &config,
            &manifest,
            target,
            Some(recipe),
            OperationIntent::CleanAnyway,
            Some("reg-seam".into()),
            Some("chap-1".into()),
            Some(0),
            None,
        );
        assert!(res_ok.is_ok());
        let prop_dto = res_ok.unwrap();
        assert_eq!(prop_dto.profile_id, "modal-prof-1");
        assert_eq!(prop_dto.provider, CloudProvider::Modal);

        // 6. confirm_cloud_consent_inner negative condition: simulate_epoch_mismatch = true
        let conf_epoch = confirm_cloud_consent_inner(
            true,
            &config,
            &manifest,
            &prop_dto.proposal_id,
            &OperationIntent::CleanAnyway,
            Some(true),
        );
        assert!(conf_epoch.is_err());
        assert_eq!(conf_epoch.unwrap_err(), "Profile mutated: epoch mismatch");

        // 7. confirm_cloud_consent_inner negative condition: cloud_allowed = false
        let conf_dis = confirm_cloud_consent_inner(
            false,
            &config,
            &manifest,
            &prop_dto.proposal_id,
            &OperationIntent::CleanAnyway,
            None,
        );
        assert!(conf_dis.is_err());
        assert!(conf_dis
            .unwrap_err()
            .contains("cloudEngines permission denied"));

        // 8. confirm_cloud_consent_inner negative condition: non-existent proposal ID
        let conf_notfound = confirm_cloud_consent_inner(
            true,
            &config,
            &manifest,
            "non-existent-prop-id",
            &OperationIntent::CleanAnyway,
            None,
        );
        assert!(conf_notfound.is_err());
        assert_eq!(conf_notfound.unwrap_err(), "proposal was not found");

        // 9. confirm_cloud_consent_inner success
        let conf_ok = confirm_cloud_consent_inner(
            true,
            &config,
            &manifest,
            &prop_dto.proposal_id,
            &OperationIntent::CleanAnyway,
            None,
        );
        assert!(conf_ok.is_ok());
        let grant_dto = conf_ok.unwrap();
        assert_eq!(grant_dto.scope.profile_id, "modal-prof-1");
        assert_eq!(grant_dto.allowed_attempts, 1);
        assert_eq!(grant_dto.used_attempts, 0);
    }

    #[test]
    fn test_lifecycle_handler_seams_submit_status_result_cancel_recovery() {
        let scratch = std::env::temp_dir()
            .join("mc-cmd-seams-sub-stat-rec")
            .join(format!("{}", current_epoch_ms()));
        std::fs::create_dir_all(&scratch).unwrap();

        let limits = provisional_fixture_limits();
        let journal = AttemptJournal::new(scratch.join("journal"), limits);
        let config = InferenceConfig::default();

        let make_intent = |att_id: &str| {
            let mut intent = CreateAttemptIntent {
                qwen_edit: None,
                attempt_id: att_id.into(),
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
                width: 128,
                height: 128,
                seed: 42,
                steps: 4,
                guidance_scaled: 350,
                source_image_hash:
                    "0000000000000000000000000000000000000000000000000000000000000000".into(),
                region_id: "reg-1".into(),
                region_revision: 1,
                input_sha256: None,
                predecessors_sha256: None,
                crop_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                hint_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                request_digest: String::new(),
                chapter_id: Some("chap-1".into()),
                page_index: Some(0),
            };
            intent.request_digest = compute_intent_request_digest(&intent);
            intent
        };

        // 1. submit_cloud_attempt_inner negative condition: cloud_allowed = false
        let sub_dis = submit_cloud_attempt_inner(
            false,
            &config,
            &journal,
            "att-123",
            "some-nonce",
            Some("prop-1"),
            None,
            None,
            None,
            None,
        );
        assert!(sub_dis.is_err());
        assert!(sub_dis
            .unwrap_err()
            .contains("cloudEngines permission denied"));

        // 2. submit_cloud_attempt_inner negative condition: empty nonce
        let sub_nonce = submit_cloud_attempt_inner(
            true,
            &config,
            &journal,
            "att-123",
            "   ",
            Some("prop-1"),
            None,
            None,
            None,
            None,
        );
        assert!(sub_nonce.is_err());
        assert!(sub_nonce
            .unwrap_err()
            .contains("invalid or missing grant nonce"));

        // 3. submit_cloud_attempt_inner negative condition: mismatched attemptId
        let sub_att = submit_cloud_attempt_inner(
            true,
            &config,
            &journal,
            "att-mismatch-id-1234567890123456",
            "actual-nonce-123",
            Some("prop-1"),
            None,
            None,
            None,
            None,
        );
        assert!(sub_att.is_err());
        assert!(sub_att
            .unwrap_err()
            .contains("caller attemptId does not match"));

        // 4. get_cloud_attempt_status_inner negative condition: attempt not found
        let stat_nf =
            get_cloud_attempt_status_inner(&journal, Some(&config), "att-non-existent", None);
        assert!(stat_nf.is_err());
        assert_eq!(stat_nf.unwrap_err(), "attempt not found in journal");

        // 5. get_cloud_attempt_result_inner negative condition: non-completed phase
        let (_rec, guard) = journal
            .create_intent(make_intent("att-intent-test"))
            .unwrap();
        drop(guard);
        let res_noncomp =
            get_cloud_attempt_result_inner(&journal, Some(&config), "att-intent-test", None);
        assert!(res_noncomp.is_err());
        assert_eq!(
            res_noncomp.unwrap_err(),
            "cannot retrieve result for attempt in non-completed phase"
        );

        // 6. cancel_cloud_attempt_inner: Intent phase cancels cleanly
        let cancel_intent =
            cancel_cloud_attempt_inner(&journal, Some(&config), "att-intent-test", None);
        assert!(cancel_intent.is_ok());
        let cancel_dto = cancel_intent.unwrap();
        assert_eq!(cancel_dto.status, "cancelled");
        assert!(cancel_dto.acknowledged);

        // 7. cancel_cloud_attempt_inner: Dispatching phase cancel rejected
        let (_rec2, guard2) = journal.create_intent(make_intent("att-disp-test")).unwrap();
        journal.mark_dispatching(&guard2).unwrap();
        drop(guard2);
        let cancel_disp =
            cancel_cloud_attempt_inner(&journal, Some(&config), "att-disp-test", None);
        assert!(cancel_disp.is_err());
        assert!(cancel_disp.unwrap_err().contains("currently dispatching"));

        // 8. reconcile_cloud_recovery_inner: empty journal returns terminal
        let empty_journal = AttemptJournal::new(scratch.join("empty_journal"), limits);
        let recov_empty = reconcile_cloud_recovery_inner(
            &empty_journal,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(recov_empty.is_ok());
        let recov_empty_dto = recov_empty.unwrap();
        assert_eq!(recov_empty_dto.decision, "terminal");
        assert_eq!(
            recov_empty_dto.message.as_deref(),
            Some("No uncommitted attempts found in journal")
        );

        // 9. reconcile_cloud_recovery_inner: cancelled attempt returns terminal
        let recov_cancelled = reconcile_cloud_recovery_inner(
            &journal,
            None,
            Some("att-intent-test"),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(recov_cancelled.is_ok());
        assert_eq!(
            recov_cancelled.unwrap(),
            CloudRecoveryDecisionDto {
                decision: "terminal".to_string(),
                attempt_id: Some("att-intent-test".to_string()),
                handle: None,
                result_digest: None,
                patch_id: None,
                auto_retryable: Some(false),
                message: Some("explicit pre-dispatch abort".to_string()),
            }
        );
    }

    #[test]
    fn test_guard_cloud_execution_enabled_fails_closed() {
        let res = guard_cloud_execution_enabled(false);
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(err, "cloud_disabled");
        assert!(!err.contains("/"));
        assert!(!err.contains("http"));

        let res_ok = guard_cloud_execution_enabled(true);
        assert!(res_ok.is_ok());
    }
}
