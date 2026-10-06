//! Backend-only authorization consent service and proposal preparation.
//!
//! Provides the authoritative two-step backend authorization flow for remote cloud inference:
//! 1. **Prepare Proposal (`prepare_proposal`):** Under the chapter writer lock [`crate::run::lock_job`], reads
//!    REAL local source rasters and existing region geometry from disk, evaluates [`PreparedRender::prepare`],
//!    encodes and hashes exact crop and hint PNG buffers ([`cleaner_core::ingest::sha256_hex`]),
//!    derives deterministic backend content fingerprints for region revision, resolves validated
//!    public profile endpoints from [`InferenceConfig`], and validates pinned wire recipes.
//!    Caller CANNOT supply hashes or assert region revisions. Proposals expire with bounded TTL,
//!    are capped in a bounded in-memory cache, and avoid full-page memory residency.
//! 2. **Confirm Proposal (`confirm_proposal`):** Validates unexpired, unconsumed proposals upon
//!    explicit transmission/spend confirmation. Re-locks and re-prepares exact crop and hint
//!    rasters to revalidate region geometry, source raster, mask bits, ink bits, config origin
//!    fingerprint, backend permission, and operation intent before issuing an attempt-limited
//!    [`Grant`] via [`GrantService`].
//!
//! ## Invariants
//!
//! 1. **No Client-Asserted Hashes:** The caller provides only identifiers, execution target,
//!    recipe, and operation intent. All SHA-256 digests (`source_hash`, `mask_hash`, `crop_sha256`,
//!    `hint_sha256`, `operation_digest`) and revision fingerprints are computed directly from real disk assets.
//! 2. **Backend Permission & Profile Verification:** Preparation and confirmation strictly enforce
//!    the backend `cloudAllowed` permission (`cloudEngines == "allowed"`) and ensure the selected
//!    profile exists with HTTPS hygiene.
//! 3. **Pinned Wire Recipe Validation:** Pinned recipes are verified via [`cleaner_core::cloud_wire::validate_recipe`];
//!    no fabricated defaults or placeholders are introduced.
//! 4. **No Remote Network or Model Warming on Prepare:** Preparation is entirely local raster
//!    extraction and digest computation; zero remote HTTP requests, DNS queries, or model warmups occur.
//! 5. **Operation Intent Binding:** Binds operation intent (`apply`, `create`, `rerun`, `clean`) and
//!    parameters via a structured SHA-256 operation digest to prevent cross-path replay attacks.
//! 6. **Full Identity Re-preparation on Confirm:** Confirm re-runs crop extraction and re-verifies
//!    crop and hint hashes, mask bits, ink bits, source hash, and revision before minting a grant.
//! 7. **Single-Use Consumption & Revocation Hooks:** Confirming a proposal atomically consumes it.
//!    Credential and profile updates revoke pending proposals alongside issued grants.
//! 8. **Zero Plaintext Secrets & Zero Full-Page Residency:** Proposals serialize only public digests
//!    and bounds. Cached proposals hold bounded descriptors and digests, never full-page [`Raster`]s.
//! 9. **Static Safe Errors:** Error variants never echo arbitrary operation JSON payload data or raw
//!    filesystem paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cleaner_core::engines::render::{CloudProvider, ExecutionTarget, Preprocessing, RenderRecipe};
use cleaner_core::image::{BitDepth, ColorMode, Format, Raster};
use cleaner_core::ingest::sha256_hex;
use cleaner_core::mask::Rect;
use cleaner_core::project::{DetectedRegion, Job, PatchRecord};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::inference::config::{compute_canonical_endpoint_fingerprint, InferenceConfig};
use crate::inference::policy::{Grant, GrantError, GrantScope, GrantService};
use crate::library::Library;
use crate::run;

/// Default lifetime for a consent proposal (5 minutes).
pub const DEFAULT_PROPOSAL_TTL: Duration = Duration::from_secs(300);

/// Maximum allowable lifetime for a consent proposal (10 minutes).
pub const MAX_PROPOSAL_TTL: Duration = Duration::from_secs(600);

/// Minimum allowable lifetime for a consent proposal (1 second).
pub const MIN_PROPOSAL_TTL: Duration = Duration::from_secs(1);

/// Maximum number of cached consent proposals held in memory.
pub const MAX_CACHED_PROPOSALS: usize = 256;

/// Maximum serialized size for operation parameter payloads (64 KiB).
pub const MAX_OPERATION_PARAMS_LEN: usize = 65536;

/// Maximum allowable length for tool names.
pub const MAX_TOOL_NAME_LEN: usize = 64;

/// Maximum allowable length for mask IDs.
pub const MAX_MASK_ID_LEN: usize = 128;

/// Maximum allowable length for rerun kind strings.
pub const MAX_KIND_NAME_LEN: usize = 64;

/// The specific user/tool operation intent being authorized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum OperationIntent {
    ApplyTool {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
    },
    CreateRegion {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
    },
    RerunMask {
        mask_id: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<String>,
    },
    CleanAnyway,
    /// One region of a batch cloud clean of stored detections
    /// (`docs/detect-clean.md` §3). The batch's own grant stands behind it; the
    /// per-region grant each region spends is bound to this.
    CloudClean,
}

/// Request parameters to prepare an authorization consent proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareProposalRequest {
    pub chapter_id: String,
    pub page_index: u32,
    pub region_id: Option<String>,
    pub target: ExecutionTarget,
    pub recipe: RenderRecipe,
    pub intent: OperationIntent,
}

/// Options for confirming an authorization proposal and issuing an authorization grant.
#[derive(Debug, Clone)]
pub struct ConfirmProposalOptions<'a> {
    pub proposal_id: &'a str,
    pub intent: &'a OperationIntent,
    pub config: &'a InferenceConfig,
    pub cloud_allowed: bool,
    pub job_path: &'a Path,
    pub grant_ttl: Duration,
    pub max_attempts: u32,
}

/// Immutable proposal summary describing the exact scope, assets, and endpoint.
///
/// Contains ONLY public identifiers, digests, dimensions, and recipes.
/// Never contains private credentials, API keys, or full raster pixel buffers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConsentProposal {
    pub proposal_id: String,
    pub provider: CloudProvider,
    pub profile_id: String,
    pub profile_name: String,
    pub endpoint_url: String,
    pub canonical_endpoint_fingerprint: String,
    pub crop_bounds: Rect,
    pub crop_width: u32,
    pub crop_height: u32,
    pub crop_png_sha256: String,
    pub hint_png_sha256: String,
    pub operation_digest: String,
    pub source_hash: String,
    pub mask_hash: String,
    pub revision: u64,
    pub revision_hash: String,
    #[serde(default)]
    pub input_sha256: String,
    #[serde(default)]
    pub predecessors_sha256: String,
    pub recipe: RenderRecipe,
    pub region_ids: Vec<String>,
    pub intent: OperationIntent,
    pub chapter_id: String,
    pub page_index: u32,
    pub source_idx: usize,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

