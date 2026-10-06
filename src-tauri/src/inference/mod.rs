//! Cloud inference security, configuration, credential management, and grant policy.

pub mod commands;
pub mod cloud_clean;
pub mod cloud_denoise;
pub mod analysis;
pub mod config;
pub mod consent;
pub mod gpu;
pub mod gpu_jobs;
pub mod http;
pub mod journal;
pub mod policy;
pub mod run_analysis;
pub mod secrets;
pub mod service;

pub use config::{
    compute_canonical_endpoint_fingerprint, validate_https_endpoint, validate_inference_config,
    validate_profile_id, BeamProfile, CloudProfile, InferenceConfig, ModalProfile,
    CURRENT_SCHEMA_VERSION, INFERENCE_CONFIG_FILE, MAX_ENDPOINT_URL_LEN, MAX_PROFILES_PER_PROVIDER,
    MAX_PROFILE_ID_LEN, MAX_PROFILE_NAME_LEN,
};
pub use consent::{
    compute_region_revision_fingerprint, compute_region_revision_hash, ConsentError,
    ConsentProposal, ConsentService, OperationIntent, PrepareProposalRequest,
    DEFAULT_PROPOSAL_TTL, MAX_CACHED_PROPOSALS, MAX_PROPOSAL_TTL, MIN_PROPOSAL_TTL,
};
pub use http::{
    BoundRuntimeCredential, CloudEndpointTarget, CloudHttpClient, HttpTransportError,
    RuntimeCredential,
};
pub use journal::{
    AttemptJournal, AttemptLockGuard, AttemptPhase, AttemptRecord, CreateAttemptIntent,
    JournalError, ProjectRegionSnapshot, RecoveryDecision,
};
pub use policy::{Grant, GrantError, GrantScope, GrantService};
pub use secrets::{
    MemorySecretStore, OsKeyringSecretStore, SecretKey, SecretManager, SecretRole, SecretStore,
    SecretSummary, SecretValue, StorageBackendKind, KEYRING_SERVICE,
};
pub use service::{InferenceService, InferenceServiceError, PollOptions};

/// Providers this version will not connect to. Beam is paused: no setup, no
/// helper run and no endpoint traffic (`CloudHttpClient::new` refuses it), while
/// its code stays for when it returns. The interface marks it Paused.
pub fn provider_paused(provider: cleaner_core::engines::render::CloudProvider) -> bool {
    matches!(provider, cleaner_core::engines::render::CloudProvider::Beam)
}

/// The settings key of the user's cloud permission switch.
pub const CLOUD_ENGINES_KEY: &str = "cloudEngines";

/// The user's cloud permission switch: `cloudEngines` in the settings file.
///
/// Anything but an explicit `"allowed"` is off, including a key that was never
/// written and a settings file that cannot be read, so paid work fails closed.
/// Every command that can spend reads the switch through this one function.
pub fn cloud_allowed(app: &tauri::AppHandle) -> bool {
    crate::settings::read(app)
        .ok()
        .and_then(|settings| {
            settings
                .get(CLOUD_ENGINES_KEY)
                .and_then(|value| value.as_str())
                .map(|value| value == "allowed")
        })
        .unwrap_or(false)
}

/// Why work already started on a profile may not go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Withdrawn {
    /// The cloud permission switch is off.
    CloudDisabled,
    /// The profile is no longer configured, or the configuration cannot be read.
    ProfileGone,
}

/// Whether work already started on `provider`/`profile_id` may go on: cloud
/// engines on and the profile still configured.
///
/// Which profile is *selected* is not asked. Selection picks where new work
/// goes; started work keeps the profile its grant was given for, so picking
/// another default mid-run leaves the run going. An edited or removed profile
/// is caught here and by its grant epoch, which every config write that moves
/// the profile advances.
pub(crate) fn profile_in_service(
    app: &tauri::AppHandle,
    provider: cleaner_core::engines::render::CloudProvider,
    profile_id: &str,
) -> Result<(), Withdrawn> {
    use cleaner_core::engines::render::CloudProvider;
    if !cloud_allowed(app) {
        return Err(Withdrawn::CloudDisabled);
    }
    let config = config::read_inference_config(app).map_err(|_| Withdrawn::ProfileGone)?;
    let configured = match provider {
        CloudProvider::Beam => config.beam_profiles.contains_key(profile_id),
        CloudProvider::Modal => config.modal_profiles.contains_key(profile_id),
    };
    if configured { Ok(()) } else { Err(Withdrawn::ProfileGone) }
}

pub mod review;
