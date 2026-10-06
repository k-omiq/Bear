//! Provider-neutral cloud wire contract schemas, validation, and canonical digest.
//!
//! This module defines the offline wire protocol (`/mc/v1`) for Manga Cleaner cloud diffusion
//! inference across both Modal and Beam backends.
//!
//! ## Invariants
//!
//! 1. **Crop-Only Transmission & Zero Coordinate Leakage:** Only tightly cropped raster bounds
//!    are transmitted. Request metadata strictly forbids full page coordinates (`x`, `y`,
//!    `page_x`, `page_y`, `strip_rect`, `on_page`), region IDs, or filesystem paths.
//! 2. **Provider-Agnostic Gateway:** Both Modal and Beam wrappers expose this uniform
//!    `/mc/v1` REST interface. Provider-native container lifecycle and queuing details
//!    are encapsulated behind the gateway.
//! 3. **Exact Response Binding & Semantic Validation:** Server responses (`202 Accepted`, status
//!    queries, result downloads, and cancellations) must bind `handle`, `job_id`, `attempt_id`,
//!    `request_digest`, and full recipe/model identity (including `native_mask_conditioning`)
//!    matching the original request metadata, while executing per-message semantic validation.
//! 4. **Deterministic Canonical Digest (`MC-REQ-V1`):** Digest computation uses an exact,
//!    versioned JSON ordered array over restricted ASCII strings, integers, and booleans.
//!    This eliminates delimiter collision ambiguities and floating-point variances across languages.
//! 5. **Fail-Closed Validation:** Unknown versions, unexpected fields, mutable revision tags
//!    (e.g., `main`, `latest`, `dev`), non-hex revisions, control/whitespace/empty IDs,
//!    unaligned dimensions, non-finite costs, or out-of-bound limits fail closed.
//! 6. **Honest Capability Reporting:** Model info endpoints must report `native_mask_conditioning = false`
//!    honestly for current FLUX diffusion recipes. Requests claiming native mask conditioning are rejected.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::engines::render::{CloudProvider, RenderRecipe, LATENT_STRIDE, MAX_CROP_PIXELS, MAX_CROP_SIDE};

pub const PROTOCOL_VERSION: &str = "1.0.0";

const ALLOWLISTED_REJECTION_CODES: &[&str] = &[
    "unsupported_recipe",
    "unsupported_model",
    "invalid_dimensions",
    "payload_too_large",
    "invalid_digest",
    "unknown_protocol_version",
    "forbidden_fields",
    "invalid_image",
    "weights_missing",
    "weights_corrupt",
    "rate_limited_pre_queue",
    "service_unavailable_pre_queue",
];

const RETRYABLE_PRE_ENQUEUE_CODES: &[&str] = &[
    "rate_limited_pre_queue",
    "service_unavailable_pre_queue",
];

#[derive(Debug, Error, PartialEq)]
pub enum WireValidationError {
    #[error("unsupported protocol version: expected {expected}, got {actual}")]
    UnsupportedProtocolVersion {
        expected: &'static str,
        actual: String,
    },

    #[error("field '{field}' contains invalid ASCII identifier: '{value}'")]
    InvalidAsciiIdentifier {
        field: &'static str,
        value: String,
    },

    #[error("field '{field}' contains invalid canonical lowercase hex hash: '{value}', expected length {expected_len}")]
    InvalidHexHash {
        field: &'static str,
        value: String,
        expected_len: usize,
    },

    #[error("width {width} or height {height} exceeds bounds (max_w: {max_w}, max_h: {max_h})")]
    DimensionOutOfBounds {
        width: u32,
        height: u32,
        max_w: u32,
        max_h: u32,
    },

    #[error("dimensions {width}x{height} not aligned to latent stride {stride}")]
    DimensionNotSnapped {
        width: u32,
        height: u32,
        stride: u32,
    },

    #[error("pixel count {pixels} exceeds maximum {max_pixels}")]
    PixelCountExceeded { pixels: u64, max_pixels: u64 },

    #[error("arithmetic overflow computing pixel count for {width}x{height}")]
    PixelCountOverflow { width: u32, height: u32 },

    #[error("payload byte length {actual} exceeds limit {limit}")]
    PayloadSizeExceeded { actual: u64, limit: u64 },

    #[error("image checksum mismatch: declared {declared}, computed {computed}")]
    ImageChecksumMismatch { declared: String, computed: String },

    #[error("hint checksum mismatch: declared {declared}, computed {computed}")]
    HintChecksumMismatch { declared: String, computed: String },

    #[error("request digest mismatch: declared {declared}, computed {computed}")]
    RequestDigestMismatch { declared: String, computed: String },

    #[error("recipe compatibility mismatch for field '{field}': requested '{requested}', model-info '{model_info}'")]
    RecipeMismatch {
        field: &'static str,
        requested: String,
        model_info: String,
    },