/// Internal record stored in `ConsentService` cache.
#[derive(Debug, Clone)]
struct StoredProposal {
    proposal: ConsentProposal,
    canonical_job_path: PathBuf,
    profile_epoch: u64,
    confirmed_grant_nonce: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConsentError {
    #[error("cloud execution is disabled in application settings")]
    CloudDisabled,

    #[error("execution target is local; consent is only required for remote execution")]
    LocalTargetNotAllowed,

    #[error("profile not found for requested provider")]
    ProfileNotFound(String, CloudProvider),

    #[error("invalid endpoint URL: {0}")]
    InvalidEndpoint(String),

    #[error("invalid render recipe: {0}")]
    InvalidRecipe(String),

    #[error("page index not found in chapter")]
    PageNotFound(u32, String),

    #[error("requested region was not found on specified page")]
    RegionNotFound(String),

    #[error("region source image is missing or unreadable")]
    SourceImageError,

    #[error("region geometry declined or invalid for rendering: {0}")]
    RenderPrepareFailed(String),

    /// The region's crop is past the render service's limits even with no
    /// page around it ([`CloudRegion::sendable_crop`]); the service would
    /// refuse it, so no proposal is made.
    #[error("cloud_consent_crop_too_large: the region's crop is past the render service limits")]
    CropTooLarge,

    #[error("proposal was not found")]
    ProposalNotFound,

    #[error("proposal expired at {expired_at_ms} ms (current time {now_ms} ms)")]
    ProposalExpired { expired_at_ms: u64, now_ms: u64 },

    #[error("proposal has already been confirmed or consumed")]
    ProposalAlreadyConsumed,

    #[error("proposal has not been confirmed")]
    ProposalNotConfirmed,

    #[error("grant nonce does not match confirmed proposal binding")]
    GrantNonceMismatch,

    #[error("system clock rollback detected")]
    ClockRollbackDetected,

    #[error("operation intent mismatch: requested operation does not match prepared proposal")]
    IntentMismatch,

    #[error("operation parameters exceed maximum allowable length")]
    OperationParamsTooLarge,

    #[error("invalid operation parameter: {0}")]
    InvalidParameter(&'static str),

    #[error("region revision mismatch: content has been modified since proposal was prepared")]
    RevisionMismatch,

    #[error("crop or hint raster digest mismatch during revalidation")]
    CropDigestMismatch,

    #[error("source image digest mismatch: image has changed since proposal was prepared")]
    SourceMismatch,

    #[error("mask digest mismatch: mask has changed since proposal was prepared")]
    MaskMismatch,

    #[error("page or job boundary mismatch between request and stored proposal")]
    PageMismatch,

    #[error("configuration or endpoint mismatch since proposal was prepared")]
    ConfigMismatch,

    #[error("profile or credentials were modified since proposal was prepared")]
    ProfileMutated,

    #[error("grant issuance failed: {0}")]
    GrantIssuanceError(#[from] GrantError),

    #[error("unsupported new region geometry: {0}")]
    UnsupportedNewRegion(String),

    #[error("system random source error")]
    RandomSourceError,

    #[error("storage or locking error: {0}")]
    Internal(String),
}

/// Helper to get current epoch milliseconds.
fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

/// Generate a 32-byte (256-bit) cryptographically random hex proposal ID.
fn generate_proposal_id() -> Result<String, ConsentError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| ConsentError::RandomSourceError)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Validate size and parameter bounds of an [`OperationIntent`].
pub fn validate_operation_intent_bounds(intent: &OperationIntent) -> Result<(), ConsentError> {
    match intent {
        OperationIntent::ApplyTool { tool, params } => {
            if tool.is_empty() || tool.len() > MAX_TOOL_NAME_LEN {
                return Err(ConsentError::InvalidParameter("tool name length invalid"));
            }
            if let Some(p) = params {
                let s = p.to_string();
                if s.len() > MAX_OPERATION_PARAMS_LEN {
                    return Err(ConsentError::OperationParamsTooLarge);
                }
            }
        }
        OperationIntent::CreateRegion { tool, params } => {
            if tool.is_empty() || tool.len() > MAX_TOOL_NAME_LEN {
                return Err(ConsentError::InvalidParameter("tool name length invalid"));
            }
            if let Some(p) = params {
                let s = p.to_string();
                if s.len() > MAX_OPERATION_PARAMS_LEN {
                    return Err(ConsentError::OperationParamsTooLarge);
                }
            }
        }
        OperationIntent::RerunMask {
            mask_id,
            kind,
            engine,
        } => {
            if mask_id.is_empty() || mask_id.len() > MAX_MASK_ID_LEN {
                return Err(ConsentError::InvalidParameter("mask_id length invalid"));
            }
            if kind.is_empty() || kind.len() > MAX_KIND_NAME_LEN {
                return Err(ConsentError::InvalidParameter("kind length invalid"));
            }
            if let Some(eng) = engine {
                if eng.is_empty() || eng.len() > MAX_TOOL_NAME_LEN {
                    return Err(ConsentError::InvalidParameter("engine length invalid"));
                }
            }
        }
        OperationIntent::CleanAnyway | OperationIntent::CloudClean => {}
    }
    Ok(())
}

/// Compute a canonical SHA-256 operation digest over an [`OperationIntent`] with a structured domain separator.
pub fn compute_operation_digest(intent: &OperationIntent) -> Result<String, ConsentError> {
    validate_operation_intent_bounds(intent)?;

    let mut hasher = Sha256::new();
    hasher.update(b"domain:operation_intent_v1:");
    let json_bytes = serde_json::to_vec(intent).map_err(|e| {
        ConsentError::Internal(format!("failed to serialize operation intent: {e}"))
    })?;
    hasher.update(&json_bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute a deterministic 64-bit revision fingerprint and 64-char hex hash over the stored region state.
///
/// Hashes full [`PatchRecord`], mask bounds & bits, and ink bounds & bits.
pub fn compute_region_revision_hash(
    source_sha256: &str,
    record: &PatchRecord,
    mask: &cleaner_core::mask::Mask,
    ink: &cleaner_core::mask::Mask,
) -> (u64, String) {
    let mut hasher = Sha256::new();
    hasher.update(b"region_revision_v1:");
    hasher.update(source_sha256.as_bytes());
    hasher.update(b"\0");
    // Structured fields bind the complete record without delimiter ambiguity.
    hasher.update(serde_json::to_vec(record).expect("patch record serialization"));
    hasher.update(b"\0");
    hasher.update(mask.bounds.x.to_le_bytes());
    hasher.update(mask.bounds.y.to_le_bytes());
    hasher.update(mask.bounds.w.to_le_bytes());
    hasher.update(mask.bounds.h.to_le_bytes());
    hasher.update(&mask.bits);
    hasher.update(b"\0");
    hasher.update(ink.bounds.x.to_le_bytes());
    hasher.update(ink.bounds.y.to_le_bytes());
    hasher.update(ink.bounds.w.to_le_bytes());
    hasher.update(ink.bounds.h.to_le_bytes());
    hasher.update(&ink.bits);

    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let mut num_bytes = [0u8; 8];
    num_bytes.copy_from_slice(&digest[0..8]);
    let u64_fp = u64::from_be_bytes(num_bytes);
    (u64_fp, hex)
}

/// [`compute_region_revision_hash`] for a stored detection: the source, the
/// whole [`DetectedRegion`] record and both of its masks. A domain of its own,
/// so a detection and the patch that later carries its id can never hash alike.
pub fn compute_detection_revision_hash(
    source_sha256: &str,
    record: &DetectedRegion,
    mask: &cleaner_core::mask::Mask,
    ink: &cleaner_core::mask::Mask,
) -> (u64, String) {
    let mut hasher = Sha256::new();
    hasher.update(b"detection_revision_v1:");
    hasher.update(source_sha256.as_bytes());
    hasher.update(b"\0");
    hasher.update(serde_json::to_vec(record).expect("detection record serialization"));
    for mask in [mask, ink] {
        hasher.update(b"\0");
        hasher.update(mask.bounds.x.to_le_bytes());
        hasher.update(mask.bounds.y.to_le_bytes());
        hasher.update(mask.bounds.w.to_le_bytes());
        hasher.update(mask.bounds.h.to_le_bytes());
        hasher.update(&mask.bits);
    }
    let digest = hasher.finalize();
    let mut num_bytes = [0u8; 8];
    num_bytes.copy_from_slice(&digest[0..8]);
    (u64::from_be_bytes(num_bytes), format!("{digest:x}"))
}

/// What a cloud render of one stored region is bound to and prepared from.
///
/// A region is a patch, or a stored detection that has no pixels yet and is
/// cleaned in the cloud from the mask it stored (`docs/detect-clean.md` §3).
/// Every step of a render - consent, the check before dispatch, the grant the
/// dispatch spends and the attachment - reads the region through
/// [`load_cloud_region`], so the two kinds are told apart in one place and a
/// render never binds one kind and attaches to the other.
pub(crate) struct CloudRegion {
    pub source_idx: usize,
    /// The region's place in page order: the underlay below it is read up to
    /// here, and a detection's patch keeps it.
    pub order: u32,
    pub mask: cleaner_core::mask::Mask,
    pub ink: cleaner_core::mask::Mask,
    pub revision: u64,
    pub revision_hash: String,
    pub kind: CloudRegionKind,
}

pub(crate) enum CloudRegionKind {
    /// A patch, re-rendered in place. Boxed: a patch carries its pixels.
    Patch(Box<(PatchRecord, cleaner_core::patch::Patch)>),
    /// A stored detection, replaced by the patch its render makes. Boxed to
    /// keep the two variants of a size.
    Detection(Box<DetectedRegion>),
}

impl CloudRegion {
    /// The cloud crop, hint and underlay of this region on `page`, prepared
    /// the way `preprocessing` does.
    pub(crate) fn prepare(
        &self,
        job: &Job,
        page: &Raster,
        preprocessing: Preprocessing,
    ) -> Result<crate::underlay::CloudInput, String> {
        match &self.kind {
            CloudRegionKind::Patch(stored) => crate::underlay::prepare_cloud(
                job,
                self.source_idx,
                page,
                &stored.0,
                &stored.1,
                preprocessing,
            ),
            CloudRegionKind::Detection(_) => crate::underlay::prepare_cloud_seed(
                job,
                self.source_idx,
                page,
                self.order,
                self.hole(preprocessing),
                preprocessing,
            ),
        }
    }

    /// The mask a render of this region under `preprocessing` removes: the one
    /// the preparation, the consent digest and the cost estimate all read, so
    /// none of them can name a different mask from the render.
    pub(crate) fn hole(&self, preprocessing: Preprocessing) -> &cleaner_core::mask::Mask {
        preprocessing.hole(&self.mask, &self.ink)
    }

    /// The digest a proposal, its grant and the dispatch bind this region's
    /// mask by. V1 bound the stored mask's bits, and keeps doing so; later
    /// versions bind the hole the render removes, bounds included, in a domain
    /// of their own.
    pub(crate) fn mask_digest(&self, preprocessing: Preprocessing) -> String {
        match preprocessing {
            Preprocessing::V1 => sha256_hex(&self.mask.bits),
            _ => {
                let hole = self.hole(preprocessing);
                let mut hasher = Sha256::new();
                hasher.update(b"cloud_hole:");
                hasher.update(preprocessing.version().as_bytes());
                hasher.update(b"\0");
                hasher.update(hole.bounds.x.to_le_bytes());
                hasher.update(hole.bounds.y.to_le_bytes());
                hasher.update(hole.bounds.w.to_le_bytes());
                hasher.update(hole.bounds.h.to_le_bytes());
                hasher.update(&hole.bits);
                format!("{:x}", hasher.finalize())
            }
        }
    }

    /// The crop a render under `preprocessing` will send from a `page_w` by
    /// `page_h` page, for an estimate made without reading the page: the
    /// preparation's own geometry ([`Preprocessing::crop_of_hole`]), so a hole
    /// at the page's right or bottom edge is clipped there as the render clips
    /// it. `None` for a crop no page could hold.
    pub(crate) fn estimated_crop(&self, preprocessing: Preprocessing, page_w: u32, page_h: u32) -> Option<Rect> {
        preprocessing.crop_of_hole(self.hole(preprocessing).bounds, page_w, page_h)
    }

    /// [`CloudRegion::estimated_crop`] when the render service takes it
    /// ([`cleaner_core::engines::render::within_service_limits`]), `None` when
    /// it would refuse it. The one test a clean plan (`cloud_clean::bind`)
    /// and a single region's proposal both hold a region to before it is
    /// priced or consented to.
    pub(crate) fn sendable_crop(&self, preprocessing: Preprocessing, page_w: u32, page_h: u32) -> Option<Rect> {
        self.estimated_crop(preprocessing, page_w, page_h)
            .filter(|crop| cleaner_core::engines::render::within_service_limits(*crop))
    }
}

/// Read the stored region `region_id` for a cloud render, with its revision
/// against `source_sha256`. A patch is looked for first, because a detection
/// that was cleaned leaves a patch under its own id.
///
/// Refuses what the legacy cloud path cannot write: a text-shaped patch or a
/// region with a prepared text-shape plan.
pub(crate) fn load_cloud_region(
    job: &Job,
    region_id: &str,
    source_sha256: &str,
) -> Result<CloudRegion, ConsentError> {
    if job
        .project
        .text_shape_plans
        .iter()
        .any(|plan| plan.region_id == region_id)
    {
        return Err(ConsentError::RenderPrepareFailed(
            "cloud renders do not support text-shaped write plans".into(),
        ));
    }
    if let Some(record) = job.project.patches.iter().find(|r| r.id == region_id) {
        if record.geometry_policy == cleaner_core::text_shape::GeometryPolicy::TextShape {
            return Err(ConsentError::RenderPrepareFailed(
                "cloud renders do not support text-shaped write plans".into(),
            ));
        }
        let patch = job
            .load_display_patch(record)
            .map_err(|e| ConsentError::Internal(e.to_string()))?;
        let (revision, revision_hash) =
            compute_region_revision_hash(source_sha256, record, &patch.mask, &patch.ink);
        return Ok(CloudRegion {
            source_idx: record.source_idx,
            order: record.order,
            mask: patch.mask.clone(),
            ink: patch.ink.clone(),
            revision,
            revision_hash,
            kind: CloudRegionKind::Patch(Box::new((record.clone(), patch))),
        });
    }
    let loaded = job
        .load_detection(region_id)
        .map_err(|e| ConsentError::Internal(e.to_string()))?
        .ok_or_else(|| ConsentError::RegionNotFound(region_id.to_string()))?;
    let (revision, revision_hash) =
        compute_detection_revision_hash(source_sha256, &loaded.record, &loaded.mask, &loaded.ink);
    Ok(CloudRegion {
        source_idx: loaded.record.source_idx,
        order: loaded.record.order,
        mask: loaded.mask,
        ink: loaded.ink,
        revision,
        revision_hash,
        kind: CloudRegionKind::Detection(Box::new(loaded.record)),
    })
}

/// Alias for [`compute_region_revision_hash`].
pub fn compute_region_revision_fingerprint(
    source_sha256: &str,
    record: &PatchRecord,
    mask: &cleaner_core::mask::Mask,
    ink: &cleaner_core::mask::Mask,
) -> (u64, String) {
    compute_region_revision_hash(source_sha256, record, mask, ink)
}

pub(crate) fn encode_rgb8_png(
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<Vec<u8>, ConsentError> {
    let raster = Raster {
        width,
        height,
        mode: ColorMode::Rgb,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: Some(1), color: Default::default(),
        data: data.to_vec(),
    };
    cleaner_core::image::encode(&raster, Format::Png)
        .map_err(|e| ConsentError::RenderPrepareFailed(e.to_string()))
}

pub(crate) fn encode_gray8_png(
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<Vec<u8>, ConsentError> {
    let raster = Raster {
        width,
        height,
        mode: ColorMode::Gray,
        depth: BitDepth::Eight,
        icc: None,
        palette: None,
        trns: None,
        srgb_intent: None, color: Default::default(),
        data: data.to_vec(),
    };
    cleaner_core::image::encode(&raster, Format::Png)
        .map_err(|e| ConsentError::RenderPrepareFailed(e.to_string()))
}

/// Thread-safe in-memory authorization consent authority.
pub struct ConsentService {
    proposals: Mutex<HashMap<String, StoredProposal>>,
    grant_service: Option<Arc<GrantService>>,
}

impl ConsentService {
    /// Create a ConsentService using the shared global [`GrantService`].
    pub fn new() -> Self {
        Self {
            proposals: Mutex::new(HashMap::new()),
            grant_service: None,
        }
    }

    /// Create an isolated ConsentService instance with a custom [`GrantService`] for tests.
    pub fn new_isolated(grant_service: Arc<GrantService>) -> Self {
        Self {
            proposals: Mutex::new(HashMap::new()),
            grant_service: Some(grant_service),
        }
    }

    /// Global shared consent authority.
    pub fn global() -> &'static ConsentService {
        static INSTANCE: OnceLock<ConsentService> = OnceLock::new();
        INSTANCE.get_or_init(ConsentService::new)
    }

    fn resolve_grant_service(&self) -> &GrantService {
        match &self.grant_service {
            Some(svc) => svc.as_ref(),
            None => GrantService::global(),
        }
    }

    /// Return the current mutation epoch for a profile.
    pub fn get_profile_epoch(&self, provider: CloudProvider, profile_id: &str) -> u64 {
        self.resolve_grant_service()
            .get_profile_epoch(provider, profile_id)
    }

    /// Atomically revoke all proposals, grants, and advance the profile epoch for a profile.
    pub fn invalidate_profile(&self, provider: CloudProvider, profile_id: &str) {
        self.resolve_grant_service()
            .invalidate_profile(provider, profile_id);
        drop(self.proposals.lock().unwrap_or_else(|p| p.into_inner()));
    }

    /// Atomically revoke all proposals, grants, and advance all profile epochs.
    pub fn invalidate_all(&self) {
        self.resolve_grant_service().invalidate_all();
        drop(self.proposals.lock().unwrap_or_else(|p| p.into_inner()));
    }

    /// Step 1: Prepare an immutable authorization proposal from real local source and region assets.
    pub fn prepare_proposal(
        &self,
        request: PrepareProposalRequest,
        config: &InferenceConfig,
        cloud_allowed: bool,
        job_path: &Path,
    ) -> Result<ConsentProposal, ConsentError> {
        if !cloud_allowed {
            return Err(ConsentError::CloudDisabled);
        }

        let provider = request
            .target
            .provider()
            .ok_or(ConsentError::LocalTargetNotAllowed)?;
        let profile_id = request
            .target
            .profile_id()
            .ok_or(ConsentError::LocalTargetNotAllowed)?;

        // 1. Capture profile mutation epoch BEFORE reading config or performing raster work
        let start_epoch = self.get_profile_epoch(provider, profile_id);

        let profile = match provider {
            CloudProvider::Beam => config.beam_profiles.get(profile_id),
            CloudProvider::Modal => config.modal_profiles.get(profile_id),
        }
        .ok_or_else(|| ConsentError::ProfileNotFound(profile_id.to_string(), provider))?;

        let canonical_endpoint_fingerprint =
            compute_canonical_endpoint_fingerprint(&profile.endpoint_url)
                .map_err(ConsentError::InvalidEndpoint)?;

        cleaner_core::cloud_wire::validate_recipe(&request.recipe.clone().into())
            .map_err(|e| ConsentError::InvalidRecipe(e.to_string()))?;
        let preprocessing = Preprocessing::of(&request.recipe.preprocessing_version)
            .map_err(|e| ConsentError::InvalidRecipe(e.to_string()))?;

        validate_operation_intent_bounds(&request.intent)?;
        let operation_digest = compute_operation_digest(&request.intent)?;

        let (
            crop_bounds,
            crop_png_sha256,
            hint_png_sha256,
            source_hash,
            mask_hash,
            revision,
            revision_hash,
            input_sha256,
            predecessors_sha256,
            source_idx,
            region_id,
        ) = {
            let _lock = run::lock_job(job_path).map_err(|e| ConsentError::Internal(e.to_string()))?;
            let job = Job::open(job_path).map_err(|e| ConsentError::Internal(e.to_string()))?;
            let source_idx = Library::resolve_page(&job.project, request.page_index as usize)
                .ok_or_else(|| {
                    ConsentError::PageNotFound(request.page_index, request.chapter_id.clone())
                })?;

            let source_path = job
                .source_path(source_idx)
                .ok_or(ConsentError::SourceImageError)?;
            let source_bytes =
                std::fs::read(&source_path).map_err(|_| ConsentError::SourceImageError)?;
            let source_hash = sha256_hex(&source_bytes);
            let page = job.display_page(source_idx,&source_bytes)
                .map_err(|e| ConsentError::RenderPrepareFailed(e.to_string()))?;

            let Some(region_id) = request.region_id else {
                return Err(ConsentError::UnsupportedNewRegion(
                    "ad-hoc manual geometry preparation for new region is unsupported; proposal requires existing stored region".to_string(),
                ));
            };

            let region = load_cloud_region(&job, &region_id, &source_hash)?;
            if region.source_idx != source_idx {
                return Err(ConsentError::PageMismatch);
            }
            if region.hole(preprocessing).is_empty() {
                return Err(ConsentError::RenderPrepareFailed(
                    "seed mask is empty".to_string(),
                ));
            }
            // A crop the render service would refuse is not proposed, as a
            // clean plan holds it out: checked on the page before any raster
            // work, and on the crop as prepared, which a long strip's window
            // can widen past the page's edge.
            if region.sendable_crop(preprocessing, page.width, page.height).is_none() {
                return Err(ConsentError::CropTooLarge);
            }
            let cloud = region
                .prepare(&job, &page, preprocessing)
                .map_err(ConsentError::RenderPrepareFailed)?;
            if !cleaner_core::engines::render::within_service_limits(cloud.crop_bounds) {
                return Err(ConsentError::CropTooLarge);
            }
            let prepared = &cloud.prepared;
            let crop_bounds = cloud.crop_bounds;
            let crop_png = encode_rgb8_png(crop_bounds.w, crop_bounds.h, prepared.image_rgb8())?;
            let crop_png_sha256 = sha256_hex(&crop_png);

            let hint_png = encode_gray8_png(crop_bounds.w, crop_bounds.h, prepared.hint_gray8())?;
            let hint_png_sha256 = sha256_hex(&hint_png);

            let mask_hash = region.mask_digest(preprocessing);
            let (revision, revision_hash) = (region.revision, region.revision_hash.clone());

            (
                crop_bounds,
                crop_png_sha256,
                hint_png_sha256,
                source_hash,
                mask_hash,
                revision,
                revision_hash,
                cloud.input.digest,
                cloud.input.predecessors,
                source_idx,
                region_id,
            )
        };

        let proposal_id = generate_proposal_id()?;
        let now_ms = now_epoch_ms();
        let expires_at_ms = now_ms.saturating_add(DEFAULT_PROPOSAL_TTL.as_millis() as u64);

        let proposal = ConsentProposal {
            proposal_id: proposal_id.clone(),
            provider,
            profile_id: profile_id.to_string(),
            profile_name: profile.name.clone(),
            endpoint_url: profile.endpoint_url.clone(),
            canonical_endpoint_fingerprint,
            crop_bounds,
            crop_width: crop_bounds.w,
            crop_height: crop_bounds.h,
            crop_png_sha256,
            hint_png_sha256,
            operation_digest,
            source_hash,
            mask_hash,
            revision,
            revision_hash,
            input_sha256,
            predecessors_sha256,
            recipe: request.recipe,
            region_ids: vec![region_id],
            intent: request.intent,
            chapter_id: request.chapter_id,
            page_index: request.page_index,
            source_idx,
            issued_at_ms: now_ms,
            expires_at_ms,
        };

        let canonical_job_path = job_path
            .canonicalize()
            .unwrap_or_else(|_| job_path.to_path_buf());

        {
            let mut map = self.proposals.lock().unwrap_or_else(|p| p.into_inner());

            // Atomically verify epoch before inserting proposal into cache
            let current_epoch = self.get_profile_epoch(provider, profile_id);
            if current_epoch != start_epoch {
                return Err(ConsentError::ProfileMutated);
            }

            // Purge expired
            map.retain(|_, v| v.proposal.expires_at_ms > now_ms);
            // Cap capacity
            if map.len() >= MAX_CACHED_PROPOSALS {
                if let Some(oldest_key) = map
                    .iter()
                    .min_by_key(|(_, v)| v.proposal.issued_at_ms)
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&oldest_key);
                }
            }
            map.insert(
                proposal_id,
                StoredProposal {
                    proposal: proposal.clone(),
                    canonical_job_path,
                    profile_epoch: start_epoch,
                    confirmed_grant_nonce: None,
                },
            );
        }

        Ok(proposal)
    }

    /// Step 2: Confirm a previously prepared proposal and issue an attempt-limited authorization grant.
    pub fn confirm_proposal(
        &self,
        opts: ConfirmProposalOptions<'_>,
    ) -> Result<Grant, ConsentError> {
        let proposal_id = opts.proposal_id;
        let intent = opts.intent;
        let config = opts.config;
        let cloud_allowed = opts.cloud_allowed;
        let job_path = opts.job_path;
        let grant_ttl = opts.grant_ttl;
        let max_attempts = opts.max_attempts;

        if !cloud_allowed {
            return Err(ConsentError::CloudDisabled);
        }

        let now_ms = now_epoch_ms();
        validate_operation_intent_bounds(intent)?;
        let incoming_op_digest = compute_operation_digest(intent)?;

        // 1. Fetch and validate cached proposal under mutex
        let (proposal, stored_canonical_job_path, start_epoch) = {
            let map = self.proposals.lock().unwrap_or_else(|p| p.into_inner());

            let stored = map.get(proposal_id).ok_or(ConsentError::ProposalNotFound)?;

            let current_epoch =
                self.get_profile_epoch(stored.proposal.provider, &stored.proposal.profile_id);
            if stored.profile_epoch != current_epoch {
                return Err(ConsentError::ProfileMutated);
            }

            if stored.confirmed_grant_nonce.is_some() {
                return Err(ConsentError::ProposalAlreadyConsumed);
            }

            if now_ms < stored.proposal.issued_at_ms {
                return Err(ConsentError::ClockRollbackDetected);
            }

            if now_ms >= stored.proposal.expires_at_ms {
                return Err(ConsentError::ProposalExpired {
                    expired_at_ms: stored.proposal.expires_at_ms,
                    now_ms,
                });
            }

            if &stored.proposal.intent != intent
                || stored.proposal.operation_digest != incoming_op_digest
            {
                return Err(ConsentError::IntentMismatch);
            }

            (
                stored.proposal.clone(),
                stored.canonical_job_path.clone(),
                current_epoch,
            )
        };

        // 2. Validate current configuration and endpoint fingerprint
        let profile = match proposal.provider {
            CloudProvider::Beam => config.beam_profiles.get(&proposal.profile_id),
            CloudProvider::Modal => config.modal_profiles.get(&proposal.profile_id),
        }
        .ok_or_else(|| {
            ConsentError::ProfileNotFound(proposal.profile_id.clone(), proposal.provider)
        })?;

        let current_fingerprint = compute_canonical_endpoint_fingerprint(&profile.endpoint_url)
            .map_err(ConsentError::InvalidEndpoint)?;

        if current_fingerprint != proposal.canonical_endpoint_fingerprint {
            return Err(ConsentError::ConfigMismatch);
        }

        // 3. Lock job and re-prepare raster crops to revalidate all identities
        let current_canonical_job_path = job_path
            .canonicalize()
            .unwrap_or_else(|_| job_path.to_path_buf());
        if current_canonical_job_path != stored_canonical_job_path {
            return Err(ConsentError::PageMismatch);
        }

        let _lock = run::lock_job(job_path).map_err(|e| ConsentError::Internal(e.to_string()))?;
        let job = Job::open(job_path).map_err(|e| ConsentError::Internal(e.to_string()))?;
        let source_idx = Library::resolve_page(&job.project, proposal.page_index as usize)
            .ok_or_else(|| {
                ConsentError::PageNotFound(proposal.page_index, proposal.chapter_id.clone())
            })?;

        if source_idx != proposal.source_idx {
            return Err(ConsentError::PageMismatch);
        }

        let source_path = job
            .source_path(source_idx)
            .ok_or(ConsentError::SourceImageError)?;
        let source_bytes =
            std::fs::read(&source_path).map_err(|_| ConsentError::SourceImageError)?;
        let current_source_hash = sha256_hex(&source_bytes);
        if current_source_hash != proposal.source_hash {
            return Err(ConsentError::SourceMismatch);
        }

        let page = job.display_page(source_idx,&source_bytes)
            .map_err(|e| ConsentError::RenderPrepareFailed(e.to_string()))?;

        let preprocessing = Preprocessing::of(&proposal.recipe.preprocessing_version)
            .map_err(|e| ConsentError::InvalidRecipe(e.to_string()))?;
        for region_id in &proposal.region_ids {
            let region = load_cloud_region(&job, region_id, &current_source_hash)?;

            if region.source_idx != proposal.source_idx {
                return Err(ConsentError::PageMismatch);
            }

            let current_mask_hash = region.mask_digest(preprocessing);
            if current_mask_hash != proposal.mask_hash {
                return Err(ConsentError::MaskMismatch);
            }

            // Full structured re-preparation
            let cloud = region
                .prepare(&job, &page, preprocessing)
                .map_err(ConsentError::RenderPrepareFailed)?;
            let prepared = &cloud.prepared;
            if cloud.crop_bounds != proposal.crop_bounds
                || cloud.input.digest != proposal.input_sha256
                || cloud.input.predecessors != proposal.predecessors_sha256
            {
                return Err(ConsentError::CropDigestMismatch);
            }

            let crop_png =
                encode_rgb8_png(prepared.crop().w, prepared.crop().h, prepared.image_rgb8())?;
            if sha256_hex(&crop_png) != proposal.crop_png_sha256 {
                return Err(ConsentError::CropDigestMismatch);
            }

            let hint_png =
                encode_gray8_png(prepared.crop().w, prepared.crop().h, prepared.hint_gray8())?;
            if sha256_hex(&hint_png) != proposal.hint_png_sha256 {
                return Err(ConsentError::CropDigestMismatch);
            }

            if region.revision != proposal.revision
                || region.revision_hash != proposal.revision_hash
            {
                return Err(ConsentError::RevisionMismatch);
            }
        }

        // 4. Issue authorization grant via GrantService with atomic epoch validation
        let scope = GrantScope {
            capability: crate::inference::policy::FLUX_CAPABILITY.to_string(),
            provider: proposal.provider,
            profile_id: proposal.profile_id,
            canonical_endpoint_fingerprint: proposal.canonical_endpoint_fingerprint,
            source_hash: proposal.source_hash,
            crop_sha256: proposal.crop_png_sha256,
            hint_sha256: proposal.hint_png_sha256,
            operation_digest: proposal.operation_digest,
            crop_bounds: proposal.crop_bounds,
            mask_hash: proposal.mask_hash,
            revision: proposal.revision,
            input_sha256: proposal.input_sha256.clone(),
            predecessors_sha256: proposal.predecessors_sha256.clone(),
            recipe: proposal.recipe,
            region_ids: proposal.region_ids,
        };

        let grant = self
            .resolve_grant_service()
            .issue_grant_with_epoch(scope, start_epoch, grant_ttl, max_attempts)
            .map_err(|e| match e {
                GrantError::ProfileMutated => ConsentError::ProfileMutated,
                other => ConsentError::GrantIssuanceError(other),
            })?;

        // Explicit confirmed grant binding: record nonce ONLY after successful issuance
        {
            let mut map = self.proposals.lock().unwrap_or_else(|p| p.into_inner());

            let stored = map
                .get_mut(proposal_id)
                .ok_or(ConsentError::ProposalNotFound)?;

            let current_epoch =
                self.get_profile_epoch(stored.proposal.provider, &stored.proposal.profile_id);
            if stored.profile_epoch != current_epoch || current_epoch != start_epoch {
                let _ = self.resolve_grant_service().revoke_grant(&grant.nonce);
                return Err(ConsentError::ProfileMutated);
            }

            stored.confirmed_grant_nonce = Some(grant.nonce.clone());
        }

        Ok(grant)
    }

    /// Backend-only wrapper to prepare proposal resolving application settings, config, and library path.
    pub fn prepare_proposal_for_app(
        &self,
        app: &tauri::AppHandle,
        request: PrepareProposalRequest,
    ) -> Result<ConsentProposal, ConsentError> {
        let cloud_allowed = crate::inference::cloud_allowed(app);

        if !cloud_allowed {
            return Err(ConsentError::CloudDisabled);
        }

        let config =
            crate::inference::config::read_inference_config(app).map_err(ConsentError::Internal)?;
        let library = Library::for_app(app).map_err(|e| ConsentError::Internal(e.to_string()))?;
        let job_path = library
            .resolve_chapter(&request.chapter_id)
            .map_err(|e| ConsentError::Internal(e.to_string()))?;

        self.prepare_proposal(request, &config, cloud_allowed, &job_path)
    }

    /// Backend-only wrapper to confirm proposal resolving application settings, config, and library path.
    pub fn confirm_proposal_for_app(
        &self,
        app: &tauri::AppHandle,
        proposal_id: &str,
        intent: &OperationIntent,
        grant_ttl: Duration,
        max_attempts: u32,
    ) -> Result<Grant, ConsentError> {
        let cloud_allowed = crate::inference::cloud_allowed(app);

        if !cloud_allowed {
            return Err(ConsentError::CloudDisabled);
        }

        let config =
            crate::inference::config::read_inference_config(app).map_err(ConsentError::Internal)?;
        let library = Library::for_app(app).map_err(|e| ConsentError::Internal(e.to_string()))?;

        let proposal = self.get_proposal(proposal_id)?;
        let job_path = library
            .resolve_chapter(&proposal.chapter_id)
            .map_err(|e| ConsentError::Internal(e.to_string()))?;

        self.confirm_proposal(ConfirmProposalOptions {
            proposal_id,
            intent,
            config: &config,
            cloud_allowed,
            job_path: &job_path,
            grant_ttl,
            max_attempts,
        })
    }

    /// Retrieve an unexpired proposal by ID.
    pub fn get_proposal(&self, proposal_id: &str) -> Result<ConsentProposal, ConsentError> {
        let map = self.proposals.lock().unwrap_or_else(|p| p.into_inner());
        let stored = map.get(proposal_id).ok_or(ConsentError::ProposalNotFound)?;
        let current_epoch =
            self.get_profile_epoch(stored.proposal.provider, &stored.proposal.profile_id);
        if stored.profile_epoch != current_epoch {
            return Err(ConsentError::ProfileMutated);
        }
        let now_ms = now_epoch_ms();
        if now_ms < stored.proposal.issued_at_ms {
            return Err(ConsentError::ClockRollbackDetected);
        }
        if now_ms >= stored.proposal.expires_at_ms {
            return Err(ConsentError::ProposalExpired {
                expired_at_ms: stored.proposal.expires_at_ms,
                now_ms,
            });
        }
        Ok(stored.proposal.clone())
    }

    /// Retrieve an unexpired proposal along with its canonical job path by ID.
    pub fn get_proposal_with_job_path(
        &self,
        proposal_id: &str,
    ) -> Result<(ConsentProposal, PathBuf), ConsentError> {
        let map = self.proposals.lock().unwrap_or_else(|p| p.into_inner());
        let stored = map.get(proposal_id).ok_or(ConsentError::ProposalNotFound)?;
        let current_epoch =
            self.get_profile_epoch(stored.proposal.provider, &stored.proposal.profile_id);
        if stored.profile_epoch != current_epoch {
            return Err(ConsentError::ProfileMutated);
        }
        let now_ms = now_epoch_ms();
        if now_ms < stored.proposal.issued_at_ms {
            return Err(ConsentError::ClockRollbackDetected);
        }
        if now_ms >= stored.proposal.expires_at_ms {
            return Err(ConsentError::ProposalExpired {
                expired_at_ms: stored.proposal.expires_at_ms,
                now_ms,
            });
        }
        Ok((stored.proposal.clone(), stored.canonical_job_path.clone()))
    }

    /// Retrieve a confirmed proposal and its canonical job path by verifying proposal ID and exact grant nonce binding.
    pub fn get_confirmed_proposal_with_job_path(
        &self,
        proposal_id: &str,
        grant_nonce: &str,
    ) -> Result<(ConsentProposal, PathBuf), ConsentError> {
        let map = self.proposals.lock().unwrap_or_else(|p| p.into_inner());
        let stored = map.get(proposal_id).ok_or(ConsentError::ProposalNotFound)?;

        let current_epoch =
            self.get_profile_epoch(stored.proposal.provider, &stored.proposal.profile_id);
        if stored.profile_epoch != current_epoch {
            return Err(ConsentError::ProfileMutated);
        }

        let now_ms = now_epoch_ms();
        if now_ms < stored.proposal.issued_at_ms {
            return Err(ConsentError::ClockRollbackDetected);
        }
        if now_ms >= stored.proposal.expires_at_ms {
            return Err(ConsentError::ProposalExpired {
                expired_at_ms: stored.proposal.expires_at_ms,
                now_ms,
            });
        }

        match &stored.confirmed_grant_nonce {
            Some(nonce) if nonce == grant_nonce => {
                Ok((stored.proposal.clone(), stored.canonical_job_path.clone()))
            }
            Some(_) => Err(ConsentError::GrantNonceMismatch),
            None => Err(ConsentError::ProposalNotConfirmed),
        }
    }

    /// Purge expired proposals from the cache.
    pub fn purge_expired(&self) {
        let mut map = self.proposals.lock().unwrap_or_else(|p| p.into_inner());
        let now_ms = now_epoch_ms();
        map.retain(|_, stored| stored.proposal.expires_at_ms > now_ms);
    }

    /// Revoke all pending proposals associated with a specific profile.
    pub fn revoke_profile_proposals(&self, provider: CloudProvider, profile_id: &str) {
        self.invalidate_profile(provider, profile_id);
    }

    /// Explicitly revoke a proposal by ID.
    pub fn revoke_proposal(&self, proposal_id: &str) -> Result<(), ConsentError> {
        let mut map = self.proposals.lock().unwrap_or_else(|p| p.into_inner());
        map.remove(proposal_id)
            .map(|_| ())
            .ok_or(ConsentError::ProposalNotFound)
    }
}

impl Default for ConsentService {
    fn default() -> Self {
        Self::new()
    }
}

/* ------------------------------------------------------------------ */
/* Cloud clean plans: one consent, bounded chunks                      */
/* ------------------------------------------------------------------ */

/// One region of a cloud clean plan as its consent binds it.
#[derive(Debug, Serialize)]
pub(crate) struct PlanRegionTerms<'a> {
    pub id: &'a str,
    pub page_index: u32,
    pub revision_hash: &'a str,
    /// The text group the region cleans (`tg-...`), when it has one.
    pub group_id: Option<&'a str>,
}

/// The estimate a cloud clean consent shows, in integer units so the digest
/// never depends on how a float prints.
#[derive(Debug, Serialize)]
pub(crate) struct PlanEstimateTerms<'a> {
    pub crop_pixels: u64,
    pub work_pixels: u64,
    pub gpu_ms_low: u64,
    pub gpu_ms_high: u64,
    pub cost_micro_usd_low: Option<u64>,
    pub cost_micro_usd_high: Option<u64>,
    pub gpu: Option<&'a str>,
    pub gpu_micro_usd_per_hour: Option<u64>,
}

/// Everything one cloud clean consent covers: the destination and model, the
/// exact regions in the order they run, the chunking rule that cuts them into
/// bounded chunks, where they are cleaned, and the estimate the person saw.
#[derive(Debug, Serialize)]
pub(crate) struct CleanPlanTerms<'a> {
    pub chapter_id: &'a str,
    pub provider: CloudProvider,
    pub profile_id: &'a str,
    pub endpoint_fingerprint: &'a str,
    pub recipe: &'a RenderRecipe,
    /// `cloud`, or `mixed` when fill and solid picks are tried here first.
    pub execution: &'a str,
    /// Consecutive runs of at most this many regions, in plan order.
    pub chunk_regions: u32,
    pub regions: Vec<PlanRegionTerms<'a>>,
    pub estimate: PlanEstimateTerms<'a>,
}

