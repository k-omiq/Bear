//! Inference authorization grant policy, cryptographic nonces, and bounded scope enforcement.
//!
//! Provides backend-issued, tamper-evident authorization grants for cloud rendering.
//! Every remote cloud inference operation must consume an unexpired, unrevoked grant
//! whose immutable scope exactly matches the target provider, profile, canonical endpoint fingerprint,
//! crop SHA-256 digest, crop geometry, source/mask hashes, and recipe revision.
//!
//! ## Invariants
//!
//! 1. **Cryptographically Random Nonce:** Grant nonces are 256-bit (32-byte) hex strings generated
//!    directly from the operating system's cryptographic random generator via `getrandom`.
//! 2. **Backend-Only Issuance:** Grant minting is strictly a backend internal API. No frontend command
//!    can mint a grant or supply unverified hashes.
//! 3. **Exact Immutable Scope Binding:** Grant validation checks every scope field bit-for-bit:
//!    `provider`, `profile_id`, `canonical_endpoint_fingerprint`, `source_hash`, `crop_sha256`,
//!    `crop_bounds`, `mask_hash`, `revision`, `recipe`, and `region_ids`.
//! 4. **Endpoint Path Invalidation:** `canonical_endpoint_fingerprint` binds the full URL path, so
//!    changing an endpoint path within the same origin immediately invalidates issued grants.
//! 5. **Atomic Mutex Consume/Revoke:** Grant state transitions (issuance, consumption, revocation)
//!    are atomic under a dedicated mutex, guaranteeing strict single-use / attempt-limited execution.
//! 6. **Strict Expiry & Clock Rollback Handling:** Grants expire at `now >= deadline` and fail
//!    closed if system clock rollback is detected (`now < issued_at`).
//! 7. **Profile Revocation Hooks:** Updating or deleting public profiles or credentials immediately
//!    revokes all outstanding grants for that profile.
//! 8. **Sanitized Error Strings:** Grant validation errors never leak raw nonces or internal hashes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cleaner_core::engines::render::{CloudProvider, RenderRecipe};
use cleaner_core::mask::Rect;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::inference::config::validate_profile_id;

/// Maximum allowable duration for an authorization grant (5 minutes).
pub const MAX_GRANT_TTL: Duration = Duration::from_secs(300);

/// Minimum allowable duration for an authorization grant (1 second).
pub const MIN_GRANT_TTL: Duration = Duration::from_secs(1);

/// Maximum allowable attempts per grant.
pub const MAX_GRANT_ATTEMPTS: u32 = 10;

/// Maximum number of cached authorization grants held in memory.
pub const MAX_CACHED_GRANTS: usize = 256;

/// Maximum number of region IDs in a single grant scope.
pub const MAX_REGIONS_PER_GRANT: usize = 1024;

pub const FLUX_CAPABILITY: &str = "flux_render@1";

fn default_capability() -> String { FLUX_CAPABILITY.to_string() }

/// Immutable scope binding for an authorization grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantScope {
    #[serde(default = "default_capability")]
    pub capability: String,
    pub provider: CloudProvider,
    pub profile_id: String,
    pub canonical_endpoint_fingerprint: String,
    pub source_hash: String,
    pub crop_sha256: String,
    pub hint_sha256: String,
    pub operation_digest: String,
    pub crop_bounds: Rect,
    pub mask_hash: String,
    pub revision: u64,
    #[serde(default)]
    pub input_sha256: String,
    #[serde(default)]
    pub predecessors_sha256: String,
    pub recipe: RenderRecipe,
    pub region_ids: Vec<String>,
}