    #[error("unsupported capability '{0}' requested or advertised")]
    UnsupportedCapability(&'static str),

    #[error("response binding mismatch on '{field}': request had '{request_val}', response had '{response_val}'")]
    ResponseBindingMismatch {
        field: &'static str,
        request_val: String,
        response_val: String,
    },

    #[error("result byte length {actual} does not match declared metadata {declared}")]
    ResultByteLengthMismatch { actual: u64, declared: u64 },

    #[error("result digest mismatch: declared {declared}, computed {computed}")]
    ResultDigestMismatch { declared: String, computed: String },

    #[error("cancel response must have nonterminal status 'cancel_requested', got '{0}'")]
    NonterminalCancelStatus(String),

    #[error("cancel response acknowledged must be true")]
    CancelNotAcknowledged,

    #[error("job accepted response must have status 'pending', got '{0:?}'")]
    InvalidAcceptedStatus(JobExecutionStatus),

    #[error("revision '{0}' is invalid; revision must be an explicit immutable 40 or 64 lowercase hex hash")]
    MutableRevisionForbidden(String),

    #[error("invalid PNG signature")]
    InvalidPngSignature,

    #[error("invalid PNG IHDR header: {0}")]
    InvalidPngHeader(String),

    #[error("PNG geometry mismatch: expected {expected_w}x{expected_h}, got {actual_w}x{actual_h}")]
    PngGeometryMismatch {
        expected_w: u32,
        expected_h: u32,
        actual_w: u32,
        actual_h: u32,
    },

    #[error("unsupported PNG bit depth: {0}, expected 8")]
    UnsupportedBitDepth(u8),

    #[error("PNG color type mismatch: expected {expected}, got {actual}")]
    PngColorTypeMismatch { expected: u8, actual: u8 },

    #[error("PNG IHDR CRC-32 checksum mismatch: expected {expected}, computed {computed}")]
    PngCrcMismatch { expected: u32, computed: u32 },

    #[error("pre-enqueue rejection disposition error: enqueued must be false, got true")]
    InvalidRejectionEnqueuedStatus,

    #[error("unknown rejection error code: '{0}'")]
    UnknownRejectionCode(String),

    #[error("invalid service limits: field '{field}' must be positive")]
    InvalidServiceLimits { field: &'static str },

    #[error("parameter '{field}' value {value} out of allowed range [{min}..{max}]")]
    ParameterOutOfRange {
        field: &'static str,
        value: i64,
        min: i64,
        max: i64,
    },

    #[error("reported cost '{0}' must be a finite non-negative number")]
    InvalidReportedCost(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngColorMode {
    Gray8,
    Rgb8,
}

/// Validate that an identifier is non-empty, within bounds, and printable ASCII without whitespace/control.
pub fn validate_ascii_id(
    s: &str,
    field: &'static str,
    min_len: usize,
    max_len: usize,
) -> Result<(), WireValidationError> {
    if s.len() < min_len
        || s.len() > max_len
        || !s
            .chars()
            .all(|c| c.is_ascii() && !c.is_ascii_control() && !c.is_whitespace())
    {
        return Err(WireValidationError::InvalidAsciiIdentifier {
            field,
            value: s.to_string(),
        });
    }
    Ok(())
}

/// Validate that a hash string is strictly canonical lowercase hexadecimal of the expected length.
pub fn validate_lowercase_hex_hash(
    h: &str,
    field: &'static str,
    expected_len: usize,
) -> Result<(), WireValidationError> {
    if h.len() != expected_len
        || !h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(WireValidationError::InvalidHexHash {
            field,
            value: h.to_string(),
            expected_len,
        });
    }
    Ok(())
}

/// Validate that a revision identifier is an explicit immutable 40 or 64 lowercase hex hash.
pub fn validate_revision_identity(rev: &str) -> Result<(), WireValidationError> {
    let len = rev.len();
    if (len != 40 && len != 64)
        || !rev
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(WireValidationError::MutableRevisionForbidden(
            rev.to_string(),
        ));
    }
    Ok(())
}

/// Service limits for input crop dimensions, payload byte length, and timeouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceLimits {
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    pub max_png_bytes: u64,
    pub max_multipart_bytes: u64,
    pub worker_timeout_seconds: u32,
}

/// Provisional test fixture limits. Production limits remain unmeasured until P5 GPU benchmarks.
/// The crop bounds are the ones the crop preparation sizes its context to
/// ([`crate::engines::render::Preprocessing::context_for`]).
pub fn provisional_fixture_limits() -> ServiceLimits {
    ServiceLimits {
        max_width: MAX_CROP_SIDE,
        max_height: MAX_CROP_SIDE,
        max_pixels: MAX_CROP_PIXELS,    // 2048 * 2048
        max_png_bytes: 16 * 1024 * 1024, // 16 MiB
        max_multipart_bytes: 32 * 1024 * 1024, // 32 MiB
        worker_timeout_seconds: 120,
    }
}

/// Validate that service limits are strictly positive.
pub fn validate_service_limits(limits: &ServiceLimits) -> Result<(), WireValidationError> {
    if limits.max_width == 0 {
        return Err(WireValidationError::InvalidServiceLimits { field: "max_width" });
    }
    if limits.max_height == 0 {
        return Err(WireValidationError::InvalidServiceLimits {
            field: "max_height",
        });
    }
    if limits.max_pixels == 0 {
        return Err(WireValidationError::InvalidServiceLimits {
            field: "max_pixels",
        });
    }
    if limits.max_png_bytes == 0 {
        return Err(WireValidationError::InvalidServiceLimits {
            field: "max_png_bytes",
        });
    }
    if limits.max_multipart_bytes == 0 {
        return Err(WireValidationError::InvalidServiceLimits {
            field: "max_multipart_bytes",
        });
    }
    if limits.worker_timeout_seconds == 0 {
        return Err(WireValidationError::InvalidServiceLimits {
            field: "worker_timeout_seconds",
        });
    }
    Ok(())
}

/// Strict wire DTO for `RenderRecipe` ensuring unknown fields are rejected closed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRenderRecipe {
    pub recipe_id: String,
    pub preprocessing_version: String,
    pub model_id: String,
    pub model_revision: String,
    pub native_mask_conditioning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qwen_edit: Option<crate::engines::render::QwenEdit>,
}

impl From<RenderRecipe> for WireRenderRecipe {
    fn from(r: RenderRecipe) -> Self {
        Self {
            recipe_id: r.recipe_id,
            preprocessing_version: r.preprocessing_version,
            model_id: r.model_id,
            model_revision: r.model_revision,
            native_mask_conditioning: r.native_mask_conditioning,
            qwen_edit: r.qwen_edit,
        }
    }
}

impl From<WireRenderRecipe> for RenderRecipe {
    fn from(w: WireRenderRecipe) -> Self {
        Self {
            recipe_id: w.recipe_id,
            preprocessing_version: w.preprocessing_version,
            model_id: w.model_id,
            model_revision: w.model_revision,
            native_mask_conditioning: w.native_mask_conditioning,
            qwen_edit: w.qwen_edit,
        }
    }
}

pub fn validate_recipe(recipe: &WireRenderRecipe) -> Result<(), WireValidationError> {
    validate_ascii_id(&recipe.recipe_id, "recipe_id", 1, 128)?;
    validate_ascii_id(
        &recipe.preprocessing_version,
        "preprocessing_version",
        1,
        128,
    )?;
    validate_ascii_id(&recipe.model_id, "model_id", 1, 128)?;
    validate_revision_identity(&recipe.model_revision)?;
    if recipe.native_mask_conditioning {
        return Err(WireValidationError::UnsupportedCapability(
            "native_mask_conditioning",
        ));
    }
    if let Some(edit) = &recipe.qwen_edit {
        if !recipe.model_id.starts_with("Disty0/Qwen-Image-Edit") || recipe.recipe_id != "mc-qwen-image-edit-2511-v4" {
            return Err(WireValidationError::UnsupportedCapability("qwen_edit"));
        }
        if edit.description.chars().count() > 500 || edit.description.chars().any(|c| c.is_control()) {
            return Err(WireValidationError::UnsupportedCapability("qwen_description"));
        }
    }
    Ok(())
}

/// Authenticated health check response schema (`GET /mc/v1/health`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthResponse {
    pub status: String,
    pub provider: CloudProvider,
    pub protocol_version: String,
}

pub fn validate_health_response(resp: &HealthResponse) -> Result<(), WireValidationError> {
    if resp.protocol_version != PROTOCOL_VERSION {
        return Err(WireValidationError::UnsupportedProtocolVersion {
            expected: PROTOCOL_VERSION,
            actual: resp.protocol_version.clone(),
        });
    }
    if resp.status != "ok" {
        return Err(WireValidationError::InvalidAcceptedStatus(
            JobExecutionStatus::Failed,
        ));
    }
    Ok(())
}

/// Authenticated model-info runtime response schema (`GET /mc/v1/model-info`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInfoResponse {
    pub protocol_version: String,
    pub provider: CloudProvider,
    pub model_id: String,
    pub model_revision: String,
    pub recipe_id: String,
    pub preprocessing_version: String,
    pub native_mask_conditioning: bool,
    pub limits: ServiceLimits,
}

pub fn validate_model_info_response(resp: &ModelInfoResponse) -> Result<(), WireValidationError> {
    if resp.protocol_version != PROTOCOL_VERSION {
        return Err(WireValidationError::UnsupportedProtocolVersion {
            expected: PROTOCOL_VERSION,
            actual: resp.protocol_version.clone(),
        });
    }
    validate_ascii_id(&resp.recipe_id, "recipe_id", 1, 128)?;
    validate_ascii_id(&resp.preprocessing_version, "preprocessing_version", 1, 128)?;
    validate_ascii_id(&resp.model_id, "model_id", 1, 128)?;
    validate_revision_identity(&resp.model_revision)?;
    if resp.native_mask_conditioning {
        return Err(WireValidationError::RecipeMismatch {
            field: "native_mask_conditioning",
            requested: "false".into(),
            model_info: "true (unsupported cloud render capability)".into(),
        });
    }
    validate_service_limits(&resp.limits)?;
    Ok(())
}

