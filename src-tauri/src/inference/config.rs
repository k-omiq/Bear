//! Public inference configuration, profile validation, and atomic persistence.
//!
//! Manages versioned public profiles for both Beam and Modal cloud providers alongside
//! the local default execution target. Persists public settings under the dedicated
//! `inference.json` configuration file.
//!
//! ## Invariants
//!
//! 1. **Local Default Target:** The initial and fallback execution target is [`ExecutionTarget::Local`].
//! 2. **Simultaneous Provider Profiles:** Both Beam and Modal profiles can be configured and stored
//!    simultaneously in `inference.json`.
//! 3. **Bounded Safe Profile IDs:** Profile IDs must be non-empty, bounded (1..=64 chars), and
//!    consist solely of ASCII alphanumeric characters, hyphens (`-`), and underscores (`_`).
//! 4. **Strict HTTPS Endpoint Hygiene (`reqwest::Url`):** Endpoints must use `https://`, have
//!    non-empty hostnames, contain NO user credentials (`@`), NO query parameters (`?`), NO URL
//!    fragments (`#`), NO control characters, and NO surrounding whitespace.
//! 5. **Conservative IP Literal & Local Host Rejection:** Rejects all IPv4 and IPv6 literals
//!    (including WHATWG octal/hex/decimal and IPv4-mapped IPv6 normalization), `localhost`,
//!    and `.local`/`.localhost`/`.internal`/`.arpa` domains following trailing-dot normalization.
//!    (DNS resolution and connect-time network verification remain P3b scope).
//! 6. **Canonical Origin Fingerprinting:** Every profile computes a lowercase canonical origin
//!    (`https://<host>` or `https://<host>:<port>`) and a deterministic SHA-256 hex fingerprint used for
//!    immutable secret binding.
//! 7. **Canonical Endpoint Fingerprinting:** Computes full URL path fingerprints for grant scope binding,
//!    so changing endpoint paths within the same origin immediately invalidates issued grants.
//! 8. **Fail-Closed Schema Versioning & Bounds:** Unrecognized, missing (0), or future schema versions
//!    fail closed. Profile counts per provider are bounded ([`MAX_PROFILES_PER_PROVIDER`]). All structures
//!    strictly enforce `deny_unknown_fields`.
//! 9. **Zero Plaintext Secrets:** Public profiles NEVER contain API keys, bearer tokens, or secrets.
//!    Secret material is managed exclusively by [`crate::inference::secrets`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cleaner_core::engines::render::ExecutionTarget;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Host;

/// The dedicated configuration file name under the application's config directory.
pub const INFERENCE_CONFIG_FILE: &str = "inference.json";

/// The current supported public profile schema version.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Maximum allowable length for profile identifiers.
pub const MAX_PROFILE_ID_LEN: usize = 64;

/// Maximum allowable length for profile human-readable display names.
pub const MAX_PROFILE_NAME_LEN: usize = 128;

/// Maximum allowable length for endpoint URL strings.
pub const MAX_ENDPOINT_URL_LEN: usize = 2048;

/// Maximum allowable number of profiles per cloud provider.
pub const MAX_PROFILES_PER_PROVIDER: usize = 32;

/// A validated public configuration profile for a cloud deployment (Beam or Modal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudProfile {
    pub id: String,
    pub name: String,
    pub endpoint_url: String,
    pub canonical_origin: String,
    pub canonical_origin_fingerprint: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

pub type BeamProfile = CloudProfile;
pub type ModalProfile = CloudProfile;

/// The root public inference configuration structure persisted to `inference.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InferenceConfig {
    pub schema_version: u32,
    pub selected_target: ExecutionTarget,
    #[serde(default)]
    pub beam_profiles: HashMap<String, CloudProfile>,
    #[serde(default)]
    pub modal_profiles: HashMap<String, CloudProfile>,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            selected_target: ExecutionTarget::Local,
            beam_profiles: HashMap::new(),
            modal_profiles: HashMap::new(),
        }
    }
}