/// An active or historical backend-issued authorization grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub nonce: String,
    pub scope: GrantScope,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub max_attempts: u32,
    pub attempts_used: u32,
    pub revoked: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GrantError {
    #[error("grant was not found")]
    NotFound,

    #[error("grant expired at {expired_at_ms} ms")]
    Expired { expired_at_ms: u64, now_ms: u64 },

    #[error("system clock rollback detected")]
    ClockRollbackDetected,

    #[error("grant has been explicitly revoked")]
    Revoked,

    #[error("profile or credentials were modified since proposal was prepared")]
    ProfileMutated,

    #[error("grant attempt limit exceeded: limit {max_attempts}, used {attempts_used}")]
    AttemptsExceeded {
        max_attempts: u32,
        attempts_used: u32,
    },

    #[error("grant scope mismatch on field '{field}'")]
    ScopeMismatch { field: &'static str },

    #[error("invalid grant issuance parameter: {0}")]
    InvalidParameter(String),

    #[error("system random source error")]
    RandomSourceError,
}

/// Helper to get current epoch milliseconds.
fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

/// Helper to verify 64-character lowercase hex string.
fn is_lower_hex_64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// Generate a 32-byte (256-bit) cryptographically random hex nonce.
fn generate_nonce() -> Result<String, GrantError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| GrantError::RandomSourceError)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Validate a [`GrantScope`] before issuance.
pub fn validate_grant_scope(scope: &GrantScope) -> Result<(), GrantError> {
    if !matches!(scope.capability.as_str(),
        FLUX_CAPABILITY | "text_mask_sam_ts@1" | "text_regions_rt@1" | cleaner_core::cloud_denoise_wire::CAPABILITY)
    {
        return Err(GrantError::InvalidParameter("unknown cloud capability".into()));
    }
    if scope.capability != FLUX_CAPABILITY
        && (scope.recipe.recipe_id != scope.capability || scope.recipe.model_id != scope.capability)
    {
        return Err(GrantError::InvalidParameter("analysis grant recipe does not match capability".into()));
    }
    validate_profile_id(&scope.profile_id)
        .map_err(|e| GrantError::InvalidParameter(format!("profile_id invalid: {e}")))?;

    if !is_lower_hex_64(&scope.canonical_endpoint_fingerprint) {
        return Err(GrantError::InvalidParameter(
            "canonical_endpoint_fingerprint must be a 64-character lowercase SHA-256 hex string"
                .to_string(),
        ));
    }

    if scope.source_hash.is_empty() {
        return Err(GrantError::InvalidParameter(
            "source_hash cannot be empty".to_string(),
        ));
    }

    if !is_lower_hex_64(&scope.crop_sha256) {
        return Err(GrantError::InvalidParameter(
            "crop_sha256 must be a 64-character lowercase SHA-256 hex string".to_string(),
        ));
    }

    if !is_lower_hex_64(&scope.hint_sha256) {
        return Err(GrantError::InvalidParameter(
            "hint_sha256 must be a 64-character lowercase SHA-256 hex string".to_string(),
        ));
    }

    if !is_lower_hex_64(&scope.operation_digest) {
        return Err(GrantError::InvalidParameter(
            "operation_digest must be a 64-character lowercase SHA-256 hex string".to_string(),
        ));
    }

    if scope.crop_bounds.w == 0 || scope.crop_bounds.h == 0 {
        return Err(GrantError::InvalidParameter(
            "crop_bounds dimensions must be positive non-zero".to_string(),
        ));
    }

    if scope.mask_hash.is_empty() {
        return Err(GrantError::InvalidParameter(
            "mask_hash cannot be empty".to_string(),
        ));
    }

    if scope.region_ids.is_empty() {
        return Err(GrantError::InvalidParameter(
            "grant scope region_ids cannot be empty".to_string(),
        ));
    }

    if scope.region_ids.len() > MAX_REGIONS_PER_GRANT {
        return Err(GrantError::InvalidParameter(format!(
            "grant scope region_ids count ({}) exceeds maximum limit of {}",
            scope.region_ids.len(),
            MAX_REGIONS_PER_GRANT
        )));
    }

    for id in &scope.region_ids {
        if id.is_empty() || id.len() > 128 {
            return Err(GrantError::InvalidParameter(
                "each region_id must be between 1 and 128 characters".to_string(),
            ));
        }
    }

    Ok(())
}

struct GrantState {
    grants: HashMap<String, Grant>,
    profile_epochs: HashMap<(CloudProvider, String), u64>,
}

/// Thread-safe in-memory authorization grant authority.
pub struct GrantService {
    state: Mutex<GrantState>,
}