/// Request metadata for a crop rendering job (`POST /mc/v1/jobs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobRequestMetadata {
    pub protocol_version: String,
    pub job_id: String,
    pub attempt_id: String,
    pub recipe: WireRenderRecipe,
    pub width: u32,
    pub height: u32,
    pub seed: u64,
    pub steps: u32,
    pub guidance_scaled: u32,
    pub image_sha256: String,
    pub hint_sha256: String,
    pub request_digest: String,
}

pub fn validate_job_request_metadata(meta: &JobRequestMetadata) -> Result<(), WireValidationError> {
    if meta.protocol_version != PROTOCOL_VERSION {
        return Err(WireValidationError::UnsupportedProtocolVersion {
            expected: PROTOCOL_VERSION,
            actual: meta.protocol_version.clone(),
        });
    }
    validate_ascii_id(&meta.job_id, "job_id", 1, 128)?;
    validate_ascii_id(&meta.attempt_id, "attempt_id", 1, 128)?;
    validate_recipe(&meta.recipe)?;
    validate_lowercase_hex_hash(&meta.image_sha256, "image_sha256", 64)?;
    validate_lowercase_hex_hash(&meta.hint_sha256, "hint_sha256", 64)?;
    validate_lowercase_hex_hash(&meta.request_digest, "request_digest", 64)?;
    if meta.steps < 1 || meta.steps > 100 {
        return Err(WireValidationError::ParameterOutOfRange {
            field: "steps",
            value: meta.steps as i64,
            min: 1,
            max: 100,
        });
    }
    if meta.guidance_scaled > 5000 {
        return Err(WireValidationError::ParameterOutOfRange {
            field: "guidance_scaled",
            value: meta.guidance_scaled as i64,
            min: 0,
            max: 5000,
        });
    }
    Ok(())
}

/// Execution status reported by gateway or worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobExecutionStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Client-side dispatch lifecycle state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientDispatchState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    CancelRequested,
    SubmissionUnknown,
}

/// Response returned when a job is successfully enqueued (`202 Accepted`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobAcceptedResponse {
    pub handle: String,
    pub status: JobExecutionStatus,
    pub job_id: String,
    pub attempt_id: String,
    pub request_digest: String,
    pub recipe_id: String,
    pub preprocessing_version: String,
    pub model_id: String,
    pub model_revision: String,
    pub native_mask_conditioning: bool,
}

pub fn validate_job_accepted_response(
    resp: &JobAcceptedResponse,
) -> Result<(), WireValidationError> {
    if resp.status != JobExecutionStatus::Pending {
        return Err(WireValidationError::InvalidAcceptedStatus(resp.status));
    }
    validate_ascii_id(&resp.handle, "handle", 1, 128)?;
    validate_ascii_id(&resp.job_id, "job_id", 1, 128)?;
    validate_ascii_id(&resp.attempt_id, "attempt_id", 1, 128)?;
    validate_ascii_id(&resp.recipe_id, "recipe_id", 1, 128)?;
    validate_ascii_id(
        &resp.preprocessing_version,
        "preprocessing_version",
        1,
        128,
    )?;
    validate_ascii_id(&resp.model_id, "model_id", 1, 128)?;
    validate_revision_identity(&resp.model_revision)?;
    validate_lowercase_hex_hash(&resp.request_digest, "request_digest", 64)?;
    if resp.native_mask_conditioning {
        return Err(WireValidationError::UnsupportedCapability(
            "native_mask_conditioning",
        ));
    }
    Ok(())
}

/// Structured error payload when a job fails or is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedError {
    pub error_code: String,
    pub message: String,
}

/// Authoritative job status query response (`GET /mc/v1/jobs/{handle}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobStatusResponse {
    pub handle: String,
    pub job_id: String,
    pub attempt_id: String,
    pub request_digest: String,
    pub recipe_id: String,
    pub preprocessing_version: String,
    pub model_id: String,
    pub model_revision: String,
    pub native_mask_conditioning: bool,
    pub status: JobExecutionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TypedError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_bytes: Option<u64>,
}

pub fn validate_job_status_response(resp: &JobStatusResponse) -> Result<(), WireValidationError> {
    validate_ascii_id(&resp.handle, "handle", 1, 128)?;
    validate_ascii_id(&resp.job_id, "job_id", 1, 128)?;
    validate_ascii_id(&resp.attempt_id, "attempt_id", 1, 128)?;
    validate_ascii_id(&resp.recipe_id, "recipe_id", 1, 128)?;
    validate_ascii_id(
        &resp.preprocessing_version,
        "preprocessing_version",
        1,
        128,
    )?;
    validate_ascii_id(&resp.model_id, "model_id", 1, 128)?;
    validate_revision_identity(&resp.model_revision)?;
    validate_lowercase_hex_hash(&resp.request_digest, "request_digest", 64)?;
    if resp.native_mask_conditioning {
        return Err(WireValidationError::UnsupportedCapability(
            "native_mask_conditioning",
        ));
    }
    if let Some(cost) = resp.reported_cost_usd {
        if !cost.is_finite() || cost < 0.0 {
            return Err(WireValidationError::InvalidReportedCost(cost));
        }
    }
    if resp.status == JobExecutionStatus::Completed {
        if let Some(ref d) = resp.result_digest {
            validate_lowercase_hex_hash(d, "result_digest", 64)?;
        }
    }
    Ok(())
}

/// Non-terminal cancellation acknowledgement (`POST /mc/v1/jobs/{handle}/cancel`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobCancelResponse {
    pub handle: String,
    pub job_id: String,
    pub attempt_id: String,
    pub request_digest: String,
    pub status: String, // "cancel_requested"
    pub acknowledged: bool,
}

/// Authoritative pre-enqueue rejection response confirming request was not queued.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreEnqueueRejectionResponse {
    pub enqueued: bool,
    pub error_code: String,
    pub message: String,
    pub job_id: String,
    pub attempt_id: String,
    pub request_digest: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

pub fn validate_pre_enqueue_rejection(
    rejection: &PreEnqueueRejectionResponse,
) -> Result<(), WireValidationError> {
    if rejection.enqueued {
        return Err(WireValidationError::InvalidRejectionEnqueuedStatus);
    }
    validate_ascii_id(&rejection.job_id, "job_id", 1, 128)?;
    validate_ascii_id(&rejection.attempt_id, "attempt_id", 1, 128)?;
    validate_lowercase_hex_hash(&rejection.request_digest, "request_digest", 64)?;
    if !ALLOWLISTED_REJECTION_CODES.contains(&rejection.error_code.as_str()) {
        return Err(WireValidationError::UnknownRejectionCode(
            rejection.error_code.clone(),
        ));
    }
    Ok(())
}

/// Result download metadata and validation headers (`GET /mc/v1/jobs/{handle}/result`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultMetadata {
    pub handle: String,
    pub job_id: String,
    pub attempt_id: String,
    pub request_digest: String,
    pub recipe_id: String,
    pub preprocessing_version: String,
    pub model_id: String,
    pub model_revision: String,
    pub native_mask_conditioning: bool,
    pub result_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_cost_usd: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub byte_length: u64,
}