/// The digest a cloud clean consent is given for. A change to any term (one
/// id more, two ids swapped, another chunk size, another estimate) is another
/// digest, so a grant spent under it covers nothing else.
pub(crate) fn compute_clean_plan_digest(terms: &CleanPlanTerms<'_>) -> Result<String, ConsentError> {
    let mut hasher = Sha256::new();
    hasher.update(b"domain:cloud_clean_plan_v1:");
    hasher.update(serde_json::to_vec(terms).map_err(|e| {
        ConsentError::Internal(format!("failed to serialize clean plan: {e}"))
    })?);
    Ok(format!("{:x}", hasher.finalize()))
}

/// How many chunks the rule makes of a plan of `total` regions.
pub(crate) fn chunk_count(total: usize, chunk_regions: usize) -> usize {
    if chunk_regions == 0 { 0 } else { total.div_ceil(chunk_regions) }
}

/// The plan positions chunk `index` covers, or `None` past the last chunk.
pub(crate) fn chunk_range(total: usize, chunk_regions: usize, index: usize) -> Option<std::ops::Range<usize>> {
    let start = index.checked_mul(chunk_regions).filter(|_| chunk_regions > 0)?;
    (start < total).then(|| start..total.min(start + chunk_regions))
}

/// One chunk of an agreed plan: the plan it belongs to, its place under the
/// chunking rule, and the ids it may send. Minted just before the chunk runs,
/// from the plan's own list and never from a caller's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkGrant {
    pub plan_digest: String,
    pub index: usize,
    pub ids: Vec<String>,
}