impl GrantService {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(GrantState {
                grants: HashMap::new(),
                profile_epochs: HashMap::new(),
            }),
        }
    }

    /// Global shared grant authority.
    pub fn global() -> &'static GrantService {
        static INSTANCE: OnceLock<GrantService> = OnceLock::new();
        INSTANCE.get_or_init(GrantService::new)
    }

    /// Return the current mutation epoch for a profile.
    pub fn get_profile_epoch(&self, provider: CloudProvider, profile_id: &str) -> u64 {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state
            .profile_epochs
            .get(&(provider, profile_id.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// Atomically advance profile epoch and revoke all active grants for that profile.
    pub fn invalidate_profile(&self, provider: CloudProvider, profile_id: &str) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let entry = state
            .profile_epochs
            .entry((provider, profile_id.to_string()))
            .or_insert(0);
        *entry = entry.saturating_add(1);
        let new_epoch = *entry;
        for grant in state.grants.values_mut() {
            if grant.scope.provider == provider && grant.scope.profile_id == profile_id {
                grant.revoked = true;
            }
        }
        new_epoch
    }

    /// Atomically advance all profile epochs and revoke all outstanding grants.
    pub fn invalidate_all(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        for epoch in state.profile_epochs.values_mut() {
            *epoch = epoch.saturating_add(1);
        }
        for grant in state.grants.values_mut() {
            grant.revoked = true;
        }
    }

    /// Issue a new authorization grant for the specified scope against current profile epoch.
    pub fn issue_grant(
        &self,
        scope: GrantScope,
        ttl: Duration,
        max_attempts: u32,
    ) -> Result<Grant, GrantError> {
        let current_epoch = self.get_profile_epoch(scope.provider, &scope.profile_id);
        self.issue_grant_with_epoch(scope, current_epoch, ttl, max_attempts)
    }

    /// Issue a new authorization grant ensuring the profile epoch has not mutated since `expected_epoch`.
    pub fn issue_grant_with_epoch(
        &self,
        scope: GrantScope,
        expected_epoch: u64,
        ttl: Duration,
        max_attempts: u32,
    ) -> Result<Grant, GrantError> {
        validate_grant_scope(&scope)?;

        if max_attempts == 0 || max_attempts > MAX_GRANT_ATTEMPTS {
            return Err(GrantError::InvalidParameter(format!(
                "max_attempts must be between 1 and {MAX_GRANT_ATTEMPTS}"
            )));
        }

        if ttl < MIN_GRANT_TTL || ttl > MAX_GRANT_TTL {
            return Err(GrantError::InvalidParameter(format!(
                "ttl must be between {:?} and {:?}",
                MIN_GRANT_TTL, MAX_GRANT_TTL
            )));
        }

        let nonce = generate_nonce()?;
        let now_ms = now_epoch_ms();
        let expires_at_ms = now_ms.saturating_add(ttl.as_millis() as u64);

        let grant = Grant {
            nonce: nonce.clone(),
            scope: scope.clone(),
            issued_at_ms: now_ms,
            expires_at_ms,
            max_attempts,
            attempts_used: 0,
            revoked: false,
        };

        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        let current_epoch = state
            .profile_epochs
            .get(&(scope.provider, scope.profile_id.clone()))
            .copied()
            .unwrap_or(0);

        if current_epoch != expected_epoch {
            return Err(GrantError::ProfileMutated);
        }

        // Purge expired grants
        state.grants.retain(|_, g| g.expires_at_ms > now_ms);

        // Enforce hard capacity bound by evicting oldest grant
        if state.grants.len() >= MAX_CACHED_GRANTS {
            if let Some(oldest_key) = state
                .grants
                .iter()
                .min_by_key(|(_, v)| v.issued_at_ms)
                .map(|(k, _)| k.clone())
            {
                state.grants.remove(&oldest_key);
            }
        }

        state.grants.insert(nonce, grant.clone());

        Ok(grant)
    }

    /// Validate and atomically consume an attempt against a grant.
    pub fn validate_and_consume(
        &self,
        nonce: &str,
        requested_scope: &GrantScope,
    ) -> Result<(), GrantError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        let grant = state.grants.get_mut(nonce).ok_or(GrantError::NotFound)?;

        if grant.revoked {
            return Err(GrantError::Revoked);
        }

        let now_ms = now_epoch_ms();

        // Conservative clock rollback check
        if now_ms < grant.issued_at_ms {
            return Err(GrantError::ClockRollbackDetected);
        }

        // Expiry at now >= deadline
        if now_ms >= grant.expires_at_ms {
            return Err(GrantError::Expired {
                expired_at_ms: grant.expires_at_ms,
                now_ms,
            });
        }

        if grant.attempts_used >= grant.max_attempts {
            return Err(GrantError::AttemptsExceeded {
                max_attempts: grant.max_attempts,
                attempts_used: grant.attempts_used,
            });
        }

        // Exact immutable scope verification
        if grant.scope.capability != requested_scope.capability {
            return Err(GrantError::ScopeMismatch { field: "capability" });
        }
        if grant.scope.provider != requested_scope.provider {
            return Err(GrantError::ScopeMismatch { field: "provider" });
        }

        if grant.scope.profile_id != requested_scope.profile_id {
            return Err(GrantError::ScopeMismatch {
                field: "profile_id",
            });
        }

        if grant.scope.canonical_endpoint_fingerprint
            != requested_scope.canonical_endpoint_fingerprint
        {
            return Err(GrantError::ScopeMismatch {
                field: "canonical_endpoint_fingerprint",
            });
        }

        if grant.scope.source_hash != requested_scope.source_hash {
            return Err(GrantError::ScopeMismatch {
                field: "source_hash",
            });
        }

        if grant.scope.crop_sha256 != requested_scope.crop_sha256 {
            return Err(GrantError::ScopeMismatch {
                field: "crop_sha256",
            });
        }

        if grant.scope.hint_sha256 != requested_scope.hint_sha256 {
            return Err(GrantError::ScopeMismatch {
                field: "hint_sha256",
            });
        }

        if grant.scope.operation_digest != requested_scope.operation_digest {
            return Err(GrantError::ScopeMismatch {
                field: "operation_digest",
            });
        }

        if grant.scope.crop_bounds != requested_scope.crop_bounds {
            return Err(GrantError::ScopeMismatch {
                field: "crop_bounds",
            });
        }

        if grant.scope.mask_hash != requested_scope.mask_hash {
            return Err(GrantError::ScopeMismatch { field: "mask_hash" });
        }

        if grant.scope.revision != requested_scope.revision {
            return Err(GrantError::ScopeMismatch { field: "revision" });
        }

        if grant.scope.input_sha256 != requested_scope.input_sha256 {
            return Err(GrantError::ScopeMismatch { field: "input_sha256" });
        }

        if grant.scope.predecessors_sha256 != requested_scope.predecessors_sha256 {
            return Err(GrantError::ScopeMismatch { field: "predecessors_sha256" });
        }

        if grant.scope.recipe != requested_scope.recipe {
            return Err(GrantError::ScopeMismatch { field: "recipe" });
        }

        if grant.scope.region_ids != requested_scope.region_ids {
            return Err(GrantError::ScopeMismatch {
                field: "region_ids",
            });
        }

        // Increment attempt
        grant.attempts_used += 1;
        Ok(())
    }

    /// Revoke a single grant by nonce.
    pub fn revoke_grant(&self, nonce: &str) -> Result<(), GrantError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        let grant = state.grants.get_mut(nonce).ok_or(GrantError::NotFound)?;
        grant.revoked = true;
        Ok(())
    }

    /// Revoke all active grants for a specific provider and profile ID.
    pub fn revoke_profile_grants(&self, provider: CloudProvider, profile_id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        for grant in state.grants.values_mut() {
            if grant.scope.provider == provider && grant.scope.profile_id == profile_id {
                grant.revoked = true;
            }
        }
    }

    /// Revoke all outstanding grants across all providers and profiles.
    pub fn revoke_all(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        for grant in state.grants.values_mut() {
            grant.revoked = true;
        }
    }

    /// Purge expired grants from memory.
    pub fn purge_expired(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let now_ms = now_epoch_ms();
        state.grants.retain(|_, grant| grant.expires_at_ms > now_ms);
    }

    /// Return current number of cached grants in memory.
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .grants
            .len()
    }

    /// Check if grant cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Retrieve a copy of an active or historical grant by nonce if present.
    pub fn get_grant(&self, nonce: &str) -> Option<Grant> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.grants.get(nonce).cloned()
    }
}