pub fn validate_result_metadata(resp: &ResultMetadata) -> Result<(), WireValidationError> {
    validate_ascii_id(&resp.handle, "handle", 1, 128)?;
    validate_ascii_id(&resp.job_id, "job_id", 1, 128)?;
    validate_ascii_id(&resp.attempt_id, "attempt_id", 1, 128)?;
    validate_ascii_id(&resp.recipe_id, "recipe_id", 1, 128)?;
    validate_ascii_id(
        &resp.preprocessing_version,
        "preprocessing_version",
        1,
        128,
    )?;
    validate_ascii_id(&resp.model_id, "model_id", 1, 128)?;
    validate_revision_identity(&resp.model_revision)?;
    validate_lowercase_hex_hash(&resp.request_digest, "request_digest", 64)?;
    validate_lowercase_hex_hash(&resp.result_digest, "result_digest", 64)?;
    if resp.native_mask_conditioning {
        return Err(WireValidationError::UnsupportedCapability(
            "native_mask_conditioning",
        ));
    }
    if let Some(cost) = resp.reported_cost_usd {
        if !cost.is_finite() || cost < 0.0 {
            return Err(WireValidationError::InvalidReportedCost(cost));
        }
    }
    if resp.width == 0 || resp.height == 0 || resp.byte_length == 0 {
        return Err(WireValidationError::DimensionOutOfBounds {
            width: resp.width,
            height: resp.height,
            max_w: u32::MAX,
            max_h: u32::MAX,
        });
    }
    Ok(())
}

/// Compute the deterministic canonical JSON ordered array for request metadata.
pub fn compute_canonical_json_array(meta: &JobRequestMetadata) -> String {
    let mut canonical_array = serde_json::json!([
        "MC-REQ-V1",
        meta.protocol_version,
        meta.job_id,
        meta.attempt_id,
        meta.recipe.recipe_id,
        meta.recipe.preprocessing_version,
        meta.recipe.model_id,
        meta.recipe.model_revision,
        meta.recipe.native_mask_conditioning,
        meta.width,
        meta.height,
        meta.seed,
        meta.steps,
        meta.guidance_scaled,
        meta.image_sha256,
        meta.hint_sha256,
    ]);
    if let Some(edit) = &meta.recipe.qwen_edit {
        canonical_array.as_array_mut().unwrap().push(serde_json::json!([edit.target, edit.description]));
    }
    serde_json::to_string(&canonical_array).expect("valid JSON array serialization")
}

/// Compute the SHA-256 hex digest over the deterministic canonical JSON array.
pub fn compute_request_digest(meta: &JobRequestMetadata) -> String {
    let canonical = compute_canonical_json_array(meta);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Bounded inspection of PNG signature, IHDR length 13, geometry, and CRC-32 checksum.
pub fn validate_png_header(
    png_bytes: &[u8],
    expected_width: u32,
    expected_height: u32,
    expected_color: PngColorMode,
) -> Result<(), WireValidationError> {
    if png_bytes.len() < 33 || &png_bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return Err(WireValidationError::InvalidPngSignature);
    }
    let chunk_len = u32::from_be_bytes(
        png_bytes[8..12]
            .try_into()
            .map_err(|_| WireValidationError::InvalidPngHeader("Truncated IHDR length".into()))?,
    );
    if chunk_len != 13 || &png_bytes[12..16] != b"IHDR" {
        return Err(WireValidationError::InvalidPngHeader(
            "First chunk must be IHDR with length 13".into(),
        ));
    }
    let width = u32::from_be_bytes(
        png_bytes[16..20]
            .try_into()
            .map_err(|_| WireValidationError::InvalidPngHeader("Truncated IHDR width".into()))?,
    );
    let height = u32::from_be_bytes(
        png_bytes[20..24]
            .try_into()
            .map_err(|_| WireValidationError::InvalidPngHeader("Truncated IHDR height".into()))?,
    );
    let bit_depth = png_bytes[24];
    let color_type = png_bytes[25];
    let compression = png_bytes[26];
    let filter_method = png_bytes[27];
    let interlace = png_bytes[28];

    if width != expected_width || height != expected_height {
        return Err(WireValidationError::PngGeometryMismatch {
            expected_w: expected_width,
            expected_h: expected_height,
            actual_w: width,
            actual_h: height,
        });
    }
    if bit_depth != 8 {
        return Err(WireValidationError::UnsupportedBitDepth(bit_depth));
    }
    let expected_ct = match expected_color {
        PngColorMode::Rgb8 => 2,
        PngColorMode::Gray8 => 0,
    };
    if color_type != expected_ct {
        return Err(WireValidationError::PngColorTypeMismatch {
            expected: expected_ct,
            actual: color_type,
        });
    }
    if compression != 0 || filter_method != 0 || interlace != 0 {
        return Err(WireValidationError::InvalidPngHeader(
            "Unsupported compression/filter/interlace method".into(),
        ));
    }

    let expected_crc = u32::from_be_bytes(
        png_bytes[29..33]
            .try_into()
            .map_err(|_| WireValidationError::InvalidPngHeader("Truncated IHDR CRC".into()))?,
    );
    let computed_crc = crc32fast::hash(&png_bytes[12..29]);
    if computed_crc != expected_crc {
        return Err(WireValidationError::PngCrcMismatch {
            expected: expected_crc,
            computed: computed_crc,
        });
    }

    Ok(())
}

/// Validate crop request metadata and associated PNG buffers using bounded arithmetic.
pub fn validate_crop_payload(
    meta: &JobRequestMetadata,
    image_bytes: &[u8],
    hint_bytes: &[u8],
    limits: &ServiceLimits,
) -> Result<(), WireValidationError> {
    validate_job_request_metadata(meta)?;
    validate_service_limits(limits)?;

    if meta.protocol_version != PROTOCOL_VERSION {
        return Err(WireValidationError::UnsupportedProtocolVersion {
            expected: PROTOCOL_VERSION,
            actual: meta.protocol_version.clone(),
        });
    }

    if meta.width == 0
        || meta.width > limits.max_width
        || meta.height == 0
        || meta.height > limits.max_height
    {
        return Err(WireValidationError::DimensionOutOfBounds {
            width: meta.width,
            height: meta.height,
            max_w: limits.max_width,
            max_h: limits.max_height,
        });
    }

    if meta.width % LATENT_STRIDE != 0 || meta.height % LATENT_STRIDE != 0 {
        return Err(WireValidationError::DimensionNotSnapped {
            width: meta.width,
            height: meta.height,
            stride: LATENT_STRIDE,
        });
    }

    let pixels = (meta.width as u64)
        .checked_mul(meta.height as u64)
        .ok_or(WireValidationError::PixelCountOverflow {
            width: meta.width,
            height: meta.height,
        })?;

    if pixels > limits.max_pixels {
        return Err(WireValidationError::PixelCountExceeded {
            pixels,
            max_pixels: limits.max_pixels,
        });
    }

    let img_len = image_bytes.len() as u64;
    let hint_len = hint_bytes.len() as u64;

    if img_len > limits.max_png_bytes {
        return Err(WireValidationError::PayloadSizeExceeded {
            actual: img_len,
            limit: limits.max_png_bytes,
        });
    }

    if hint_len > limits.max_png_bytes {
        return Err(WireValidationError::PayloadSizeExceeded {
            actual: hint_len,
            limit: limits.max_png_bytes,
        });
    }

    let total_bytes =
        img_len
            .checked_add(hint_len)
            .ok_or(WireValidationError::PayloadSizeExceeded {
                actual: u64::MAX,
                limit: limits.max_multipart_bytes,
            })?;

    if total_bytes > limits.max_multipart_bytes {
        return Err(WireValidationError::PayloadSizeExceeded {
            actual: total_bytes,
            limit: limits.max_multipart_bytes,
        });
    }

    validate_png_header(image_bytes, meta.width, meta.height, PngColorMode::Rgb8)?;
    validate_png_header(hint_bytes, meta.width, meta.height, PngColorMode::Gray8)?;

    let mut img_hasher = Sha256::new();
    img_hasher.update(image_bytes);
    let computed_img_sha = format!("{:x}", img_hasher.finalize());
    if computed_img_sha != meta.image_sha256 {
        return Err(WireValidationError::ImageChecksumMismatch {
            declared: meta.image_sha256.clone(),
            computed: computed_img_sha,
        });
    }

    let mut hint_hasher = Sha256::new();
    hint_hasher.update(hint_bytes);
    let computed_hint_sha = format!("{:x}", hint_hasher.finalize());
    if computed_hint_sha != meta.hint_sha256 {
        return Err(WireValidationError::HintChecksumMismatch {
            declared: meta.hint_sha256.clone(),
            computed: computed_hint_sha,
        });
    }

    let computed_digest = compute_request_digest(meta);
    if computed_digest != meta.request_digest {
        return Err(WireValidationError::RequestDigestMismatch {
            declared: meta.request_digest.clone(),
            computed: computed_digest,
        });
    }

    Ok(())
}