/// Validate that a profile identifier is bounded, non-empty, and free of dangerous characters.
pub fn validate_profile_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("profile id cannot be empty".to_string());
    }
    if id != id.trim() {
        return Err("profile id must not contain leading or trailing whitespace".to_string());
    }
    if id.len() > MAX_PROFILE_ID_LEN {
        return Err(format!(
            "profile id '{}' exceeds maximum length of {} characters",
            id, MAX_PROFILE_ID_LEN
        ));
    }
    if id.starts_with('-') || id.starts_with('_') {
        return Err(format!(
            "profile id '{id}' must start with an alphanumeric character"
        ));
    }
    for c in id.chars() {
        if !c.is_ascii_alphanumeric() && c != '-' && c != '_' {
            return Err(format!(
                "profile id '{id}' contains invalid character '{c}'; only ASCII alphanumeric, '-', and '_' are allowed"
            ));
        }
    }
    Ok(())
}

/// Validate that a human-readable profile name is bounded.
pub fn validate_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("profile name cannot be empty".to_string());
    }
    if name != name.trim() {
        return Err("profile name must not contain leading or trailing whitespace".to_string());
    }
    if name.len() > MAX_PROFILE_NAME_LEN {
        return Err(format!(
            "profile name exceeds maximum length of {} characters",
            MAX_PROFILE_NAME_LEN
        ));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("profile name cannot contain control characters".to_string());
    }
    Ok(())
}

/// Validate an HTTPS endpoint URL using `reqwest::Url` and return `(canonical_origin, canonical_origin_fingerprint)`.
///
/// Enforces HTTPS scheme, rejects embedded credentials, query parameters, URL fragments,
/// control characters, surrounding whitespace, and conservative IP literals / loopback domains.
pub fn validate_https_endpoint(raw_url: &str) -> Result<(String, String), String> {
    if raw_url.is_empty() {
        return Err("endpoint URL cannot be empty".to_string());
    }

    if raw_url != raw_url.trim() {
        return Err("endpoint URL must not contain leading or trailing whitespace".to_string());
    }

    if raw_url.len() > MAX_ENDPOINT_URL_LEN {
        return Err(format!(
            "endpoint URL exceeds maximum length of {} characters",
            MAX_ENDPOINT_URL_LEN
        ));
    }

    if raw_url.chars().any(|c| c.is_control()) {
        return Err("endpoint URL must not contain control characters".to_string());
    }

    let parsed = Url::parse(raw_url).map_err(|e| format!("invalid endpoint URL: {e}"))?;

    if parsed.scheme() != "https" {
        return Err(format!(
            "endpoint URL must use 'https://' scheme, got '{}'",
            parsed.scheme()
        ));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("endpoint URL must not contain user credentials (user:pass@host is forbidden)".to_string());
    }

    if parsed.query().is_some() {
        return Err("endpoint URL must not contain query parameters ('?' is forbidden)".to_string());
    }

    if parsed.fragment().is_some() {
        return Err("endpoint URL must not contain URL fragments ('#' is forbidden)".to_string());
    }

    let path = parsed.path();
    let trimmed_path = path.trim_end_matches('/');
    if trimmed_path != "/mc/v1" {
        return Err(format!(
            "endpoint URL must specify exact base path '/mc/v1', got '{path}'"
        ));
    }

    let host = parsed
        .host()
        .ok_or_else(|| "endpoint URL has missing host".to_string())?;

    // Conservatively reject IP literals (IPv4, IPv6, WHATWG octal/decimal/hex/mapped)
    match host {
        Host::Ipv4(ip) => {
            Err(format!(
                "endpoint host '{ip}' is a forbidden IPv4 literal; domain names are required"
            ))
        }
        Host::Ipv6(ip) => {
            Err(format!(
                "endpoint host '[{ip}]' is a forbidden IPv6 literal; domain names are required"
            ))
        }
        Host::Domain(d) => {
            let host_str = d.to_ascii_lowercase();
            // Trailing-dot normalization
            let normalized = host_str.trim_end_matches('.');
            if normalized.is_empty() {
                return Err("endpoint host cannot be empty".to_string());
            }

            if normalized == "localhost"
                || normalized.ends_with(".localhost")
                || normalized.ends_with(".local")
                || normalized.ends_with(".internal")
                || normalized.ends_with(".arpa")
            {
                return Err(format!(
                    "endpoint host '{d}' is a forbidden local or internal domain literal"
                ));
            }

            // Verify characters
            for c in normalized.chars() {
                if !c.is_ascii_alphanumeric() && c != '-' && c != '.' && c != '_' {
                    return Err(format!(
                        "endpoint host '{d}' contains invalid character '{c}'"
                    ));
                }
            }

            let canonical_origin = match parsed.port() {
                Some(p) if p != 443 => format!("https://{normalized}:{p}"),
                _ => format!("https://{normalized}"),
            };

            let mut hasher = Sha256::new();
            hasher.update(canonical_origin.as_bytes());
            let fingerprint = format!("{:x}", hasher.finalize());

            Ok((canonical_origin, fingerprint))
        }
    }
}