/// Mint chunk `index` of the plan whose ordered ids are `ordered`. `keep` is
/// asked about each plan position of the chunk, in order, and a position it
/// refuses is left out: a chunk only ever shrinks from its place in the plan.
pub(crate) fn issue_chunk_grant<S: AsRef<str>>(
    plan_digest: &str,
    ordered: &[S],
    chunk_regions: usize,
    index: usize,
    keep: &mut dyn FnMut(usize) -> bool,
) -> Option<ChunkGrant> {
    let range = chunk_range(ordered.len(), chunk_regions, index)?;
    let ids = range.filter(|&position| keep(position)).map(|position| ordered[position].as_ref().to_owned()).collect();
    Some(ChunkGrant { plan_digest: plan_digest.to_owned(), index, ids })
}

/// Prove a chunk is a subset of its plan: the same plan digest, a chunk the
/// rule makes, no larger than `max_regions`, and ids that are that chunk's
/// own, in plan order, each once. Answers their plan positions.
pub(crate) fn verify_chunk_grant<S: AsRef<str>>(
    plan_digest: &str,
    ordered: &[S],
    chunk_regions: usize,
    max_regions: usize,
    grant: &ChunkGrant,
) -> Result<Vec<usize>, &'static str> {
    if grant.plan_digest != plan_digest {
        return Err("cloud_clean_chunk_mismatch: plan");
    }
    if chunk_regions > max_regions {
        return Err("cloud_clean_chunk_mismatch: size");
    }
    let range = chunk_range(ordered.len(), chunk_regions, grant.index).ok_or("cloud_clean_chunk_mismatch: index")?;
    let mut next = range.start;
    let mut positions = Vec::with_capacity(grant.ids.len());
    for id in &grant.ids {
        // Strictly after the last match: a reordered or repeated id fails.
        let position = (next..range.end)
            .find(|&position| ordered[position].as_ref() == id)
            .ok_or("cloud_clean_chunk_outside_plan")?;
        positions.push(position);
        next = position + 1;
    }
    Ok(positions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::engines::render::CloudProvider;
    use cleaner_core::image::fixtures;
    use cleaner_core::mask::Mask;
    use cleaner_core::patch::{Engine, Patch, Provenance};
    use cleaner_core::project::{Project, StripMode};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn test_scratch(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mc-consent-test-{}-{}-{}",
            name,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_recipe() -> RenderRecipe {
        RenderRecipe::new(
            "test-sdnq-v1",
            "1.0.0",
            "test-flux-schnell",
            "0123456789abcdef0123456789abcdef01234567",
            false,
        )
    }

    fn test_config() -> InferenceConfig {
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
        config
    }

    fn setup_test_job(root: &Path, patch_id: &str) -> (PathBuf, Rect) {
        setup_job_on(root, patch_id, fixtures::by_name("l8").raster, Rect::new(10, 10, 30, 20))
    }

    /// [`setup_test_job`] on `raster`, a Gray page, with the patch over `bounds`.
    fn setup_job_on(root: &Path, patch_id: &str, raster: cleaner_core::image::Raster, bounds: Rect) -> (PathBuf, Rect) {
        let raws = root.join("raws");
        std::fs::create_dir_all(&raws).unwrap();
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

    /// **A crop the render service would refuse is not proposed.** The
    /// region's crop is past the service's limits under either version, even
    /// with no page around it, so the proposal is refused with its own code
    /// before any raster work, and nothing is left to consent to: the same
    /// test a clean plan holds it out by.
    #[test]
    fn an_oversized_crop_is_refused_before_any_proposal() {
        let scratch = test_scratch("oversized-crop");
        let mut page = fixtures::by_name("l8").raster;
        (page.width, page.height) = (2200, 64);
        page.data = vec![180; 2200 * 64];
        let (manifest, _) = setup_job_on(&scratch, "big", page, Rect::new(20, 10, 2044, 30));
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));
        for version in ["1.0.0", cleaner_core::engines::render::PREPROCESSING_VERSION] {
            let mut recipe = test_recipe();
            recipe.preprocessing_version = version.into();
            let result = service.prepare_proposal(PrepareProposalRequest {
                chapter_id: "chap-1".into(), page_index: 0, region_id: Some("big".into()),
                target: ExecutionTarget::Modal { profile_id: "modal-prof-1".into() },
                recipe, intent: OperationIntent::CleanAnyway,
            }, &test_config(), true, &manifest);
            assert!(matches!(result.as_ref().err(), Some(ConsentError::CropTooLarge)), "{version}: {:?}", result.err());
        }
        assert!(service.proposals.lock().unwrap().is_empty(), "a proposal was made");
        assert!(ConsentError::CropTooLarge.to_string().starts_with("cloud_consent_crop_too_large"));
    }

    #[test]
    fn text_shaped_region_is_refused_at_proposal_without_mutation() {
        let scratch = test_scratch("text-shape-proposal-refusal");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let mut job = Job::open(&manifest).unwrap();
        job.project.patches[0].geometry_policy = cleaner_core::text_shape::GeometryPolicy::TextShape;
        job.flush().unwrap();
        let before = std::fs::read(&manifest).unwrap();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));
        let result = service.prepare_proposal(PrepareProposalRequest {
            chapter_id: "chap-1".into(), page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal { profile_id: "modal-prof-1".into() },
            recipe: test_recipe(), intent: OperationIntent::CleanAnyway,
        }, &test_config(), true, &manifest);
        assert!(matches!(result, Err(ConsentError::RenderPrepareFailed(reason))
            if reason.contains("text-shaped")));
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
    }

    #[test]
    fn consent_rejects_a_changed_visible_predecessor_even_when_target_is_unchanged() {
        let scratch = test_scratch("predecessor-stale");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let mut job = Job::open(&manifest).unwrap();
        let target = job
            .load_patch(
                job.project
                    .patches
                    .iter()
                    .find(|p| p.id == "reg-1")
                    .unwrap(),
            )
            .unwrap();
        let mut first = target.clone();
        first.id = "earlier".into();
        first.order = 0;
        first.pixels.data.fill(240);
        job.complete_region(0, &first, None).unwrap();

        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));
        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };
        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();
        assert_ne!(proposal.input_sha256, "");
        first.pixels.data.fill(60);
        job.complete_region(0, &first, None).unwrap();
        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();
        assert_eq!(err, ConsentError::CropDigestMismatch);
    }

    #[test]
    fn test_prepare_and_confirm_lifecycle_success() {
        let scratch = test_scratch("lifecycle-success");
        let (manifest, _bounds) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let grant_service = Arc::new(GrantService::new());
        let service = ConsentService::new_isolated(Arc::clone(&grant_service));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::ApplyTool {
                tool: "aiMaskBrush".into(),
                params: Some(serde_json::json!({ "radius": 15 })),
            },
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .expect("prepare should succeed");

        assert_eq!(proposal.provider, CloudProvider::Modal);
        assert_eq!(proposal.profile_id, "modal-prof-1");
        assert_eq!(proposal.region_ids, vec!["reg-1".to_string()]);
        assert_eq!(proposal.crop_bounds.w % 16, 0);
        assert_eq!(proposal.crop_bounds.h % 16, 0);
        assert!(!proposal.crop_png_sha256.is_empty());
        assert!(!proposal.hint_png_sha256.is_empty());
        assert!(!proposal.operation_digest.is_empty());
        assert!(!proposal.source_hash.is_empty());
        assert_ne!(proposal.revision, 0);

        // Confirm proposal
        let grant = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .expect("confirm should succeed");

        assert_eq!(grant.scope.provider, CloudProvider::Modal);
        assert_eq!(grant.scope.profile_id, "modal-prof-1");
        assert_eq!(grant.scope.crop_sha256, proposal.crop_png_sha256);
        assert_eq!(grant.scope.hint_sha256, proposal.hint_png_sha256);
        assert_eq!(grant.scope.operation_digest, proposal.operation_digest);
        assert_eq!(grant.scope.source_hash, proposal.source_hash);
        assert_eq!(grant.scope.mask_hash, proposal.mask_hash);
        assert_eq!(grant.scope.revision, proposal.revision);
        assert_eq!(grant.scope.crop_bounds, proposal.crop_bounds);

        // Verify grant can be consumed via GrantService
        grant_service
            .validate_and_consume(&grant.nonce, &grant.scope)
            .expect("grant consumption should succeed");
    }

    #[test]
    fn test_source_image_mutation_fails_confirm() {
        let scratch = test_scratch("source-mutation");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Mutate actual imported source path
        let job = Job::open(&manifest).unwrap();
        let source_path = job.source_path(0).unwrap();
        std::fs::write(&source_path, b"corrupted-mutated-image-bytes").unwrap();

        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert_eq!(err, ConsentError::SourceMismatch);
    }

    #[test]
    fn test_mask_data_mutation_fails_confirm() {
        let scratch = test_scratch("mask-mutation");
        let (manifest, bounds) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::RerunMask {
                mask_id: "reg-1.mask".into(),
                kind: "retry".into(),
                engine: None,
            },
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Mutate mask file on disk using job.sidecar
        let job = Job::open(&manifest).unwrap();
        let record = job
            .project
            .patches
            .iter()
            .find(|r| r.id == "reg-1")
            .unwrap();
        let mask_file = job.sidecar().join(&record.mask_ref);
        let changed = Mask::empty(bounds);
        std::fs::write(
            &mask_file,
            cleaner_core::project::buffers::encode_mask(&changed),
        )
        .unwrap();

        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert!(matches!(
            err,
            ConsentError::MaskMismatch
                | ConsentError::RevisionMismatch
                | ConsentError::CropDigestMismatch
        ));
    }

    #[test]
    fn test_config_endpoint_change_fails_confirm() {
        let scratch = test_scratch("config-mutation");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Mutate endpoint URL in config
        let mut mutated_config = config.clone();
        mutated_config
            .modal_profiles
            .get_mut("modal-prof-1")
            .unwrap()
            .endpoint_url = "https://modal-mutated.example.com/mc/v1".to_string();

        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &mutated_config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert_eq!(err, ConsentError::ConfigMismatch);
    }

    #[test]
    fn test_cloud_permission_disabled_fails_closed() {
        let scratch = test_scratch("permission-disabled");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        // Preparation fails if cloud_allowed = false
        let prep_err = service
            .prepare_proposal(req.clone(), &config, false, &manifest)
            .unwrap_err();
        assert_eq!(prep_err, ConsentError::CloudDisabled);

        // Prepare succeeds with cloud_allowed = true
        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Confirmation fails if permission is revoked before confirmation
        let conf_err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: false,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();
        assert_eq!(conf_err, ConsentError::CloudDisabled);
    }

    #[test]
    fn test_intent_mismatch_fails_confirm() {
        let scratch = test_scratch("intent-mismatch");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::ApplyTool {
                tool: "brush".into(),
                params: Some(serde_json::json!({ "size": 10 })),
            },
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Confirm with different tool intent
        let wrong_intent = OperationIntent::CleanAnyway;
        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &wrong_intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert_eq!(err, ConsentError::IntentMismatch);
    }

    #[test]
    fn test_operation_params_length_bound_enforced() {
        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::ApplyTool {
                tool: "brush".into(),
                params: Some(
                    serde_json::json!({ "huge": "x".repeat(MAX_OPERATION_PARAMS_LEN + 100) }),
                ),
            },
        };

        let scratch = test_scratch("params-bound");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let err = service
            .prepare_proposal(req, &config, true, &manifest)
            .unwrap_err();
        assert_eq!(err, ConsentError::OperationParamsTooLarge);
    }

    #[test]
    fn test_repeated_confirm_fails_consumed() {
        let scratch = test_scratch("repeated-confirm");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // First confirm succeeds
        service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        // Second confirm fails closed
        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert_eq!(err, ConsentError::ProposalAlreadyConsumed);
    }

    #[test]
    fn test_unknown_proposal_id_fails() {
        let scratch = test_scratch("unknown-proposal");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: "non-existent-proposal-id",
                intent: &OperationIntent::CleanAnyway,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert_eq!(err, ConsentError::ProposalNotFound);
    }

    #[test]
    fn test_unsupported_new_region_fails_without_stored_patch() {
        let scratch = test_scratch("unsupported-new-region");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: None,
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CreateRegion {
                tool: "brush".into(),
                params: None,
            },
        };

        let err = service
            .prepare_proposal(req, &config, true, &manifest)
            .unwrap_err();
        assert!(matches!(err, ConsentError::UnsupportedNewRegion(_)));
    }

    #[test]
    fn test_invalid_recipe_fails_prepare() {
        let scratch = test_scratch("invalid-recipe");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let mut recipe = test_recipe();
        recipe.native_mask_conditioning = true;

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe,
            intent: OperationIntent::CleanAnyway,
        };

        let err = service
            .prepare_proposal(req, &config, true, &manifest)
            .unwrap_err();
        assert!(matches!(err, ConsentError::InvalidRecipe(_)));
    }

    /// The recipe's preprocessing version decides what a proposal binds: a
    /// version this build cannot prepare is refused before any raster work,
    /// and 2.0.0 binds the stored lettering by its own digest, which the
    /// confirm re-derives under the same version.
    #[test]
    fn test_preprocessing_version_binds_the_hole_and_an_unknown_one_is_refused() {
        let scratch = test_scratch("preprocessing-version");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        {
            // Lettering narrower than the patch's applied mask.
            let mut job = Job::open(&manifest).unwrap();
            let record = job.project.patches.iter().find(|r| r.id == "reg-1").unwrap().clone();
            let mut patch = job.load_patch(&record).unwrap();
            patch.ink = cleaner_core::mask::Mask::filled(Rect::new(16, 14, 12, 8));
            job.complete_region(0, &patch, None).unwrap();
        }
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));
        let request = |version: &str| {
            let mut recipe = test_recipe();
            recipe.preprocessing_version = version.into();
            PrepareProposalRequest {
                chapter_id: "chap-1".into(),
                page_index: 0,
                region_id: Some("reg-1".into()),
                target: ExecutionTarget::Modal { profile_id: "modal-prof-1".into() },
                recipe,
                intent: OperationIntent::CleanAnyway,
            }
        };

        let err = service
            .prepare_proposal(request("9.9.9"), &config, true, &manifest)
            .unwrap_err();
        assert!(matches!(err, ConsentError::InvalidRecipe(_)), "{err:?}");

        let v1 = service
            .prepare_proposal(request("1.0.0"), &config, true, &manifest)
            .unwrap();
        let v2_request = request(cleaner_core::engines::render::PREPROCESSING_VERSION);
        let v2 = service
            .prepare_proposal(v2_request.clone(), &config, true, &manifest)
            .unwrap();
        let job = Job::open(&manifest).unwrap();
        let region = load_cloud_region(&job, "reg-1", &v2.source_hash).unwrap();
        assert_eq!(v1.mask_hash, sha256_hex(&region.mask.bits), "V1 keeps its digest");
        assert_eq!(v2.mask_hash, region.mask_digest(Preprocessing::V2));
        assert_ne!(v1.mask_hash, v2.mask_hash);
        let source = &job.project.sources[region.source_idx];
        assert_eq!(
            Some(v2.crop_bounds),
            region.estimated_crop(Preprocessing::V2, source.w, source.h),
            "the estimate names the crop the render sends"
        );
        assert_ne!(v1.crop_png_sha256, v2.crop_png_sha256);

        let grant = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &v2.proposal_id,
                intent: &v2_request.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .expect("a 2.0.0 proposal confirms under 2.0.0");
        assert_eq!(grant.scope.mask_hash, v2.mask_hash);
    }

    #[test]
    fn test_local_target_rejected_by_consent_service() {
        let scratch = test_scratch("local-target");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Local,
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let err = service
            .prepare_proposal(req, &config, true, &manifest)
            .unwrap_err();
        assert_eq!(err, ConsentError::LocalTargetNotAllowed);
    }

    #[test]
    fn test_profile_not_found_fails_prepare() {
        let scratch = test_scratch("profile-not-found");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "non-existent-profile".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let err = service
            .prepare_proposal(req, &config, true, &manifest)
            .unwrap_err();
        assert_eq!(
            err,
            ConsentError::ProfileNotFound("non-existent-profile".into(), CloudProvider::Modal)
        );
    }

    #[test]
    fn test_expired_proposal_fails_confirm() {
        let scratch = test_scratch("proposal-expiry");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Artificially expire proposal
        {
            let mut map = service.proposals.lock().unwrap();
            let stored = map.get_mut(&proposal.proposal_id).unwrap();
            stored.proposal.expires_at_ms = 1;
        }

        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert!(matches!(err, ConsentError::ProposalExpired { .. }));
    }

    #[test]
    fn test_profile_revocation_cascades_to_proposals() {
        let scratch = test_scratch("cascade-revocation");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Profile revocation hook called
        service.revoke_profile_proposals(CloudProvider::Modal, "modal-prof-1");

        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert_eq!(err, ConsentError::ProfileMutated);
    }

    #[test]
    fn test_preserved_outside_pixels_in_prepared_render() {
        let scratch = test_scratch("outside-pixels");
        let (manifest, bounds) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req, &config, true, &manifest)
            .unwrap();

        assert!(proposal.crop_bounds.x <= bounds.x);
        assert!(proposal.crop_bounds.y <= bounds.y);
        assert!(proposal.crop_bounds.right() >= bounds.right());
        assert!(proposal.crop_bounds.bottom() >= bounds.bottom());
        assert_eq!(proposal.crop_width, proposal.crop_bounds.w);
        assert_eq!(proposal.crop_height, proposal.crop_bounds.h);
    }

    #[test]
    fn test_epoch_mismatch_fails_confirm_with_profile_mutated() {
        let scratch = test_scratch("epoch-mismatch");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Advance epoch before confirmation
        service.invalidate_profile(CloudProvider::Modal, "modal-prof-1");

        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();

        assert_eq!(err, ConsentError::ProfileMutated);
    }

    #[test]
    fn test_rerun_mask_intent_binding_with_engine() {
        let scratch = test_scratch("rerun-mask-intent");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let service = ConsentService::new_isolated(Arc::new(GrantService::new()));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::RerunMask {
                mask_id: "reg-1.mask".into(),
                kind: "engine".into(),
                engine: Some("flux".into()),
            },
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        // Mismatched engine intent must fail
        let wrong_intent = OperationIntent::RerunMask {
            mask_id: "reg-1.mask".into(),
            kind: "engine".into(),
            engine: Some("lama".into()),
        };

        let err = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &wrong_intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap_err();
        assert_eq!(err, ConsentError::IntentMismatch);

        // Matching intent must succeed
        let grant = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();
        assert_eq!(grant.scope.operation_digest, proposal.operation_digest);
    }

    #[test]
    fn test_invalidate_all_revokes_proposals_and_grants() {
        let scratch = test_scratch("invalidate-all");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let grant_svc = Arc::new(GrantService::new());
        let service = ConsentService::new_isolated(Arc::clone(&grant_svc));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .unwrap();

        let grant = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .unwrap();

        // Invalidate all
        service.invalidate_all();

        // Consuming issued grant fails closed (revoked)
        assert_eq!(
            grant_svc.validate_and_consume(&grant.nonce, &grant.scope),
            Err(GrantError::Revoked)
        );
    }

    #[test]
    fn test_concurrent_confirmation_and_profile_invalidation() {
        let scratch = test_scratch("concurrency-invalidation");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let grant_svc = Arc::new(GrantService::new());
        let service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_svc)));

        let mut proposals = Vec::new();
        for _ in 0..10 {
            let req = PrepareProposalRequest {
                chapter_id: "chap-1".into(),
                page_index: 0,
                region_id: Some("reg-1".into()),
                target: ExecutionTarget::Modal {
                    profile_id: "modal-prof-1".into(),
                },
                recipe: test_recipe(),
                intent: OperationIntent::CleanAnyway,
            };
            let p = service
                .prepare_proposal(req, &config, true, &manifest)
                .unwrap();
            proposals.push(p);
        }

        let mut handles = Vec::new();
        for p in proposals {
            let svc = Arc::clone(&service);
            let cfg = config.clone();
            let mf = manifest.clone();
            handles.push(std::thread::spawn(move || {
                svc.confirm_proposal(ConfirmProposalOptions {
                    proposal_id: &p.proposal_id,
                    intent: &OperationIntent::CleanAnyway,
                    config: &cfg,
                    cloud_allowed: true,
                    job_path: &mf,
                    grant_ttl: Duration::from_secs(60),
                    max_attempts: 1,
                })
            }));
        }

        // Concurrently invalidate profile while threads are confirming
        service.invalidate_profile(CloudProvider::Modal, "modal-prof-1");

        for h in handles {
            let res = h.join().unwrap();
            // Each thread must either fail (ProfileMutated, ProposalAlreadyConsumed) or have its grant revoked
            if let Ok(grant) = res {
                assert_eq!(
                    grant_svc.validate_and_consume(&grant.nonce, &grant.scope),
                    Err(GrantError::Revoked),
                    "Any grant issued during invalidation must be revoked"
                );
            }
        }
    }

    #[test]
    fn test_prepare_proposal_concurrent_mutation_rejected() {
        let scratch = test_scratch("prepare-concurrent-mutation");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let grant_svc = Arc::new(GrantService::new());
        let service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_svc)));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        // Advance epoch before prepare proposal
        service.invalidate_profile(CloudProvider::Modal, "modal-prof-1");
        let current_epoch = service.get_profile_epoch(CloudProvider::Modal, "modal-prof-1");
        assert_eq!(current_epoch, 1);

        // Prepare proposal captures epoch 1 and succeeds
        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .expect("prepare proposal");
        assert_eq!(proposal.profile_id, "modal-prof-1");

        // Now test concurrent mutation during prepare:
        // Use a barrier to coordinate mutation between capture of start_epoch and cache insertion
        use std::sync::Barrier;
        let barrier = Arc::new(Barrier::new(2));

        let svc_t1 = Arc::clone(&service);
        let req_t1 = req.clone();
        let cfg_t1 = config.clone();
        let mf_t1 = manifest.clone();
        let b_t1 = Arc::clone(&barrier);

        let handle = std::thread::spawn(move || {
            // Invalidate profile while prepare is starting
            b_t1.wait();
            svc_t1.prepare_proposal(req_t1, &cfg_t1, true, &mf_t1)
        });

        // Thread 2 invalidates profile
        service.invalidate_profile(CloudProvider::Modal, "modal-prof-1");
        barrier.wait();

        let res = handle.join().unwrap();
        // Either the prepare captured the new epoch (2) and succeeded or captured old epoch (1) and failed with ProfileMutated
        match res {
            Ok(p) => {
                // If it succeeded, it must have captured epoch 2, not 1
                let stored_epoch = service.get_profile_epoch(p.provider, &p.profile_id);
                assert_eq!(stored_epoch, 2);
            }
            Err(ConsentError::ProfileMutated) => {
                // Successfully rejected concurrent mutation
            }
            Err(other) => panic!("Unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_consent_service_poison_recovery() {
        let scratch = test_scratch("consent-poison-recovery");
        let (manifest, _) = setup_test_job(&scratch, "reg-1");
        let config = test_config();
        let grant_svc = Arc::new(GrantService::new());
        let service = Arc::new(ConsentService::new_isolated(Arc::clone(&grant_svc)));

        let req = PrepareProposalRequest {
            chapter_id: "chap-1".into(),
            page_index: 0,
            region_id: Some("reg-1".into()),
            target: ExecutionTarget::Modal {
                profile_id: "modal-prof-1".into(),
            },
            recipe: test_recipe(),
            intent: OperationIntent::CleanAnyway,
        };

        let proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .expect("prepare before poison");

        // Force a thread to panic while holding the proposals lock
        let svc_clone = Arc::clone(&service);
        let _ = std::thread::spawn(move || {
            let _guard = svc_clone.proposals.lock().unwrap();
            panic!("intentional proposals poison panic");
        })
        .join();

        // Mutex is now poisoned. Invalidation must safely recover and mark proposal consumed
        service.invalidate_profile(CloudProvider::Modal, "modal-prof-1");

        // Attempting to confirm the invalidated proposal must fail closed with ProfileMutated (or AlreadyConsumed)
        let confirm_res = service.confirm_proposal(ConfirmProposalOptions {
            proposal_id: &proposal.proposal_id,
            intent: &req.intent,
            config: &config,
            cloud_allowed: true,
            job_path: &manifest,
            grant_ttl: Duration::from_secs(60),
            max_attempts: 1,
        });
        assert!(
            matches!(
                confirm_res,
                Err(ConsentError::ProfileMutated | ConsentError::ProposalAlreadyConsumed)
            ),
            "Must fail closed after invalidation on poisoned mutex"
        );

        // Prepare new proposal after poison must succeed safely
        let new_proposal = service
            .prepare_proposal(req.clone(), &config, true, &manifest)
            .expect("prepare after poison");

        let grant = service
            .confirm_proposal(ConfirmProposalOptions {
                proposal_id: &new_proposal.proposal_id,
                intent: &req.intent,
                config: &config,
                cloud_allowed: true,
                job_path: &manifest,
                grant_ttl: Duration::from_secs(60),
                max_attempts: 1,
            })
            .expect("confirm after poison");

        assert_eq!(
            grant_svc.validate_and_consume(&grant.nonce, &grant.scope),
            Ok(())
        );
    }

    #[test]
    fn intents_read_the_shapes_the_editor_sends() {
        // `rename_all` renames the tag's values, not a variant's own fields,
        // so a rerun names its mask as `mask_id`.
        let rerun: OperationIntent = serde_json::from_value(serde_json::json!({
            "action": "rerunMask",
            "mask_id": "r1-m1",
            "kind": "engine",
            "engine": "cloud",
        }))
        .unwrap();
        assert_eq!(
            rerun,
            OperationIntent::RerunMask {
                mask_id: "r1-m1".into(),
                kind: "engine".into(),
                engine: Some("cloud".into()),
            }
        );
        assert!(
            serde_json::from_value::<OperationIntent>(serde_json::json!({
                "action": "rerunMask",
                "maskId": "r1-m1",
                "kind": "engine",
            }))
            .is_err()
        );
        assert_eq!(
            serde_json::from_value::<OperationIntent>(
                serde_json::json!({ "action": "cleanAnyway" })
            )
            .unwrap(),
            OperationIntent::CleanAnyway
        );
    }
}