/// Pre-submit verification that requested recipe/model matches advertised model-info capabilities.
pub fn validate_recipe_compatibility(
    meta: &JobRequestMetadata,
    model_info: &ModelInfoResponse,
) -> Result<(), WireValidationError> {
    validate_model_info_response(model_info)?;
    validate_job_request_metadata(meta)?;

    if meta.recipe.recipe_id != model_info.recipe_id {
        return Err(WireValidationError::RecipeMismatch {
            field: "recipe_id",
            requested: meta.recipe.recipe_id.clone(),
            model_info: model_info.recipe_id.clone(),
        });
    }
    if meta.recipe.preprocessing_version != model_info.preprocessing_version {
        return Err(WireValidationError::RecipeMismatch {
            field: "preprocessing_version",
            requested: meta.recipe.preprocessing_version.clone(),
            model_info: model_info.preprocessing_version.clone(),
        });
    }
    if meta.recipe.model_id != model_info.model_id {
        return Err(WireValidationError::RecipeMismatch {
            field: "model_id",
            requested: meta.recipe.model_id.clone(),
            model_info: model_info.model_id.clone(),
        });
    }
    if meta.recipe.model_revision != model_info.model_revision {
        return Err(WireValidationError::RecipeMismatch {
            field: "model_revision",
            requested: meta.recipe.model_revision.clone(),
            model_info: model_info.model_revision.clone(),
        });
    }
    if model_info.native_mask_conditioning {
        return Err(WireValidationError::RecipeMismatch {
            field: "native_mask_conditioning",
            requested: "false (required worker ceiling)".into(),
            model_info: "true (unsupported cloud render capability)".into(),
        });
    }
    if meta.recipe.native_mask_conditioning {
        return Err(WireValidationError::RecipeMismatch {
            field: "native_mask_conditioning",
            requested: "true".into(),
            model_info: "false".into(),
        });
    }
    Ok(())
}

/// Trait providing access to bound fields and semantic validation across responses.
pub trait BoundResponse {
    fn validate_semantics(&self) -> Result<(), WireValidationError>;
    fn handle(&self) -> &str;
    fn job_id(&self) -> &str;
    fn attempt_id(&self) -> &str;
    fn request_digest(&self) -> &str;
    fn recipe_id(&self) -> &str;
    fn preprocessing_version(&self) -> &str;
    fn model_id(&self) -> &str;
    fn model_revision(&self) -> &str;
    fn native_mask_conditioning(&self) -> bool;
}

impl BoundResponse for JobAcceptedResponse {
    fn validate_semantics(&self) -> Result<(), WireValidationError> {
        validate_job_accepted_response(self)
    }
    fn handle(&self) -> &str {
        &self.handle
    }
    fn job_id(&self) -> &str {
        &self.job_id
    }
    fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    fn request_digest(&self) -> &str {
        &self.request_digest
    }
    fn recipe_id(&self) -> &str {
        &self.recipe_id
    }
    fn preprocessing_version(&self) -> &str {
        &self.preprocessing_version
    }
    fn model_id(&self) -> &str {
        &self.model_id
    }
    fn model_revision(&self) -> &str {
        &self.model_revision
    }
    fn native_mask_conditioning(&self) -> bool {
        self.native_mask_conditioning
    }
}

impl BoundResponse for JobStatusResponse {
    fn validate_semantics(&self) -> Result<(), WireValidationError> {
        validate_job_status_response(self)
    }
    fn handle(&self) -> &str {
        &self.handle
    }
    fn job_id(&self) -> &str {
        &self.job_id
    }
    fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    fn request_digest(&self) -> &str {
        &self.request_digest
    }
    fn recipe_id(&self) -> &str {
        &self.recipe_id
    }
    fn preprocessing_version(&self) -> &str {
        &self.preprocessing_version
    }
    fn model_id(&self) -> &str {
        &self.model_id
    }
    fn model_revision(&self) -> &str {
        &self.model_revision
    }
    fn native_mask_conditioning(&self) -> bool {
        self.native_mask_conditioning
    }
}

impl BoundResponse for ResultMetadata {
    fn validate_semantics(&self) -> Result<(), WireValidationError> {
        validate_result_metadata(self)
    }
    fn handle(&self) -> &str {
        &self.handle
    }
    fn job_id(&self) -> &str {
        &self.job_id
    }
    fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    fn request_digest(&self) -> &str {
        &self.request_digest
    }
    fn recipe_id(&self) -> &str {
        &self.recipe_id
    }
    fn preprocessing_version(&self) -> &str {
        &self.preprocessing_version
    }
    fn model_id(&self) -> &str {
        &self.model_id
    }
    fn model_revision(&self) -> &str {
        &self.model_revision
    }
    fn native_mask_conditioning(&self) -> bool {
        self.native_mask_conditioning
    }
}