impl Default for GrantService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dummy_scope() -> GrantScope {
        GrantScope {
            capability: FLUX_CAPABILITY.to_string(),
            provider: CloudProvider::Modal,
            profile_id: "modal-prof-1".to_string(),
            canonical_endpoint_fingerprint:
                "d9e8f7a6b5c4d3e2f1a0d9e8f7a6b5c4d3e2f1a0d9e8f7a6b5c4d3e2f1a01234".to_string(),
            source_hash: "a1b2c3d4e5f6".to_string(),
            crop_sha256: "112233445566778899001122334455667788990011223344556677889900aabb"
                .to_string(),
            hint_sha256: "2233445566778899001122334455667788990011223344556677889900112233"
                .to_string(),
            operation_digest: "3344556677889900112233445566778899001122334455667788990011223344"
                .to_string(),
            crop_bounds: Rect::new(10, 20, 100, 150),
            mask_hash: "m1m2m3m4".to_string(),
            revision: 42,
            input_sha256: String::new(),
            predecessors_sha256: String::new(),
            recipe: RenderRecipe::new("sdnq-v1", "1.0.0", "flux-schnell", "rev-2026-09", false),
            region_ids: vec!["reg-1".to_string(), "reg-2".to_string()],
        }
    }

    #[test]
    fn grant_issue_and_consume_success() {
        let service = GrantService::new();
        let scope = dummy_scope();

        let grant = service
            .issue_grant(scope.clone(), Duration::from_secs(60), 1)
            .expect("issue grant");
        assert_eq!(grant.nonce.len(), 64);
        assert_eq!(grant.attempts_used, 0);
        assert!(!grant.revoked);

        let res = service.validate_and_consume(&grant.nonce, &scope);
        assert_eq!(res, Ok(()));
    }

    #[test]
    fn changed_underlay_refuses_dispatch_without_spending_grant() {
        let service = GrantService::new();
        let mut scope = dummy_scope();
        scope.input_sha256 = "a".repeat(64);
        scope.predecessors_sha256 = "b".repeat(64);
        let grant = service.issue_grant(scope.clone(), MAX_GRANT_TTL, 1).unwrap();
        let mut changed = scope.clone();
        changed.input_sha256 = "c".repeat(64);
        assert_eq!(service.validate_and_consume(&grant.nonce, &changed),
            Err(GrantError::ScopeMismatch { field: "input_sha256" }));
        changed = scope.clone();
        changed.predecessors_sha256 = "d".repeat(64);
        assert_eq!(service.validate_and_consume(&grant.nonce, &changed),
            Err(GrantError::ScopeMismatch { field: "predecessors_sha256" }));
        assert_eq!(service.get_grant(&grant.nonce).unwrap().attempts_used, 0);
        let mut analysis = scope.clone();
        analysis.capability = "text_mask_sam_ts@1".into();
        analysis.recipe = RenderRecipe::new("text_mask_sam_ts@1", "1.0.0",
            "text_mask_sam_ts@1", "c".repeat(40), false);
        assert_eq!(service.validate_and_consume(&grant.nonce, &analysis),
            Err(GrantError::ScopeMismatch { field: "capability" }));
        assert_eq!(service.validate_and_consume(&grant.nonce, &scope), Ok(()));
        let analysis_grant = service.issue_grant(analysis, MAX_GRANT_TTL, 1).unwrap();
        assert_eq!(service.validate_and_consume(&analysis_grant.nonce, &scope),
            Err(GrantError::ScopeMismatch { field: "capability" }));
    }

    #[test]
    fn legacy_grants_default_to_flux_capability() {
        let mut value = serde_json::to_value(dummy_scope()).unwrap();
        value.as_object_mut().unwrap().remove("capability");
        let restored: GrantScope = serde_json::from_value(value).unwrap();
        assert_eq!(restored.capability, FLUX_CAPABILITY);
    }

    #[test]
    fn grant_scope_validation_on_issuance() {
        let service = GrantService::new();
        let mut scope = dummy_scope();

        // Invalid profile ID
        scope.profile_id = "bad/id".to_string();
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(60), 1)
            .is_err());

        scope = dummy_scope();
        scope.canonical_endpoint_fingerprint = "short".to_string();
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(60), 1)
            .is_err());

        scope = dummy_scope();
        scope.crop_sha256 =
            "UPPERCASE112233445566778899001122334455667788990011223344556677889900".to_string();
        assert!(
            service
                .issue_grant(scope.clone(), Duration::from_secs(60), 1)
                .is_err(),
            "Uppercase hex must fail"
        );

        scope = dummy_scope();
        scope.hint_sha256 = "short".to_string();
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(60), 1)
            .is_err());

        scope = dummy_scope();
        scope.operation_digest = "short".to_string();
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(60), 1)
            .is_err());

        scope = dummy_scope();
        scope.crop_bounds = Rect::new(0, 0, 0, 10);
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(60), 1)
            .is_err());

        scope = dummy_scope();
        scope.region_ids = vec![];
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(60), 1)
            .is_err());

        // Invalid TTL & attempts
        scope = dummy_scope();
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(0), 1)
            .is_err());
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(301), 1)
            .is_err());
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(60), 0)
            .is_err());
        assert!(service
            .issue_grant(scope.clone(), Duration::from_secs(60), 11)
            .is_err());
    }

    #[test]
    fn grant_scope_mismatches_rejected() {
        let service = GrantService::new();
        let scope = dummy_scope();

        let grant = service
            .issue_grant(scope.clone(), Duration::from_secs(60), 5)
            .expect("issue grant");

        // Provider mismatch
        let mut wrong = scope.clone();
        wrong.provider = CloudProvider::Beam;
        assert_eq!(
            service.validate_and_consume(&grant.nonce, &wrong),
            Err(GrantError::ScopeMismatch { field: "provider" })
        );

        // Canonical endpoint fingerprint mismatch (e.g. path change)
        let mut wrong = scope.clone();
        wrong.canonical_endpoint_fingerprint =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert_eq!(
            service.validate_and_consume(&grant.nonce, &wrong),
            Err(GrantError::ScopeMismatch {
                field: "canonical_endpoint_fingerprint"
            })
        );

        // Crop SHA256 mismatch
        let mut wrong = scope.clone();
        wrong.crop_sha256 =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert_eq!(
            service.validate_and_consume(&grant.nonce, &wrong),
            Err(GrantError::ScopeMismatch {
                field: "crop_sha256"
            })
        );

        // Hint SHA256 mismatch
        let mut wrong = scope.clone();
        wrong.hint_sha256 =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert_eq!(
            service.validate_and_consume(&grant.nonce, &wrong),
            Err(GrantError::ScopeMismatch {
                field: "hint_sha256"
            })
        );

        // Operation digest mismatch
        let mut wrong = scope.clone();
        wrong.operation_digest =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert_eq!(
            service.validate_and_consume(&grant.nonce, &wrong),
            Err(GrantError::ScopeMismatch {
                field: "operation_digest"
            })
        );

        // Region IDs mismatch
        let mut wrong = scope.clone();
        wrong.region_ids = vec!["reg-1".to_string(), "reg-3".to_string()];
        assert_eq!(
            service.validate_and_consume(&grant.nonce, &wrong),
            Err(GrantError::ScopeMismatch {
                field: "region_ids"
            })
        );
    }

    #[test]
    fn grant_expiry_and_replay_protection() {
        let service = GrantService::new();
        let scope = dummy_scope();

        let grant = service
            .issue_grant(scope.clone(), Duration::from_secs(1), 1)
            .expect("issue grant");

        // Consume once
        assert_eq!(service.validate_and_consume(&grant.nonce, &scope), Ok(()));

        // Consume second time (replay) -> AttemptsExceeded
        assert_eq!(
            service.validate_and_consume(&grant.nonce, &scope),
            Err(GrantError::AttemptsExceeded {
                max_attempts: 1,
                attempts_used: 1
            })
        );
    }

    #[test]
    fn profile_level_grant_revocation() {
        let service = GrantService::new();
        let scope = dummy_scope();

        let grant1 = service
            .issue_grant(scope.clone(), Duration::from_secs(60), 2)
            .expect("issue grant 1");
        let grant2 = service
            .issue_grant(scope.clone(), Duration::from_secs(60), 2)
            .expect("issue grant 2");

        // Revoke all grants for Modal / modal-prof-1
        service.revoke_profile_grants(CloudProvider::Modal, "modal-prof-1");

        assert_eq!(
            service.validate_and_consume(&grant1.nonce, &scope),
            Err(GrantError::Revoked)
        );
        assert_eq!(
            service.validate_and_consume(&grant2.nonce, &scope),
            Err(GrantError::Revoked)
        );
    }

    #[test]
    fn grant_concurrent_consumption() {
        use std::sync::Arc;

        let service = Arc::new(GrantService::new());
        let scope = dummy_scope();

        let grant = service
            .issue_grant(scope.clone(), Duration::from_secs(60), 1)
            .expect("issue grant");

        let mut handles = Vec::new();
        for _ in 0..10 {
            let svc = Arc::clone(&service);
            let nonce = grant.nonce.clone();
            let sc = scope.clone();
            handles.push(std::thread::spawn(move || {
                svc.validate_and_consume(&nonce, &sc)
            }));
        }

        let mut successes = 0;
        let mut failures = 0;
        for h in handles {
            match h.join().unwrap() {
                Ok(()) => successes += 1,
                Err(GrantError::AttemptsExceeded { .. }) => failures += 1,
                Err(other) => panic!("Unexpected error: {other:?}"),
            }
        }

        assert_eq!(successes, 1, "Exactly one thread must succeed");
        assert_eq!(failures, 9, "Remaining 9 threads must fail attempt check");
    }

    #[test]
    fn grant_cache_bounds_and_oldest_eviction() {
        let service = GrantService::new();
        let scope = dummy_scope();

        let mut first_nonce = None;
        for i in 0..MAX_CACHED_GRANTS {
            let grant = service
                .issue_grant(scope.clone(), Duration::from_secs(300), 1)
                .expect("issue grant");
            if i == 0 {
                first_nonce = Some(grant.nonce);
            }
        }

        assert_eq!(service.len(), MAX_CACHED_GRANTS);

        let first_nonce = first_nonce.unwrap();
        // The first grant is currently in cache
        assert_eq!(service.validate_and_consume(&first_nonce, &scope), Ok(()));

        // Issue one more grant beyond MAX_CACHED_GRANTS
        let new_grant = service
            .issue_grant(scope.clone(), Duration::from_secs(300), 1)
            .expect("issue grant beyond capacity");

        // Cache size remains bounded to MAX_CACHED_GRANTS
        assert_eq!(service.len(), MAX_CACHED_GRANTS);

        // New grant is valid
        assert_eq!(
            service.validate_and_consume(&new_grant.nonce, &scope),
            Ok(())
        );
    }

    #[test]
    fn grant_revoke_all_revokes_all_active_grants() {
        let service = GrantService::new();
        let scope1 = dummy_scope();
        let mut scope2 = dummy_scope();
        scope2.provider = CloudProvider::Beam;
        scope2.profile_id = "beam-prof-1".to_string();

        let grant1 = service
            .issue_grant(scope1.clone(), Duration::from_secs(60), 1)
            .unwrap();
        let grant2 = service
            .issue_grant(scope2.clone(), Duration::from_secs(60), 1)
            .unwrap();

        service.revoke_all();

        assert_eq!(
            service.validate_and_consume(&grant1.nonce, &scope1),
            Err(GrantError::Revoked)
        );
        assert_eq!(
            service.validate_and_consume(&grant2.nonce, &scope2),
            Err(GrantError::Revoked)
        );
    }

    #[test]
    fn grant_issue_with_stale_epoch_rejected() {
        let service = GrantService::new();
        let scope = dummy_scope();

        // Initially epoch is 0
        assert_eq!(
            service.get_profile_epoch(CloudProvider::Modal, "modal-prof-1"),
            0
        );

        // Advance epoch to 1
        let new_epoch = service.invalidate_profile(CloudProvider::Modal, "modal-prof-1");
        assert_eq!(new_epoch, 1);

        // Issuing with stale epoch 0 must fail closed with ProfileMutated
        let err = service
            .issue_grant_with_epoch(scope.clone(), 0, Duration::from_secs(60), 1)
            .unwrap_err();
        assert_eq!(err, GrantError::ProfileMutated);
        assert_eq!(service.len(), 0);

        // Issuing with current epoch 1 succeeds
        let grant = service
            .issue_grant_with_epoch(scope.clone(), 1, Duration::from_secs(60), 1)
            .expect("issue with current epoch");
        assert_eq!(service.len(), 1);
        assert_eq!(service.validate_and_consume(&grant.nonce, &scope), Ok(()));
    }

    #[test]
    fn grant_deterministic_mutation_epoch_barrier() {
        use std::sync::{Arc, Barrier};

        let service = Arc::new(GrantService::new());
        let scope = dummy_scope();

        let barrier = Arc::new(Barrier::new(2));

        let svc_t1 = Arc::clone(&service);
        let sc_t1 = scope.clone();
        let b_t1 = Arc::clone(&barrier);

        let handle1 = std::thread::spawn(move || {
            let captured_epoch = svc_t1.get_profile_epoch(sc_t1.provider, &sc_t1.profile_id);
            // Synchronize at barrier: both threads ready
            b_t1.wait();
            // Wait for thread 2 to complete invalidation
            b_t1.wait();
            // Attempt to issue grant with captured_epoch
            svc_t1.issue_grant_with_epoch(sc_t1, captured_epoch, Duration::from_secs(60), 1)
        });

        let svc_t2 = Arc::clone(&service);
        let sc_t2 = scope.clone();
        let b_t2 = Arc::clone(&barrier);

        let handle2 = std::thread::spawn(move || {
            // Wait for thread 1 to capture epoch
            b_t2.wait();
            // Invalidate profile while thread 1 is between check and issue
            svc_t2.invalidate_profile(sc_t2.provider, &sc_t2.profile_id);
            // Signal thread 1 that invalidation is done
            b_t2.wait();
        });

        handle2.join().unwrap();
        let res1 = handle1.join().unwrap();

        assert_eq!(res1, Err(GrantError::ProfileMutated));
        assert_eq!(service.len(), 0, "No grant should have been inserted");
    }

    #[test]
    fn grant_service_poison_recovery() {
        let service = Arc::new(GrantService::new());
        let scope = dummy_scope();

        // Issue one grant before poison
        let grant1 = service
            .issue_grant(scope.clone(), Duration::from_secs(60), 2)
            .expect("initial grant");

        // Force a thread to panic while holding the state lock
        let svc_clone = Arc::clone(&service);
        let _ = std::thread::spawn(move || {
            let _guard = svc_clone.state.lock().unwrap();
            panic!("intentional poison panic");
        })
        .join();

        // Mutex is now poisoned. All operations must safely recover and never fail-open or no-op.
        assert_eq!(
            service.get_profile_epoch(CloudProvider::Modal, "modal-prof-1"),
            0
        );

        // Invalidation must advance epoch and revoke existing grants
        let new_epoch = service.invalidate_profile(CloudProvider::Modal, "modal-prof-1");
        assert_eq!(new_epoch, 1);
        assert_eq!(
            service.validate_and_consume(&grant1.nonce, &scope),
            Err(GrantError::Revoked)
        );

        // Issue new grant with current epoch succeeds
        let grant2 = service
            .issue_grant_with_epoch(scope.clone(), 1, Duration::from_secs(60), 1)
            .expect("issue after poison");
        assert_eq!(service.validate_and_consume(&grant2.nonce, &scope), Ok(()));
    }
}