/// Compute the canonical full endpoint URL fingerprint (64 lowercase hex characters).
///
/// Canonicalizes to `https://<host>[:<port>]/mc/v1` so trailing slash variants (`/mc/v1` and `/mc/v1/`)
/// produce identical fingerprints.
pub fn compute_canonical_endpoint_fingerprint(raw_url: &str) -> Result<String, String> {
    validate_https_endpoint(raw_url)?;
    let parsed = Url::parse(raw_url).map_err(|e| format!("invalid endpoint URL: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "endpoint URL has missing host".to_string())?;
    let host_str = host.to_ascii_lowercase();
    let normalized_host = host_str.trim_end_matches('.');

    // Normalize path to canonical /mc/v1 so slash variants are equal
    let canonical = match parsed.port() {
        Some(p) if p != 443 => format!("https://{normalized_host}:{p}/mc/v1"),
        _ => format!("https://{normalized_host}/mc/v1"),
    };

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

/// Validate an entire [`InferenceConfig`] instance.
pub fn validate_inference_config(config: &InferenceConfig) -> Result<(), String> {
    if config.schema_version == 0 || config.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "unsupported public inference config schema version {}; this build supports version {}",
            config.schema_version, CURRENT_SCHEMA_VERSION
        ));
    }

    if config.beam_profiles.len() > MAX_PROFILES_PER_PROVIDER {
        return Err(format!(
            "number of Beam profiles ({}) exceeds maximum limit of {}",
            config.beam_profiles.len(),
            MAX_PROFILES_PER_PROVIDER
        ));
    }

    if config.modal_profiles.len() > MAX_PROFILES_PER_PROVIDER {
        return Err(format!(
            "number of Modal profiles ({}) exceeds maximum limit of {}",
            config.modal_profiles.len(),
            MAX_PROFILES_PER_PROVIDER
        ));
    }

    // Validate all Beam profiles
    for (key, profile) in &config.beam_profiles {
        if key != &profile.id {
            return Err(format!(
                "beam profile key '{key}' does not match profile id '{}'",
                profile.id
            ));
        }
        validate_profile_id(&profile.id)?;
        validate_profile_name(&profile.name)?;
        let (origin, fp) = validate_https_endpoint(&profile.endpoint_url)?;
        if profile.canonical_origin != origin {
            return Err(format!(
                "beam profile '{}' canonical origin mismatch: expected '{origin}', got '{}'",
                profile.id, profile.canonical_origin
            ));
        }
        if profile.canonical_origin_fingerprint != fp {
            return Err(format!(
                "beam profile '{}' canonical origin fingerprint mismatch",
                profile.id
            ));
        }
    }

    // Validate all Modal profiles
    for (key, profile) in &config.modal_profiles {
        if key != &profile.id {
            return Err(format!(
                "modal profile key '{key}' does not match profile id '{}'",
                profile.id
            ));
        }
        validate_profile_id(&profile.id)?;
        validate_profile_name(&profile.name)?;
        let (origin, fp) = validate_https_endpoint(&profile.endpoint_url)?;
        if profile.canonical_origin != origin {
            return Err(format!(
                "modal profile '{}' canonical origin mismatch: expected '{origin}', got '{}'",
                profile.id, profile.canonical_origin
            ));
        }
        if profile.canonical_origin_fingerprint != fp {
            return Err(format!(
                "modal profile '{}' canonical origin fingerprint mismatch",
                profile.id
            ));
        }
    }

    // Validate selected target
    match &config.selected_target {
        ExecutionTarget::Local => {}
        ExecutionTarget::Beam { profile_id } => {
            if !config.beam_profiles.contains_key(profile_id) {
                return Err(format!(
                    "selected execution target references non-existent Beam profile '{profile_id}'"
                ));
            }
        }
        ExecutionTarget::Modal { profile_id } => {
            if !config.modal_profiles.contains_key(profile_id) {
                return Err(format!(
                    "selected execution target references non-existent Modal profile '{profile_id}'"
                ));
            }
        }
    }

    Ok(())
}