/// Verify exact binding and message semantics between original request metadata and the response.
pub fn validate_response_binding<T: BoundResponse>(
    meta: &JobRequestMetadata,
    resp: &T,
    expected_handle: Option<&str>,
) -> Result<(), WireValidationError> {
    validate_job_request_metadata(meta)?;
    resp.validate_semantics()?;

    if let Some(h) = expected_handle {
        if resp.handle() != h {
            return Err(WireValidationError::ResponseBindingMismatch {
                field: "handle",
                request_val: h.to_string(),
                response_val: resp.handle().to_string(),
            });
        }
    }
    if resp.job_id() != meta.job_id {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "job_id",
            request_val: meta.job_id.clone(),
            response_val: resp.job_id().to_string(),
        });
    }
    if resp.attempt_id() != meta.attempt_id {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "attempt_id",
            request_val: meta.attempt_id.clone(),
            response_val: resp.attempt_id().to_string(),
        });
    }
    if resp.request_digest() != meta.request_digest {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "request_digest",
            request_val: meta.request_digest.clone(),
            response_val: resp.request_digest().to_string(),
        });
    }
    if resp.recipe_id() != meta.recipe.recipe_id {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "recipe_id",
            request_val: meta.recipe.recipe_id.clone(),
            response_val: resp.recipe_id().to_string(),
        });
    }
    if resp.preprocessing_version() != meta.recipe.preprocessing_version {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "preprocessing_version",
            request_val: meta.recipe.preprocessing_version.clone(),
            response_val: resp.preprocessing_version().to_string(),
        });
    }
    if resp.model_id() != meta.recipe.model_id {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "model_id",
            request_val: meta.recipe.model_id.clone(),
            response_val: resp.model_id().to_string(),
        });
    }
    if resp.model_revision() != meta.recipe.model_revision {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "model_revision",
            request_val: meta.recipe.model_revision.clone(),
            response_val: resp.model_revision().to_string(),
        });
    }
    if resp.native_mask_conditioning() != meta.recipe.native_mask_conditioning {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "native_mask_conditioning",
            request_val: meta.recipe.native_mask_conditioning.to_string(),
            response_val: resp.native_mask_conditioning().to_string(),
        });
    }
    Ok(())
}

/// Validate cancel response non-terminal status, acknowledgement, digest, and identity binding.
pub fn validate_cancel_response(
    meta: &JobRequestMetadata,
    resp: &JobCancelResponse,
    expected_handle: &str,
) -> Result<(), WireValidationError> {
    validate_job_request_metadata(meta)?;
    validate_ascii_id(&resp.handle, "handle", 1, 128)?;
    validate_ascii_id(&resp.job_id, "job_id", 1, 128)?;
    validate_ascii_id(&resp.attempt_id, "attempt_id", 1, 128)?;
    validate_lowercase_hex_hash(&resp.request_digest, "request_digest", 64)?;

    if resp.handle != expected_handle {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "handle",
            request_val: expected_handle.to_string(),
            response_val: resp.handle.clone(),
        });
    }
    if resp.job_id != meta.job_id {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "job_id",
            request_val: meta.job_id.clone(),
            response_val: resp.job_id.clone(),
        });
    }
    if resp.attempt_id != meta.attempt_id {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "attempt_id",
            request_val: meta.attempt_id.clone(),
            response_val: resp.attempt_id.clone(),
        });
    }
    if resp.request_digest != meta.request_digest {
        return Err(WireValidationError::ResponseBindingMismatch {
            field: "request_digest",
            request_val: meta.request_digest.clone(),
            response_val: resp.request_digest.clone(),
        });
    }
    if resp.status != "cancel_requested" {
        return Err(WireValidationError::NonterminalCancelStatus(
            resp.status.clone(),
        ));
    }
    if !resp.acknowledged {
        return Err(WireValidationError::CancelNotAcknowledged);
    }
    Ok(())
}

/// Authoritatively verify structured pre-enqueue rejection before safe retry classification.
pub fn is_authoritative_safe_to_retry(
    rejection: &PreEnqueueRejectionResponse,
    meta: &JobRequestMetadata,
) -> bool {
    if validate_pre_enqueue_rejection(rejection).is_err() {
        return false;
    }
    if validate_job_request_metadata(meta).is_err() {
        return false;
    }
    if rejection.enqueued {
        return false;
    }
    if rejection.job_id != meta.job_id
        || rejection.attempt_id != meta.attempt_id
        || rejection.request_digest != meta.request_digest
    {
        return false;
    }
    if !RETRYABLE_PRE_ENQUEUE_CODES.contains(&rejection.error_code.as_str()) {
        return false;
    }
    rejection.retryable
}