/// Return the path to `inference.json` under the application's config directory.
pub fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(INFERENCE_CONFIG_FILE))
        .map_err(|e| e.to_string())
}

/// Read and validate public inference configuration from `inference.json`.
pub fn read_inference_config(app: &tauri::AppHandle) -> Result<InferenceConfig, String> {
    let path = config_path(app)?;
    read_inference_config_from_path(&path)
}

/// Read from explicit path.
pub fn read_inference_config_from_path(path: &Path) -> Result<InferenceConfig, String> {
    match std::fs::read(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(InferenceConfig::default()),
        Err(err) => Err(format!("failed to read inference configuration: {err}")),
        Ok(bytes) => {
            let config: InferenceConfig = serde_json::from_slice(&bytes)
                .map_err(|e| format!("malformed inference configuration: {e}"))?;
            validate_inference_config(&config)?;
            Ok(config)
        }
    }
}

/// Validate and atomically write public inference configuration to `inference.json`.
pub fn write_inference_config(
    app: &tauri::AppHandle,
    config: InferenceConfig,
) -> Result<InferenceConfig, String> {
    let path = config_path(app)?;
    write_inference_config_to_path(&path, config)
}

/// Write to explicit path atomically.
pub fn write_inference_config_to_path(
    path: &Path,
    mut config: InferenceConfig,
) -> Result<InferenceConfig, String> {
    // Automatically fill / refresh canonical origins and fingerprints
    for profile in config.beam_profiles.values_mut() {
        let (origin, fp) = validate_https_endpoint(&profile.endpoint_url)?;
        profile.canonical_origin = origin;
        profile.canonical_origin_fingerprint = fp;
    }
    for profile in config.modal_profiles.values_mut() {
        let (origin, fp) = validate_https_endpoint(&profile.endpoint_url)?;
        profile.canonical_origin = origin;
        profile.canonical_origin_fingerprint = fp;
    }

    validate_inference_config(&config)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create configuration directory: {e}"))?;
    }

    let bytes = serde_json::to_vec_pretty(&config)
        .map_err(|e| format!("serialization error: {e}"))?;

    cleaner_core::project::buffers::write_atomic(path, &bytes)
        .map_err(|e| format!("atomic write error: {e}"))?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid_and_local() {
        let default_cfg = InferenceConfig::default();
        assert_eq!(default_cfg.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(default_cfg.selected_target, ExecutionTarget::Local);
        assert!(default_cfg.beam_profiles.is_empty());
        assert!(default_cfg.modal_profiles.is_empty());
        assert!(validate_inference_config(&default_cfg).is_ok());
    }

    #[test]
    fn profile_id_and_name_validation() {
        assert!(validate_profile_id("modal-prod-1").is_ok());
        assert!(validate_profile_id("beam_v2").is_ok());
        assert!(validate_profile_id("profile123").is_ok());

        assert!(validate_profile_id("").is_err());
        assert!(validate_profile_id(" modal-prod-1 ").is_err());
        assert!(validate_profile_id("-invalid-start").is_err());
        assert!(validate_profile_id("_invalid-start").is_err());
        assert!(validate_profile_id("has.dot").is_err());
        assert!(validate_profile_id("has/slash").is_err());
        assert!(validate_profile_id(&"a".repeat(MAX_PROFILE_ID_LEN + 1)).is_err());

        assert!(validate_profile_name("My GPU Worker").is_ok());
        assert!(validate_profile_name("").is_err());
        assert!(validate_profile_name(" name with space ").is_err());
        assert!(validate_profile_name("name\nwith\nnewline").is_err());
        assert!(validate_profile_name(&"a".repeat(MAX_PROFILE_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn https_endpoint_validation_rules() {
        // Valid endpoints
        let (origin1, fp1) = validate_https_endpoint("https://modal-cleaner.run.modal.com/mc/v1")
            .expect("valid modal endpoint");
        assert_eq!(origin1, "https://modal-cleaner.run.modal.com");
        assert_eq!(fp1.len(), 64);

        let (origin2, fp2) =
            validate_https_endpoint("https://api.beam.cloud:8443/mc/v1").expect("custom port");
        assert_eq!(origin2, "https://api.beam.cloud:8443");
        assert_eq!(fp2.len(), 64);

        // Standard port 443 normalized
        let (origin3, _) =
            validate_https_endpoint("https://api.beam.cloud:443/mc/v1").expect("port 443 normalized");
        assert_eq!(origin3, "https://api.beam.cloud");

        // Trailing dot normalized
        let (origin4, _) =
            validate_https_endpoint("https://api.beam.cloud./mc/v1").expect("trailing dot normalized");
        assert_eq!(origin4, "https://api.beam.cloud");

        // Trailing slash on /mc/v1 is accepted
        let (origin5, _) =
            validate_https_endpoint("https://api.beam.cloud/mc/v1/").expect("trailing slash accepted");
        assert_eq!(origin5, "https://api.beam.cloud");

        // Rejections: Bare and wrong paths fail closed
        assert!(validate_https_endpoint("https://api.beam.cloud").is_err());
        assert!(validate_https_endpoint("https://api.beam.cloud/").is_err());
        assert!(validate_https_endpoint("https://api.beam.cloud/endpoint").is_err());
        assert!(validate_https_endpoint("https://api.beam.cloud/mc/v2").is_err());

        // Rejections: Whitespace & control
        assert!(validate_https_endpoint("  https://api.beam.cloud/mc/v1").is_err());
        assert!(validate_https_endpoint("https://api.beam.cloud/mc/v1  ").is_err());
        assert!(validate_https_endpoint("https://api.beam.cloud/mc/v1\0").is_err());

        // Rejections: Scheme, credentials, query, fragment
        assert!(validate_https_endpoint("http://insecure.example.com/mc/v1").is_err());
        assert!(validate_https_endpoint("https://user:pass@example.com/mc/v1").is_err());
        assert!(validate_https_endpoint("https://api.example.com/mc/v1?query=1").is_err());
        assert!(validate_https_endpoint("https://api.example.com/mc/v1#fragment").is_err());

        // Rejections: IP literals & WHATWG normalization
        assert!(validate_https_endpoint("https://localhost:8080/mc/v1").is_err());
        assert!(validate_https_endpoint("https://localhost./mc/v1").is_err());
        assert!(validate_https_endpoint("https://foo.localhost/mc/v1").is_err());
        assert!(validate_https_endpoint("https://mybox.local/mc/v1").is_err());
        assert!(validate_https_endpoint("https://internal.internal/mc/v1").is_err());
        assert!(validate_https_endpoint("https://127.0.0.1:443/mc/v1").is_err());
        assert!(validate_https_endpoint("https://0.0.0.0/mc/v1").is_err());
        assert!(validate_https_endpoint("https://[::1]:8443/mc/v1").is_err());
        // WHATWG decimal notation: 2130706433 is 127.0.0.1
        assert!(validate_https_endpoint("https://2130706433/mc/v1").is_err());
        // WHATWG octal notation: 0177.0.0.1 is 127.0.0.1
        assert!(validate_https_endpoint("https://0177.0.0.1/mc/v1").is_err());
        // WHATWG IPv4-mapped IPv6 literal
        assert!(validate_https_endpoint("https://[::ffff:127.0.0.1]/mc/v1").is_err());
        // Link-local
        assert!(validate_https_endpoint("https://169.254.169.254/mc/v1").is_err());
    }

    #[test]
    fn endpoint_fingerprint_slash_variants_equal_and_wrong_paths_fail_closed() {
        // Slash variants equal
        let fp1 = compute_canonical_endpoint_fingerprint("https://api.modal.com/mc/v1").unwrap();
        let fp2 = compute_canonical_endpoint_fingerprint("https://api.modal.com/mc/v1/").unwrap();
        assert_eq!(fp1, fp2, "Trailing slash variants must produce identical endpoint fingerprints");

        // Bare path and wrong paths fail closed
        assert!(compute_canonical_endpoint_fingerprint("https://api.modal.com").is_err());
        assert!(compute_canonical_endpoint_fingerprint("https://api.modal.com/").is_err());
        assert!(compute_canonical_endpoint_fingerprint("https://api.modal.com/endpoint").is_err());
        assert!(compute_canonical_endpoint_fingerprint("https://api.modal.com/mc/v2").is_err());

        // Different host produces different fingerprint
        let fp_other = compute_canonical_endpoint_fingerprint("https://other.modal.com/mc/v1").unwrap();
        assert_ne!(fp1, fp_other);
    }

    #[test]
    fn deny_unknown_fields_and_schema_version_rejection() {
        let json_with_unknown = r#"{
            "schemaVersion": 1,
            "selectedTarget": { "type": "local" },
            "beamProfiles": {},
            "modalProfiles": {},
            "extraSneakyField": "forbidden"
        }"#;
        let de_res: Result<InferenceConfig, _> = serde_json::from_str(json_with_unknown);
        assert!(de_res.is_err(), "unknown field must be rejected");

        let cfg_future = InferenceConfig {
            schema_version: 2,
            ..Default::default()
        };
        assert!(validate_inference_config(&cfg_future).is_err());

        let cfg_zero = InferenceConfig {
            schema_version: 0,
            ..Default::default()
        };
        assert!(validate_inference_config(&cfg_zero).is_err());
    }

    #[test]
    fn profile_counts_bounded() {
        let mut cfg = InferenceConfig::default();
        let (origin, fp) = validate_https_endpoint("https://api.beam.cloud/mc/v1").unwrap();
        for i in 0..=MAX_PROFILES_PER_PROVIDER {
            let id = format!("beam-prof-{i}");
            cfg.beam_profiles.insert(
                id.clone(),
                CloudProfile {
                    id,
                    name: "Worker".to_string(),
                    endpoint_url: "https://api.beam.cloud/mc/v1".to_string(),
                    canonical_origin: origin.clone(),
                    canonical_origin_fingerprint: fp.clone(),
                    created_at_ms: 100,
                    updated_at_ms: 100,
                },
            );
        }
        assert!(
            validate_inference_config(&cfg).is_err(),
            "Exceeding profile count limit must fail"
        );
    }

    #[test]
    fn write_inference_config_overwrites_malformed_file() {
        let temp_dir = std::env::temp_dir().join("manga_cleaner_config_repair_test");
        let _ = std::fs::create_dir_all(&temp_dir);
        let path = temp_dir.join("corrupted_inference.json");

        // Write malformed JSON
        std::fs::write(&path, b"{ broken json syntax: [").expect("write malformed file");

        // Reading must fail without displaying local filesystem path
        let err = read_inference_config_from_path(&path).unwrap_err();
        assert!(!err.contains(&path.display().to_string()), "Error must not contain local path");
        assert!(err.contains("malformed inference configuration"));

        // Writing valid config must overwrite and repair the file
        let cfg = InferenceConfig {
            selected_target: ExecutionTarget::Local,
            ..Default::default()
        };
        let written = write_inference_config_to_path(&path, cfg).expect("write valid config");

        // Reading back must succeed and match
        let read_back = read_inference_config_from_path(&path).expect("read repaired config");
        assert_eq!(read_back, written);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