/// Validate result payload bytes against result metadata, request bounds, and service limits.
pub fn validate_result_bytes(
    result_bytes: &[u8],
    result_meta: &ResultMetadata,
    request_meta: &JobRequestMetadata,
    limits: &ServiceLimits,
    expected_handle: &str,
) -> Result<(), WireValidationError> {
    validate_service_limits(limits)?;
    validate_response_binding(request_meta, result_meta, Some(expected_handle))?;

    if result_meta.width != request_meta.width || result_meta.height != request_meta.height {
        return Err(WireValidationError::PngGeometryMismatch {
            expected_w: request_meta.width,
            expected_h: request_meta.height,
            actual_w: result_meta.width,
            actual_h: result_meta.height,
        });
    }

    if result_meta.width == 0
        || result_meta.width > limits.max_width
        || result_meta.height == 0
        || result_meta.height > limits.max_height
    {
        return Err(WireValidationError::DimensionOutOfBounds {
            width: result_meta.width,
            height: result_meta.height,
            max_w: limits.max_width,
            max_h: limits.max_height,
        });
    }

    if result_meta.width % LATENT_STRIDE != 0 || result_meta.height % LATENT_STRIDE != 0 {
        return Err(WireValidationError::DimensionNotSnapped {
            width: result_meta.width,
            height: result_meta.height,
            stride: LATENT_STRIDE,
        });
    }

    let pixels = (result_meta.width as u64)
        .checked_mul(result_meta.height as u64)
        .ok_or(WireValidationError::PixelCountOverflow {
            width: result_meta.width,
            height: result_meta.height,
        })?;

    if pixels > limits.max_pixels {
        return Err(WireValidationError::PixelCountExceeded {
            pixels,
            max_pixels: limits.max_pixels,
        });
    }

    if let Some(cost) = result_meta.reported_cost_usd {
        if !cost.is_finite() || cost < 0.0 {
            return Err(WireValidationError::InvalidReportedCost(cost));
        }
    }

    let actual_len = result_bytes.len() as u64;
    if actual_len > limits.max_png_bytes {
        return Err(WireValidationError::PayloadSizeExceeded {
            actual: actual_len,
            limit: limits.max_png_bytes,
        });
    }
    if actual_len != result_meta.byte_length {
        return Err(WireValidationError::ResultByteLengthMismatch {
            actual: actual_len,
            declared: result_meta.byte_length,
        });
    }

    validate_png_header(
        result_bytes,
        result_meta.width,
        result_meta.height,
        PngColorMode::Rgb8,
    )?;

    let mut hasher = Sha256::new();
    hasher.update(result_bytes);
    let computed_digest = format!("{:x}", hasher.finalize());
    if computed_digest != result_meta.result_digest {
        return Err(WireValidationError::ResultDigestMismatch {
            declared: result_meta.result_digest.clone(),
            computed: computed_digest,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qwen_guidance_digest_matches_python_and_binds_unicode_description() {
        let mut meta: JobRequestMetadata = serde_json::from_str(include_str!("../../../deploy/cloud/fixtures/qwen_guidance_request.json")).unwrap();
        validate_job_request_metadata(&meta).unwrap();
        assert_eq!(RenderRecipe::from(meta.recipe.clone()).sampling().steps, 8);
        assert_eq!(compute_request_digest(&meta), meta.request_digest);
        let initial = meta.request_digest.clone();
        meta.recipe.qwen_edit.as_mut().unwrap().description.clear();
        assert_ne!(compute_request_digest(&meta), initial);
        meta.recipe.qwen_edit.as_mut().unwrap().target = crate::engines::render::QwenTarget::Dialogue;
        assert_ne!(compute_request_digest(&meta), initial);
        meta.recipe.qwen_edit.as_mut().unwrap().description = "x".repeat(501);
        assert!(validate_job_request_metadata(&meta).is_err());
        meta.recipe.qwen_edit.as_mut().unwrap().description.clear();
        meta.recipe.recipe_id = "mc-qwen-image-edit-2511-v3".into();
        assert!(validate_job_request_metadata(&meta).is_err());
    }

    const HEALTH_FIXTURE: &str = include_str!("../../../deploy/cloud/fixtures/health_response.json");
    const MODEL_INFO_FIXTURE: &str =
        include_str!("../../../deploy/cloud/fixtures/model_info_response.json");
    const JOB_REQUEST_FIXTURE: &str =
        include_str!("../../../deploy/cloud/fixtures/job_request_metadata.json");
    const JOB_ACCEPTED_FIXTURE: &str =
        include_str!("../../../deploy/cloud/fixtures/job_accepted_response.json");
    const JOB_STATUS_PENDING: &str =
        include_str!("../../../deploy/cloud/fixtures/job_status_pending.json");
    const JOB_STATUS_RUNNING: &str =
        include_str!("../../../deploy/cloud/fixtures/job_status_running.json");
    const JOB_STATUS_COMPLETED: &str =
        include_str!("../../../deploy/cloud/fixtures/job_status_completed.json");
    const JOB_STATUS_FAILED: &str =
        include_str!("../../../deploy/cloud/fixtures/job_status_failed.json");
    const JOB_CANCEL_FIXTURE: &str =
        include_str!("../../../deploy/cloud/fixtures/job_cancel_response.json");
    const REJECTION_FIXTURE: &str =
        include_str!("../../../deploy/cloud/fixtures/pre_enqueue_rejection.json");
    const DIGEST_VECTOR_FIXTURE: &str =
        include_str!("../../../deploy/cloud/fixtures/canonical_digest_vector.json");

    const TINY_IMAGE_BYTES: &[u8] =
        include_bytes!("../../../deploy/cloud/fixtures/tiny_image.png");
    const TINY_HINT_BYTES: &[u8] =
        include_bytes!("../../../deploy/cloud/fixtures/tiny_hint.png");

    #[test]
    fn test_health_schema() {
        let resp: HealthResponse = serde_json::from_str(HEALTH_FIXTURE).unwrap();
        validate_health_response(&resp).unwrap();
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.provider, CloudProvider::Modal);
        assert_eq!(resp.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn test_model_info_schema_and_native_mask_honesty() {
        let resp: ModelInfoResponse = serde_json::from_str(MODEL_INFO_FIXTURE).unwrap();
        validate_model_info_response(&resp).unwrap();
        assert_eq!(resp.protocol_version, PROTOCOL_VERSION);
        assert_eq!(resp.model_id, "test-flux-schnell");
        assert_eq!(
            resp.model_revision,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(resp.recipe_id, "test-sdnq-v1");
        // Must honestly declare native mask conditioning is false
        assert!(!resp.native_mask_conditioning);
        assert_eq!(resp.limits.max_width, 2048);
        assert_eq!(resp.limits.max_height, 2048);
    }

    #[test]
    fn test_canonical_digest_vector_and_collision_resistance() {
        let meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        validate_job_request_metadata(&meta).unwrap();
        #[derive(Deserialize)]
        struct CollisionCase {
            canonical_json: String,
            expected_sha256: String,
        }
        #[derive(Deserialize)]
        struct Vector {
            canonical_json: String,
            expected_sha256: String,
            collision_case_a: CollisionCase,
            collision_case_b: CollisionCase,
        }
        let vector: Vector = serde_json::from_str(DIGEST_VECTOR_FIXTURE).unwrap();

        let canonical = compute_canonical_json_array(&meta);
        assert_eq!(canonical, vector.canonical_json);

        let digest = compute_request_digest(&meta);
        assert_eq!(digest, vector.expected_sha256);
        assert_eq!(digest, meta.request_digest);

        // Collision resistance: test job_id="a|b", attempt_id="c" vs job_id="a", attempt_id="b|c"
        let mut meta_a = meta.clone();
        meta_a.job_id = "a|b".into();
        meta_a.attempt_id = "c".into();

        let mut meta_b = meta.clone();
        meta_b.job_id = "a".into();
        meta_b.attempt_id = "b|c".into();

        let digest_a = compute_request_digest(&meta_a);
        let digest_b = compute_request_digest(&meta_b);
        assert_ne!(digest_a, digest_b);
        assert_eq!(compute_canonical_json_array(&meta_a), vector.collision_case_a.canonical_json);
        assert_eq!(compute_canonical_json_array(&meta_b), vector.collision_case_b.canonical_json);
        assert_eq!(digest_a, vector.collision_case_a.expected_sha256);
        assert_eq!(digest_b, vector.collision_case_b.expected_sha256);
    }

    #[test]
    fn test_crop_payload_validation_success() {
        let meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        let limits = provisional_fixture_limits();
        validate_crop_payload(&meta, TINY_IMAGE_BYTES, TINY_HINT_BYTES, &limits).unwrap();
    }

    #[test]
    fn test_crop_payload_rejects_forbidden_fields() {
        let bad_json = r#"{
            "protocol_version": "1.0.0",
            "job_id": "job-test-001",
            "attempt_id": "attempt-test-001",
            "recipe": {
                "recipe_id": "test-sdnq-v1",
                "preprocessing_version": "1.0.0",
                "model_id": "test-flux-schnell",
                "model_revision": "0123456789abcdef0123456789abcdef01234567",
                "native_mask_conditioning": false
            },
            "width": 16,
            "height": 16,
            "seed": 42,
            "steps": 4,
            "guidance_scaled": 350,
            "image_sha256": "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f",
            "hint_sha256": "3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238",
            "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0",
            "page_x": 100
        }"#;

        let result: Result<JobRequestMetadata, _> = serde_json::from_str(bad_json);
        assert!(result.is_err());
    }

    #[test]
    fn test_crop_payload_rejects_nested_recipe_unknown_fields() {
        let bad_json = r#"{
            "protocol_version": "1.0.0",
            "job_id": "job-test-001",
            "attempt_id": "attempt-test-001",
            "recipe": {
                "recipe_id": "test-sdnq-v1",
                "preprocessing_version": "1.0.0",
                "model_id": "test-flux-schnell",
                "model_revision": "0123456789abcdef0123456789abcdef01234567",
                "native_mask_conditioning": false,
                "unexpected_nested_key": "fail_closed"
            },
            "width": 16,
            "height": 16,
            "seed": 42,
            "steps": 4,
            "guidance_scaled": 350,
            "image_sha256": "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f",
            "hint_sha256": "3552bb8bf493ba63896f7abfb3ee3d899bf7312cd0b3d87691a639344b051238",
            "request_digest": "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0"
        }"#;

        let result: Result<JobRequestMetadata, _> = serde_json::from_str(bad_json);
        assert!(result.is_err());
    }

    #[test]
    fn test_mutable_revision_rejection() {
        for mutable in [
            "main",
            "master",
            "latest",
            "head",
            "dev",
            "staging",
            "not-a-40-hex-hash",
        ] {
            let mut meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
            meta.recipe.model_revision = mutable.to_string();
            let limits = provisional_fixture_limits();
            let err = validate_crop_payload(&meta, TINY_IMAGE_BYTES, TINY_HINT_BYTES, &limits)
                .unwrap_err();
            assert!(matches!(
                err,
                WireValidationError::MutableRevisionForbidden(_)
            ));
        }
    }

    #[test]
    fn test_parameter_bounds_rejection() {
        let mut meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        meta.steps = 0;
        let limits = provisional_fixture_limits();
        assert!(validate_crop_payload(&meta, TINY_IMAGE_BYTES, TINY_HINT_BYTES, &limits).is_err());

        meta.steps = 101;
        assert!(validate_crop_payload(&meta, TINY_IMAGE_BYTES, TINY_HINT_BYTES, &limits).is_err());

        meta.steps = 4;
        meta.guidance_scaled = 5001;
        assert!(validate_crop_payload(&meta, TINY_IMAGE_BYTES, TINY_HINT_BYTES, &limits).is_err());
    }

    #[test]
    fn test_accepted_status_pending_only() {
        let mut accepted: JobAcceptedResponse =
            serde_json::from_str(JOB_ACCEPTED_FIXTURE).unwrap();
        validate_job_accepted_response(&accepted).unwrap();

        accepted.status = JobExecutionStatus::Completed;
        assert!(validate_job_accepted_response(&accepted).is_err());

        let meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        assert!(
            validate_response_binding(&meta, &accepted, Some("handle-test-modal-999")).is_err()
        );
    }

    #[test]
    fn test_job_status_cost_validation() {
        let mut status: JobStatusResponse = serde_json::from_str(JOB_STATUS_COMPLETED).unwrap();
        validate_job_status_response(&status).unwrap();

        status.reported_cost_usd = Some(-0.01);
        assert!(validate_job_status_response(&status).is_err());

        status.reported_cost_usd = Some(f64::NAN);
        assert!(validate_job_status_response(&status).is_err());

        status.reported_cost_usd = Some(f64::INFINITY);
        assert!(validate_job_status_response(&status).is_err());
    }

    #[test]
    fn test_crop_payload_validation_unsnapped_stride() {
        let mut meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        meta.width = 15; // not 16 stride
        let limits = provisional_fixture_limits();
        let err = validate_crop_payload(&meta, TINY_IMAGE_BYTES, TINY_HINT_BYTES, &limits)
            .unwrap_err();
        assert_eq!(
            err,
            WireValidationError::DimensionNotSnapped {
                width: 15,
                height: 16,
                stride: 16
            }
        );
    }

    #[test]
    fn test_recipe_compatibility_check() {
        let meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        let model_info: ModelInfoResponse = serde_json::from_str(MODEL_INFO_FIXTURE).unwrap();

        validate_recipe_compatibility(&meta, &model_info).unwrap();

        let mut bad_info = model_info.clone();
        bad_info.model_id = "other-flux-model".to_string();
        let err = validate_recipe_compatibility(&meta, &bad_info).unwrap_err();
        assert!(matches!(
            err,
            WireValidationError::RecipeMismatch {
                field: "model_id",
                ..
            }
        ));
    }

    #[test]
    fn test_response_binding() {
        let meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        let accepted: JobAcceptedResponse = serde_json::from_str(JOB_ACCEPTED_FIXTURE).unwrap();

        validate_response_binding(&meta, &accepted, Some("handle-test-modal-999")).unwrap();

        let mut bad_accepted = accepted.clone();
        bad_accepted.job_id = "job-different".to_string();
        let err =
            validate_response_binding(&meta, &bad_accepted, Some("handle-test-modal-999"))
                .unwrap_err();
        assert!(matches!(
            err,
            WireValidationError::ResponseBindingMismatch {
                field: "job_id",
                ..
            }
        ));
    }

    #[test]
    fn test_cancel_acknowledgement() {
        let cancel: JobCancelResponse = serde_json::from_str(JOB_CANCEL_FIXTURE).unwrap();
        let meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        validate_cancel_response(&meta, &cancel, "handle-test-modal-999").unwrap();

        let bad_cancel = JobCancelResponse {
            handle: "handle-test-modal-999".into(),
            job_id: meta.job_id.clone(),
            attempt_id: meta.attempt_id.clone(),
            request_digest: meta.request_digest.clone(),
            status: "cancelled".into(), // terminal status directly returned by cancel is rejected
            acknowledged: true,
        };
        assert!(validate_cancel_response(&meta, &bad_cancel, "handle-test-modal-999").is_err());
    }

    #[test]
    fn test_pre_enqueue_rejection_and_safe_retry() {
        let rejection: PreEnqueueRejectionResponse =
            serde_json::from_str(REJECTION_FIXTURE).unwrap();
        validate_pre_enqueue_rejection(&rejection).unwrap();
        assert!(!rejection.enqueued);
        assert_eq!(rejection.error_code, "unsupported_recipe");
        assert!(!rejection.retryable);

        let meta: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        assert!(!is_authoritative_safe_to_retry(&rejection, &meta));

        let retryable = PreEnqueueRejectionResponse {
            enqueued: false,
            error_code: "rate_limited_pre_queue".into(),
            message: "Temporary load shed".into(),
            job_id: meta.job_id.clone(),
            attempt_id: meta.attempt_id.clone(),
            request_digest: meta.request_digest.clone(),
            retryable: true,
            details: None,
        };
        assert!(is_authoritative_safe_to_retry(&retryable, &meta));
    }

    #[test]
    fn test_result_bytes_validation_and_request_binding() {
        let request_meta: JobRequestMetadata =
            serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        let meta = ResultMetadata {
            handle: "handle-test-modal-999".into(),
            job_id: "job-test-001".into(),
            attempt_id: "attempt-test-001".into(),
            request_digest:
                "42174589391d9064c743c4450b520f403a3b91171382c4eadc9934285f21efc0".into(),
            recipe_id: "test-sdnq-v1".into(),
            preprocessing_version: "1.0.0".into(),
            model_id: "test-flux-schnell".into(),
            model_revision: "0123456789abcdef0123456789abcdef01234567".into(),
            native_mask_conditioning: false,
            result_digest:
                "2e0a9a2d5f28563463f634d5b81ec2ef90d91afe472a0c6e2c6ef4082f93ec5f".into(),
            reported_cost_usd: Some(0.0012),
            width: 16,
            height: 16,
            byte_length: TINY_IMAGE_BYTES.len() as u64,
        };
        let limits = provisional_fixture_limits();
        validate_result_bytes(
            TINY_IMAGE_BYTES,
            &meta,
            &request_meta,
            &limits,
            "handle-test-modal-999",
        )
        .unwrap();

        // Mismatched width with request
        let mut bad_meta = meta.clone();
        bad_meta.width = 32;
        let err = validate_result_bytes(
            TINY_IMAGE_BYTES,
            &bad_meta,
            &request_meta,
            &limits,
            "handle-test-modal-999",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            WireValidationError::PngGeometryMismatch { .. }
        ));
    }
    #[test]
    fn shared_status_fixtures_validate_and_bind() {
        let request: JobRequestMetadata = serde_json::from_str(JOB_REQUEST_FIXTURE).unwrap();
        for fixture in [JOB_STATUS_PENDING, JOB_STATUS_RUNNING, JOB_STATUS_FAILED] {
            let status: JobStatusResponse = serde_json::from_str(fixture).unwrap();
            validate_response_binding(&request, &status, Some("handle-test-modal-999")).unwrap();
        }
    }

}
