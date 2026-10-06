//! Cloud secret storage, credential isolation, and role-separated access.
//!
//! Enforces zero-plaintext disk persistence for all cloud credentials (API tokens, workspace
//! keys, bearer secrets). Material is stored strictly in the platform OS credential manager
//! or, when explicitly requested, in an in-memory session-only fallback store.
//!
//! ## Invariants
//!
//! 1. **Zero Plaintext Fallback:** Cloud secrets NEVER fall back to plaintext files (`settings.json`,
//!    `inference.json`, or environment variables).
//! 2. **Canonical Origin Binding:** Every [`SecretKey`] binds the canonical origin fingerprint of
//!    the validated stored profile. Modifying a profile's endpoint URL prevents reuse of old credentials.
//! 3. **Role Separation:** `Setup`, `Runtime`, and `ModelDownload` tokens are stored and scoped
//!    separately and cannot overwrite or substitute for each other.
//! 4. **Compiler-Barrier Zeroization:** [`SecretValue`] derives [`zeroize::Zeroize`] and
//!    [`zeroize::ZeroizeOnDrop`], never implements [`serde::Serialize`], and formats as `"[REDACTED]"`
//!    in both [`std::fmt::Debug`] and [`std::fmt::Display`].
//! 5. **Session-Only Override Semantics:** Storing a session-only credential retains the value in
//!    volatile memory for the current process without mutating or clearing any existing persistent
//!    OS keyring entry.
//! 6. **Sanitized Public Errors:** Error messages never echo secret content or raw backend messages.
//! 7. **Safe Summary Only:** Frontend commands only receive existence/backend status summaries
//!    ([`SecretSummary`]) and cannot read back stored secret strings.
//! 8. **Abstract Secret Backend:** Testable via [`SecretStore`] trait with an in-memory test double
//!    without touching the host OS keyring during unit tests.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use cleaner_core::engines::render::CloudProvider;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Service name used for OS keyring entries.
pub const KEYRING_SERVICE: &str = "com.mangacleaner.studio.cloud";

/// The distinct roles a cloud credential can fulfill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretRole {
    /// Full workspace credential used during automated setup and provisioning.
    Setup,
    /// Bounded, least-privilege token used for `/mc/v1` crop inference execution.
    Runtime,
    /// Token used for pulling model weights or snapshot archives.
    ModelDownload,
}

impl SecretRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            SecretRole::Setup => "setup",
            SecretRole::Runtime => "runtime",
            SecretRole::ModelDownload => "model_download",
        }
    }
}

/// The storage backend where a secret is currently retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackendKind {
    /// Operating system credential store (macOS Keychain, Windows Credential Manager, Linux Secret Service).
    Keyring,
    /// Volatile in-memory store retained only for the current application session.
    Session,
    /// Credential store is unreachable or unavailable.
    Unavailable,
}

/// Unique key identifying a stored secret, bound to the profile's canonical origin fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretKey {
    pub provider: CloudProvider,
    pub profile_id: String,
    pub canonical_origin_fingerprint: String,
    pub role: SecretRole,
}

impl SecretKey {
    pub fn new(
        provider: CloudProvider,
        profile_id: impl Into<String>,
        canonical_origin_fingerprint: impl Into<String>,
        role: SecretRole,
    ) -> Self {
        Self {
            provider,
            profile_id: profile_id.into(),
            canonical_origin_fingerprint: canonical_origin_fingerprint.into(),
            role,
        }
    }

    /// Construct the OS keyring account identifier.
    pub fn as_keyring_user(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.provider.as_str(),
            self.profile_id,
            self.canonical_origin_fingerprint,
            self.role.as_str()
        )
    }
}

/// An opaque, zeroize-on-drop wrapper around secret token bytes with compiler-barrier guarantees.
///
/// Intentionally does NOT implement [`serde::Serialize`] or [`serde::Deserialize`].
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretValue {
    bytes: Vec<u8>,
}

impl SecretValue {
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            bytes: secret.into().into_bytes(),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
        }
    }

    pub fn expose_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.bytes)
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl std::fmt::Display for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Versioned compound payload structure for Modal credentials.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModalCompoundSecret {
    v: u32,
    token_id: String,
    token_secret: String,
}

/// Encode Modal runtime credentials into a versioned opaque compound [`SecretValue`].
///
/// Strictly bounds and validates token ID and token secret without storing plaintext files.
pub fn encode_modal_runtime_secret(token_id: &str, token_secret: &str) -> Result<SecretValue, String> {
    if token_id.chars().any(|c| c.is_control()) {
        return Err("Modal token ID cannot contain control characters".to_string());
    }
    let tid = token_id.trim();
    if tid.is_empty() {
        return Err("Modal token ID cannot be empty".to_string());
    }
    if tid.len() > 256 {
        return Err("Modal token ID exceeds maximum length of 256 characters".to_string());
    }

    if token_secret.chars().any(|c| c.is_control()) {
        return Err("Modal token secret cannot contain control characters".to_string());
    }
    let tsec = token_secret.trim();
    if tsec.is_empty() {
        return Err("Modal token secret cannot be empty".to_string());
    }
    if tsec.len() > 1024 {
        return Err("Modal token secret exceeds maximum length of 1024 characters".to_string());
    }

    let compound = ModalCompoundSecret {
        v: 1,
        token_id: tid.to_string(),
        token_secret: tsec.to_string(),
    };

    let serialized = serde_json::to_string(&compound)
        .map_err(|e| format!("failed to serialize Modal compound credential: {e}"))?;

    Ok(SecretValue::new(serialized))
}

/// Decode Modal runtime credentials from an opaque compound [`SecretValue`].
///
/// Fails closed on legacy entries, incomplete entries, unsupported versions, or invalid JSON.
pub fn decode_modal_runtime_secret(secret: &SecretValue) -> Result<(String, SecretValue), String> {
    let raw_str = secret
        .expose_str()
        .map_err(|_| "Modal credential is not valid UTF-8".to_string())?;

    let parsed: ModalCompoundSecret = serde_json::from_str(raw_str)
        .map_err(|_| "legacy or invalid Modal credential format; please re-store credential".to_string())?;

    if parsed.v != 1 {
        return Err(format!(
            "unsupported Modal compound credential schema version {}",
            parsed.v
        ));
    }

    if parsed.token_id.chars().any(|c| c.is_control()) {
        return Err("corrupted Modal token ID in stored credential".to_string());
    }
    let tid = parsed.token_id.trim();
    if tid.is_empty() || tid.len() > 256 {
        return Err("corrupted Modal token ID in stored credential".to_string());
    }

    if parsed.token_secret.chars().any(|c| c.is_control()) {
        return Err("corrupted Modal token secret in stored credential".to_string());
    }
    let tsec = parsed.token_secret.trim();
    if tsec.is_empty() || tsec.len() > 1024 {
        return Err("corrupted Modal token secret in stored credential".to_string());
    }

    Ok((tid.to_string(), SecretValue::new(tsec)))
}

/// Safe public summary of credential presence and storage backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretSummary {
    pub provider: CloudProvider,
    pub profile_id: String,
    pub role: SecretRole,
    pub present: bool,
    pub backend: StorageBackendKind,
}

/// Pluggable credential storage interface.
pub trait SecretStore: Send + Sync {
    fn get_secret(&self, key: &SecretKey) -> Result<Option<SecretValue>, String>;
    fn set_secret(&self, key: &SecretKey, secret: &SecretValue) -> Result<(), String>;
    fn delete_secret(&self, key: &SecretKey) -> Result<(), String>;
    fn has_secret(&self, key: &SecretKey) -> Result<bool, String>;
    fn backend_kind(&self) -> StorageBackendKind;
    fn is_available(&self) -> bool;
}

/// Platform OS Keyring implementation backed by the `keyring` crate.
pub struct OsKeyringSecretStore;

impl OsKeyringSecretStore {
    fn entry(key: &SecretKey) -> Result<keyring::Entry, String> {
        let user = key.as_keyring_user();
        keyring::Entry::new(KEYRING_SERVICE, &user)
            .map_err(|_| "OS credential store initialization failed".to_string())
    }
}

/// The error a [`SecretStore`] read returns when the person cancelled the
/// keychain prompt. [`SecretManager`] remembers this answer rather than asking
/// again, until the person acts on the credential or opens its settings.
pub const READ_DENIED: &str = "OS credential store access was denied";

/// Whether a platform failure is the person cancelling the prompt, as opposed
/// to the store failing. A locked or non-interactive keychain
/// (`errSecInteractionNotAllowed`) and a failed unlock (`errSecAuthFailed`) are
/// not a No from the person, so they stay ordinary read failures and the next
/// read tries again.
#[cfg(target_os = "macos")]
fn is_denial(err: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    /// `errSecUserCanceled`.
    const USER_CANCELED: i32 = -128;
    err.downcast_ref::<security_framework::base::Error>()
        .is_some_and(|err| err.code() == USER_CANCELED)
}

#[cfg(not(target_os = "macos"))]
fn is_denial(_err: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    false
}

impl SecretStore for OsKeyringSecretStore {
    fn get_secret(&self, key: &SecretKey) -> Result<Option<SecretValue>, String> {
        match Self::entry(key)?.get_password() {
            Ok(pwd) => Ok(Some(SecretValue::new(pwd))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring::Error::PlatformFailure(err)) if is_denial(err.as_ref()) => {
                Err(READ_DENIED.to_string())
            }
            Err(keyring::Error::NoStorageAccess(_)) => {
                Err("OS credential store is locked or inaccessible".to_string())
            }
            Err(keyring::Error::PlatformFailure(_)) => {
                Err("OS credential store service is unreachable".to_string())
            }
            Err(_) => Err("OS credential store read failed".to_string()),
        }
    }

    fn set_secret(&self, key: &SecretKey, secret: &SecretValue) -> Result<(), String> {
        let str_val = secret
            .expose_str()
            .map_err(|_| "credential contains invalid UTF-8 bytes".to_string())?;
        #[cfg(target_os = "macos")]
        let written = crate::macos_keychain::set_password(KEYRING_SERVICE, &key.as_keyring_user(), str_val);
        #[cfg(not(target_os = "macos"))]
        let written = Self::entry(key)?.set_password(str_val);
        match written {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoStorageAccess(_)) => {
                Err("OS credential store is locked or inaccessible".to_string())
            }
            Err(keyring::Error::PlatformFailure(_)) => {
                Err("OS credential store service is unreachable".to_string())
            }
            Err(_) => Err("OS credential store write failed".to_string()),
        }
    }

    fn delete_secret(&self, key: &SecretKey) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        let deleted = crate::macos_keychain::delete_password(KEYRING_SERVICE, &key.as_keyring_user());
        #[cfg(not(target_os = "macos"))]
        let deleted = Self::entry(key)?.delete_credential();
        match deleted {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(keyring::Error::NoStorageAccess(_)) => {
                Err("OS credential store is locked or inaccessible".to_string())
            }
            Err(keyring::Error::PlatformFailure(_)) => {
                Err("OS credential store service is unreachable".to_string())
            }
            Err(_) => Err("OS credential store deletion failed".to_string()),
        }
    }

    fn has_secret(&self, key: &SecretKey) -> Result<bool, String> {
        #[cfg(target_os = "macos")]
        {
            macos_item_exists(key)
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.get_secret(key).map(|opt| opt.is_some())
        }
    }

    fn backend_kind(&self) -> StorageBackendKind {
        StorageBackendKind::Keyring
    }

    fn is_available(&self) -> bool {
        cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux"))
    }
}

/// Whether the login keychain holds an item for `key`, asked without reading its data.
///
/// The keychain guards an item's data with an access list naming the build that
/// wrote it. An unsigned or ad-hoc signed build is named by its code hash, so
/// every update is a stranger to it and reading the data prompts for the login
/// password. Attributes are not guarded, so this check never prompts: a summary
/// after an update still says the token is there, and the one prompt waits for
/// the first request that actually needs the token.
#[cfg(target_os = "macos")]
fn macos_item_exists(key: &SecretKey) -> Result<bool, String> {
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
    use security_framework::os::macos::keychain::{SecKeychain, SecPreferencesDomain};

    /// `errSecItemNotFound`.
    const ITEM_NOT_FOUND: i32 = -25300;

    let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User)
        .map_err(|_| "OS credential store is locked or inaccessible".to_string())?;
    let user = key.as_keyring_user();
    let found = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .keychains(&[keychain])
        .service(KEYRING_SERVICE)
        .account(&user)
        .load_attributes(true)
        .limit(Limit::Max(1))
        .search();
    match found {
        Ok(items) => Ok(!items.is_empty()),
        Err(err) if err.code() == ITEM_NOT_FOUND => Ok(false),
        Err(_) => Err("OS credential store read failed".to_string()),
    }
}

/// In-memory volatile secret store used for tests and session-only fallback.
pub struct MemorySecretStore {
    store: Mutex<HashMap<SecretKey, SecretValue>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for MemorySecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for MemorySecretStore {
    fn get_secret(&self, key: &SecretKey) -> Result<Option<SecretValue>, String> {
        let map = self
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        Ok(map.get(key).cloned())
    }

    fn set_secret(&self, key: &SecretKey, secret: &SecretValue) -> Result<(), String> {
        let mut map = self
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        map.insert(key.clone(), secret.clone());
        Ok(())
    }

    fn delete_secret(&self, key: &SecretKey) -> Result<(), String> {
        let mut map = self
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        map.remove(key);
        Ok(())
    }

    fn has_secret(&self, key: &SecretKey) -> Result<bool, String> {
        let map = self
            .store
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        Ok(map.contains_key(key))
    }

    fn backend_kind(&self) -> StorageBackendKind {
        StorageBackendKind::Session
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// High-level manager coordinating OS keyring storage and session fallback.
///
/// `unlocked` holds what this process has already read from the primary store,
/// so the OS asks the person at most once per key per launch rather than once per
/// request. It is volatile, zeroized on drop like every [`SecretValue`], and
/// every store or delete through this manager clears the entry it replaces.
/// A successful persistent store then seeds it with the value just written:
/// setup's health check need not read the same credential back from Keychain.
///
/// `reads` keeps the rest of what a primary read taught this process, per key:
///
/// - **One read at a time.** Every caller that misses `unlocked` while a read of
///   the same key is in flight waits for it and shares its answer, so a burst
///   of polls, renders and status checks raises one prompt rather than one each.
/// - **Answers that are not a value.** "No entry" and "the person cancelled"
///   are remembered too, so a poll after a cancelled prompt does not prompt
///   again. Other failures are not: they are the store failing, not the person.
///   [`SecretManager::forget_denials`] drops the cancels when the person asks
///   for the credential again, such as by opening its settings.
/// - **A generation per key.** A read that waited on the OS (a keychain prompt
///   can take minutes) only fills the cache if the same key was not stored or
///   deleted while it waited, and the check and the fill happen under the same
///   lock a write takes, so a credential deleted or rotated meanwhile is never
///   put back. Writes to other keys do not discard it.
///
/// Every store or delete clears the previous read state for its key; a
/// successful persistent store remembers the new value immediately.
pub struct SecretManager {
    primary: Box<dyn SecretStore>,
    session: MemorySecretStore,
    unlocked: MemorySecretStore,
    reads: Mutex<ReadState>,
    /// Serialize mutations so a delayed save cannot cache a value after a
    /// newer save or deletion. Reads remain free to finish their OS prompt.
    writes: Mutex<()>,
}

/// A primary read's answer, as [`SecretManager::get_secret`] hands it out.
type ReadResult = Result<Option<SecretValue>, String>;

/// What a finished primary read found other than a value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Remembered {
    /// The primary store has no entry for the key.
    Absent,
    /// The person cancelled the read ([`READ_DENIED`]).
    Denied,
}

#[derive(Default)]
struct ReadState {
    generations: HashMap<SecretKey, u64>,
    remembered: HashMap<SecretKey, Remembered>,
    in_flight: HashMap<SecretKey, Arc<Flight>>,
}

/// One primary read in progress. Its result holds a [`SecretValue`], so it is
/// zeroized when the last caller sharing it lets go.
struct Flight {
    result: Mutex<Option<ReadResult>>,
    done: Condvar,
}

impl Flight {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            done: Condvar::new(),
        }
    }

    fn finish(&self, result: ReadResult) {
        let mut slot = self.result.lock().unwrap_or_else(|p| p.into_inner());
        *slot = Some(result);
        self.done.notify_all();
    }

    fn wait(&self) -> ReadResult {
        let mut slot = self.result.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(result) = slot.as_ref() {
                return result.clone();
            }
            slot = self.done.wait(slot).unwrap_or_else(|p| p.into_inner());
        }
    }
}

impl SecretManager {
    pub fn new(primary: Box<dyn SecretStore>) -> Self {
        Self {
            primary,
            session: MemorySecretStore::new(),
            unlocked: MemorySecretStore::new(),
            reads: Mutex::new(ReadState::default()),
            writes: Mutex::new(()),
        }
    }

    fn lock_reads(&self) -> std::sync::MutexGuard<'_, ReadState> {
        self.reads.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record a write to `key` and forget what was read for it.
    fn invalidate(&self, key: &SecretKey) -> Result<(), String> {
        let mut state = self.lock_reads();
        self.invalidate_locked(&mut state, key)
    }

    fn invalidate_locked(&self, state: &mut ReadState, key: &SecretKey) -> Result<(), String> {
        let generation = state.generations.entry(key.clone()).or_insert(0);
        *generation = generation.wrapping_add(1);
        state.remembered.remove(key);
        // A read already in flight still answers the callers waiting on it,
        // and fails them as stale; the next caller starts a fresh one.
        state.in_flight.remove(key);
        self.unlocked.delete_secret(key)
    }

    /// Settle a primary read of `key` begun at `generation`: cache what it
    /// taught us, release the callers waiting on `flight`, and answer.
    fn land(&self, key: &SecretKey, flight: &Arc<Flight>, generation: u64, read: ReadResult) -> ReadResult {
        let result = {
            let mut state = self.lock_reads();
            if state.in_flight.get(key).is_some_and(|f| Arc::ptr_eq(f, flight)) {
                state.in_flight.remove(key);
            }
            if state.generations.get(key).copied().unwrap_or(0) != generation {
                // A keychain prompt can outlive a delete or rotation. The value it
                // returned is stale even if we do not put it in the cache.
                Err("credential changed during read".to_string())
            } else {
                match read {
                    Ok(Some(sec)) => self.unlocked.set_secret(key, &sec).map(|()| Some(sec)),
                    Ok(None) => {
                        state.remembered.insert(key.clone(), Remembered::Absent);
                        Ok(None)
                    }
                    Err(err) if err == READ_DENIED => {
                        state.remembered.insert(key.clone(), Remembered::Denied);
                        Err(READ_DENIED.to_string())
                    }
                    Err(_) => Err("OS credential store read failed".to_string()),
                }
            }
        };
        flight.finish(result.clone());
        result
    }

    /// Forget every remembered cancel, so the next read of those keys may ask
    /// the person again. Called only on an explicit action of theirs, never
    /// from a poll: a cancel still answers every background read until then.
    pub fn forget_denials(&self) {
        self.lock_reads()
            .remembered
            .retain(|_, answer| *answer != Remembered::Denied);
    }

    /// Isolated in-memory secret manager for unit tests.
    pub fn new_in_memory() -> Self {
        Self::new(Box::new(MemorySecretStore::new()))
    }

    /// Global standard secret manager.
    pub fn global() -> &'static SecretManager {
        static INSTANCE: OnceLock<SecretManager> = OnceLock::new();
        INSTANCE.get_or_init(|| SecretManager::new(Box::new(OsKeyringSecretStore)))
    }

    /// Store a cloud credential.
    ///
    /// When `session_only` is true, the credential is held in the volatile in-memory store
    /// for the current session without modifying or deleting any pre-existing persistent OS keyring entry.
    pub fn store_secret(
        &self,
        key: &SecretKey,
        secret: SecretValue,
        session_only: bool,
    ) -> Result<SecretSummary, String> {
        if secret.is_empty() {
            return Err("cannot store empty secret".to_string());
        }
        let _write = self.writes.lock().unwrap_or_else(|p| p.into_inner());

        if session_only {
            self.invalidate(key)?;
            let stored = self.session.set_secret(key, &secret);
            self.invalidate(key)?;
            stored?;
            Ok(SecretSummary {
                provider: key.provider,
                profile_id: key.profile_id.clone(),
                role: key.role,
                present: true,
                backend: StorageBackendKind::Session,
            })
        } else if self.primary.is_available() {
            // Forget the old value before and after: a failed write must not
            // leave it answering, and a read racing the write must not cache it.
            self.invalidate(key)?;
            let written = self.primary.set_secret(key, &secret);
            // Invalidation and seeding share the read-state lock. A reader
            // racing the OS write cannot restore the previous value afterward.
            let mut state = self.lock_reads();
            self.invalidate_locked(&mut state, key)?;
            written.map_err(|_| "OS credential store write failed".to_string())?;
            // Clean up session override copy if now storing persistently in OS keyring
            self.session.delete_secret(key)?;
            self.unlocked.set_secret(key, &secret)?;
            Ok(SecretSummary {
                provider: key.provider,
                profile_id: key.profile_id.clone(),
                role: key.role,
                present: true,
                backend: StorageBackendKind::Keyring,
            })
        } else {
            Err("OS credential store unavailable on this host; use session-only storage".to_string())
        }
    }

    /// Delete a cloud credential across both keyring and session stores.
    ///
    /// Never claims successful deletion if the OS credential manager failed.
    pub fn delete_secret(&self, key: &SecretKey) -> Result<SecretSummary, String> {
        let _write = self.writes.lock().unwrap_or_else(|p| p.into_inner());
        // A failed persistent deletion must never be reported as complete.
        self.session.delete_secret(key)?;
        self.invalidate(key)?;
        if !self.primary.is_available() {
            return Err("OS credential store unavailable; persistent deletion unconfirmed".into());
        }
        let deleted = self.primary.delete_secret(key);
        self.invalidate(key)?;
        deleted.map_err(|_| "OS credential store deletion failed".to_string())?;

        Ok(SecretSummary {
            provider: key.provider,
            profile_id: key.profile_id.clone(),
            role: key.role,
            present: false,
            backend: StorageBackendKind::Keyring,
        })
    }

    /// Get safe summary of credential presence.
    pub fn get_summary(&self, key: &SecretKey) -> Result<SecretSummary, String> {
        if self.session.has_secret(key)? {
            return Ok(SecretSummary {
                provider: key.provider,
                profile_id: key.profile_id.clone(),
                role: key.role,
                present: true,
                backend: StorageBackendKind::Session,
            });
        }

        if self.unlocked.has_secret(key)? {
            return Ok(SecretSummary {
                provider: key.provider,
                profile_id: key.profile_id.clone(),
                role: key.role,
                present: true,
                backend: StorageBackendKind::Keyring,
            });
        }

        if self.primary.is_available() {
            match self.primary.has_secret(key) {
                Ok(present) => Ok(SecretSummary {
                    provider: key.provider,
                    profile_id: key.profile_id.clone(),
                    role: key.role,
                    present,
                    backend: StorageBackendKind::Keyring,
                }),
                Err(_) => Ok(SecretSummary {
                    provider: key.provider,
                    profile_id: key.profile_id.clone(),
                    role: key.role,
                    present: false,
                    backend: StorageBackendKind::Unavailable,
                }),
            }
        } else {
            Ok(SecretSummary {
                provider: key.provider,
                profile_id: key.profile_id.clone(),
                role: key.role,
                present: false,
                backend: StorageBackendKind::Unavailable,
            })
        }
    }

    /// Backend-only retrieval of secret material for authenticating `/mc/v1` requests.
    ///
    /// Asks the primary store at most once per key until that key is written:
    /// concurrent callers share the read in flight, and a cancel answers
    /// [`READ_DENIED`] from memory until [`Self::forget_denials`].
    pub fn get_secret(&self, key: &SecretKey) -> Result<Option<SecretValue>, String> {
        if let Some(sec) = self.session.get_secret(key)? {
            return Ok(Some(sec));
        }
        if !self.primary.is_available() {
            return Ok(None);
        }
        let (flight, generation) = {
            let mut state = self.lock_reads();
            if let Some(sec) = self.unlocked.get_secret(key)? {
                return Ok(Some(sec));
            }
            match state.remembered.get(key) {
                Some(Remembered::Absent) => return Ok(None),
                Some(Remembered::Denied) => return Err(READ_DENIED.to_string()),
                None => {}
            }
            if let Some(flight) = state.in_flight.get(key).cloned() {
                drop(state);
                return flight.wait();
            }
            let flight = Arc::new(Flight::new());
            state.in_flight.insert(key.clone(), Arc::clone(&flight));
            (flight, state.generations.get(key).copied().unwrap_or(0))
        };
        // Outside the lock: a prompt can take minutes, and a store or delete
        // must not wait on it.
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.primary.get_secret(key)));
        match read {
            Ok(read) => self.land(key, &flight, generation, read),
            Err(panic) => {
                // Release the waiters before unwinding, or they wait forever.
                let _ = self.land(key, &flight, generation, Err("OS credential store read failed".to_string()));
                std::panic::resume_unwind(panic)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_value_redacted_in_debug_and_display() {
        let val = SecretValue::new("super-secret-token-123");
        assert_eq!(format!("{val:?}"), "[REDACTED]");
        assert_eq!(format!("{val}"), "[REDACTED]");
        assert_eq!(val.expose_str().unwrap(), "super-secret-token-123");
        assert_eq!(val.len(), "super-secret-token-123".len());
        assert!(!val.is_empty());
    }

    #[test]
    fn role_and_provider_isolation() {
        let store = MemorySecretStore::new();

        let beam_runtime = SecretKey::new(
            CloudProvider::Beam,
            "prof-1",
            "fingerprint-a",
            SecretRole::Runtime,
        );
        let beam_setup = SecretKey::new(
            CloudProvider::Beam,
            "prof-1",
            "fingerprint-a",
            SecretRole::Setup,
        );
        let modal_runtime = SecretKey::new(
            CloudProvider::Modal,
            "prof-1",
            "fingerprint-a",
            SecretRole::Runtime,
        );

        store
            .set_secret(&beam_runtime, &SecretValue::new("beam-runtime-token"))
            .unwrap();
        store
            .set_secret(&beam_setup, &SecretValue::new("beam-setup-token"))
            .unwrap();
        store
            .set_secret(&modal_runtime, &SecretValue::new("modal-runtime-token"))
            .unwrap();

        assert_eq!(
            store
                .get_secret(&beam_runtime)
                .unwrap()
                .unwrap()
                .expose_str()
                .unwrap(),
            "beam-runtime-token"
        );
        assert_eq!(
            store
                .get_secret(&beam_setup)
                .unwrap()
                .unwrap()
                .expose_str()
                .unwrap(),
            "beam-setup-token"
        );
        assert_eq!(
            store
                .get_secret(&modal_runtime)
                .unwrap()
                .unwrap()
                .expose_str()
                .unwrap(),
            "modal-runtime-token"
        );

        // Delete beam runtime does not affect others
        store.delete_secret(&beam_runtime).unwrap();
        assert!(store.get_secret(&beam_runtime).unwrap().is_none());
        assert!(store.get_secret(&beam_setup).unwrap().is_some());
        assert!(store.get_secret(&modal_runtime).unwrap().is_some());
    }

    #[test]
    fn endpoint_origin_fingerprint_change_prevents_credential_reuse() {
        let fake_keyring = Box::new(MemorySecretStore::new());
        let manager = SecretManager::new(fake_keyring);

        let orig_key = SecretKey::new(
            CloudProvider::Modal,
            "modal-prod",
            "origin-fp-endpoint-v1",
            SecretRole::Runtime,
        );

        // Store credential under origin v1
        manager
            .store_secret(&orig_key, SecretValue::new("modal-key-v1"), false)
            .unwrap();

        assert_eq!(
            manager
                .get_secret(&orig_key)
                .unwrap()
                .unwrap()
                .expose_str()
                .unwrap(),
            "modal-key-v1"
        );

        // User changes endpoint URL -> resulting in origin-fp-endpoint-v2
        let updated_key = SecretKey::new(
            CloudProvider::Modal,
            "modal-prod",
            "origin-fp-endpoint-v2",
            SecretRole::Runtime,
        );

        // Querying under new origin must NOT find old credential
        let summary = manager.get_summary(&updated_key).unwrap();
        assert!(!summary.present, "Old credentials must not leak to new endpoint origin");
        assert!(manager.get_secret(&updated_key).unwrap().is_none());
    }

    #[test]
    fn session_override_retains_persistent_credential() {
        let fake_keyring = Box::new(MemorySecretStore::new());
        let manager = SecretManager::new(fake_keyring);

        let key = SecretKey::new(
            CloudProvider::Modal,
            "modal-dev",
            "fingerprint-1234",
            SecretRole::Runtime,
        );

        // Store persistent credential
        manager
            .store_secret(&key, SecretValue::new("persistent-token"), false)
            .unwrap();

        // Store session-only override
        manager
            .store_secret(&key, SecretValue::new("session-token"), true)
            .unwrap();

        // Active query returns session token
        let active = manager.get_secret(&key).unwrap().unwrap();
        assert_eq!(active.expose_str().unwrap(), "session-token");
        assert_eq!(manager.get_summary(&key).unwrap().backend, StorageBackendKind::Session);
    }

    #[test]
    fn sanitized_error_messages_contain_no_secrets() {
        struct FailingStore(bool);
        impl SecretStore for FailingStore {
            fn get_secret(&self, _key: &SecretKey) -> Result<Option<SecretValue>, String> {
                Err("OS credential store read failed".to_string())
            }
            fn set_secret(&self, _key: &SecretKey, _secret: &SecretValue) -> Result<(), String> {
                Err(format!("backend echoed {}", _secret.expose_str().unwrap()))
            }
            fn delete_secret(&self, _key: &SecretKey) -> Result<(), String> {
                Err("backend echoed super-secret-raw-token".to_string())
            }
            fn has_secret(&self, _key: &SecretKey) -> Result<bool, String> {
                Err("OS credential store read failed".to_string())
            }
            fn backend_kind(&self) -> StorageBackendKind {
                StorageBackendKind::Keyring
            }
            fn is_available(&self) -> bool {
                self.0
            }
        }

        let manager = SecretManager::new(Box::new(FailingStore(true)));
        let key = SecretKey::new(
            CloudProvider::Beam,
            "beam-test",
            "fingerprint-1",
            SecretRole::Runtime,
        );
        let secret = SecretValue::new("super-secret-raw-token");

        let store_err = manager.store_secret(&key, secret, false).unwrap_err();
        assert!(!store_err.contains("super-secret-raw-token"));
        assert!(store_err.contains("OS credential store write failed"));

        let delete_err = manager.delete_secret(&key).unwrap_err();
        assert!(!delete_err.contains("super-secret-raw-token"));
        assert!(delete_err.contains("deletion failed"));
        let unavailable = SecretManager::new(Box::new(FailingStore(false)));
        assert!(unavailable.delete_secret(&key).unwrap_err().contains("unconfirmed"));
    }

    /// A primary store that counts data reads, standing in for a keychain whose
    /// every data read may prompt. `delay` stands in for the prompt being open,
    /// and `deny` for the person cancelling it.
    struct CountingStore {
        inner: MemorySecretStore,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        delay: std::time::Duration,
        deny: bool,
        fail_writes: bool,
    }

    impl CountingStore {
        fn new(inner: MemorySecretStore, reads: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
            Self { inner, reads, delay: std::time::Duration::ZERO, deny: false, fail_writes: false }
        }
    }

    impl SecretStore for CountingStore {
        fn get_secret(&self, key: &SecretKey) -> Result<Option<SecretValue>, String> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(self.delay);
            if self.deny {
                return Err(READ_DENIED.to_string());
            }
            self.inner.get_secret(key)
        }
        fn set_secret(&self, key: &SecretKey, secret: &SecretValue) -> Result<(), String> {
            if self.fail_writes {
                return Err("write refused".into());
            }
            self.inner.set_secret(key, secret)
        }
        fn delete_secret(&self, key: &SecretKey) -> Result<(), String> {
            self.inner.delete_secret(key)
        }
        fn has_secret(&self, key: &SecretKey) -> Result<bool, String> {
            self.inner.has_secret(key)
        }
        fn backend_kind(&self) -> StorageBackendKind {
            StorageBackendKind::Keyring
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    #[test]
    fn primary_is_read_once_per_process_and_replaced_values_are_not_served() {
        let key = SecretKey::new(CloudProvider::Modal, "modal-prod", "fp", SecretRole::Runtime);
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inner = MemorySecretStore::new();
        inner.set_secret(&key, &SecretValue::new("from-last-launch")).unwrap();
        let manager = SecretManager::new(Box::new(CountingStore::new(inner, reads.clone())));
        let count = || reads.load(std::sync::atomic::Ordering::SeqCst);

        // A summary never reads the data.
        assert!(manager.get_summary(&key).unwrap().present);
        assert_eq!(count(), 0);

        // The first use reads once; later uses come from memory.
        for _ in 0..3 {
            assert_eq!(manager.get_secret(&key).unwrap().unwrap().expose_str().unwrap(), "from-last-launch");
        }
        assert_eq!(count(), 1);

        // A new value replaces the remembered one.
        manager.store_secret(&key, SecretValue::new("rotated"), false).unwrap();
        assert_eq!(manager.get_secret(&key).unwrap().unwrap().expose_str().unwrap(), "rotated");
        assert_eq!(count(), 1, "a saved value does not need another keychain read");

        // A deletion forgets it everywhere.
        manager.delete_secret(&key).unwrap();
        assert!(manager.get_secret(&key).unwrap().is_none());
        assert!(!manager.get_summary(&key).unwrap().present);
    }

    #[test]
    fn concurrent_callers_share_one_primary_read() {
        let key = SecretKey::new(CloudProvider::Modal, "modal-prod", "fp", SecretRole::Runtime);
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inner = MemorySecretStore::new();
        inner.set_secret(&key, &SecretValue::new("from-last-launch")).unwrap();
        let mut primary = CountingStore::new(inner, reads.clone());
        primary.delay = std::time::Duration::from_millis(200);
        let manager = std::sync::Arc::new(SecretManager::new(Box::new(primary)));

        // Eight callers miss the cache together while the one prompt is open.
        const CALLERS: usize = 8;
        let start = std::sync::Arc::new(std::sync::Barrier::new(CALLERS));
        let handles: Vec<_> = (0..CALLERS)
            .map(|_| {
                let (manager, start, key) = (manager.clone(), start.clone(), key.clone());
                std::thread::spawn(move || {
                    start.wait();
                    manager.get_secret(&key).unwrap().unwrap().expose_str().unwrap().to_owned()
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), "from-last-launch");
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1, "one prompt, not one per caller");

        // A write to another key does not throw the answer away.
        let other = SecretKey::new(CloudProvider::Modal, "modal-prod", "fp", SecretRole::Setup);
        manager.store_secret(&other, SecretValue::new("setup"), false).unwrap();
        assert!(manager.get_secret(&key).unwrap().is_some());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_denied_or_empty_read_is_not_retried_until_a_write() {
        let key = SecretKey::new(CloudProvider::Beam, "beam-prod", "fp", SecretRole::Runtime);
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inner = MemorySecretStore::new();
        inner.set_secret(&key, &SecretValue::new("guarded")).unwrap();
        let mut primary = CountingStore::new(inner, reads.clone());
        primary.deny = true;
        let manager = SecretManager::new(Box::new(primary));
        let count = || reads.load(std::sync::atomic::Ordering::SeqCst);

        // The person cancels the prompt once; the polls after it do not ask again.
        for _ in 0..3 {
            assert_eq!(manager.get_secret(&key).unwrap_err(), READ_DENIED);
        }
        assert_eq!(count(), 1);

        // Storing the key is the person acting, so the refusal is forgotten.
        manager.store_secret(&key, SecretValue::new("session"), true).unwrap();
        assert_eq!(manager.get_secret(&key).unwrap().unwrap().expose_str().unwrap(), "session");
        manager.delete_secret(&key).unwrap();
        assert_eq!(manager.get_secret(&key).unwrap_err(), READ_DENIED);
        assert_eq!(count(), 2, "a delete forgets the refusal too");

        // "No entry" is remembered as well, until a store replaces it.
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let manager = SecretManager::new(Box::new(CountingStore::new(MemorySecretStore::new(), reads.clone())));
        for _ in 0..3 {
            assert!(manager.get_secret(&key).unwrap().is_none());
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        manager.store_secret(&key, SecretValue::new("stored"), false).unwrap();
        assert_eq!(manager.get_secret(&key).unwrap().unwrap().expose_str().unwrap(), "stored");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1,
            "a successful save replaces the absent answer without another prompt");
    }

    #[test]
    fn setup_and_runtime_saves_need_no_readback_for_checks_or_requests() {
        let runtime = SecretKey::new(CloudProvider::Modal, "new-install", "fp", SecretRole::Runtime);
        let setup = SecretKey::new(CloudProvider::Modal, "new-install", "fp", SecretRole::Setup);
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut primary = CountingStore::new(MemorySecretStore::new(), reads.clone());
        // Any data read would ask the user again and be denied.
        primary.deny = true;
        let manager = SecretManager::new(Box::new(primary));
        for (key, value) in [(&runtime, "runtime-token"), (&setup, "setup-token")] {
            let summary = manager.store_secret(key, SecretValue::new(value), false).unwrap();
            assert_eq!(summary.backend, StorageBackendKind::Keyring);
            for _ in 0..3 {
                assert!(manager.get_summary(key).unwrap().present);
                assert_eq!(manager.get_secret(key).unwrap().unwrap().expose_str().unwrap(), value);
            }
        }
        manager.forget_denials();
        assert!(manager.get_secret(&runtime).unwrap().is_some());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);

        manager.delete_secret(&runtime).unwrap();
        assert!(!manager.unlocked.has_secret(&runtime).unwrap());
        assert!(manager.get_secret(&setup).unwrap().is_some(), "deleting runtime keeps setup isolated");
    }

    #[test]
    fn a_failed_save_does_not_cache_the_unsaved_value() {
        let key = SecretKey::new(CloudProvider::Modal, "modal-prod", "fp", SecretRole::Runtime);
        let inner = MemorySecretStore::new();
        inner.set_secret(&key, &SecretValue::new("persisted")).unwrap();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut primary = CountingStore::new(inner, reads.clone());
        primary.fail_writes = true;
        let manager = SecretManager::new(Box::new(primary));
        assert!(manager.get_secret(&key).unwrap().is_some());
        assert!(manager.store_secret(&key, SecretValue::new("unsaved"), false).is_err());
        assert!(!manager.unlocked.has_secret(&key).unwrap());
        assert_eq!(manager.get_secret(&key).unwrap().unwrap().expose_str().unwrap(), "persisted");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn forgetting_denials_lets_the_next_read_ask_again_but_keeps_absent_answers() {
        let key = SecretKey::new(CloudProvider::Beam, "beam-prod", "fp", SecretRole::Runtime);
        let missing = SecretKey::new(CloudProvider::Beam, "beam-prod", "fp", SecretRole::Setup);
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inner = MemorySecretStore::new();
        inner.set_secret(&key, &SecretValue::new("guarded")).unwrap();
        let mut primary = CountingStore::new(inner, reads.clone());
        primary.deny = true;
        let manager = SecretManager::new(Box::new(primary));
        let count = || reads.load(std::sync::atomic::Ordering::SeqCst);

        assert_eq!(manager.get_secret(&key).unwrap_err(), READ_DENIED);
        assert_eq!(manager.get_secret(&key).unwrap_err(), READ_DENIED);
        assert_eq!(count(), 1);
        manager.forget_denials();
        assert_eq!(manager.get_secret(&key).unwrap_err(), READ_DENIED);
        assert_eq!(count(), 2, "the person asked again, so the keychain is asked again");
        assert_eq!(manager.get_secret(&key).unwrap_err(), READ_DENIED);
        assert_eq!(count(), 2, "the new cancel is remembered like the first");

        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let manager = SecretManager::new(Box::new(CountingStore::new(MemorySecretStore::new(), reads.clone())));
        assert!(manager.get_secret(&missing).unwrap().is_none());
        manager.forget_denials();
        assert!(manager.get_secret(&missing).unwrap().is_none());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1,
            "an absent answer is not a denial and stays remembered");
    }

    #[test]
    fn a_store_failure_is_not_remembered() {
        struct Locked(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl SecretStore for Locked {
            fn get_secret(&self, _key: &SecretKey) -> Result<Option<SecretValue>, String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("OS credential store is locked or inaccessible".to_string())
            }
            fn set_secret(&self, _key: &SecretKey, _secret: &SecretValue) -> Result<(), String> {
                Ok(())
            }
            fn delete_secret(&self, _key: &SecretKey) -> Result<(), String> {
                Ok(())
            }
            fn has_secret(&self, _key: &SecretKey) -> Result<bool, String> {
                Ok(true)
            }
            fn backend_kind(&self) -> StorageBackendKind {
                StorageBackendKind::Keyring
            }
            fn is_available(&self) -> bool {
                true
            }
        }
        let key = SecretKey::new(CloudProvider::Beam, "beam-prod", "fp", SecretRole::Runtime);
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let manager = SecretManager::new(Box::new(Locked(reads.clone())));
        for _ in 0..2 {
            let err = manager.get_secret(&key).unwrap_err();
            assert_ne!(err, READ_DENIED);
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2, "a locked keychain is asked again");
    }

    /// A primary whose read is interrupted by a credential change while a
    /// keychain prompt is open.
    struct ChangedWhileReading {
        inner: MemorySecretStore,
        manager: std::sync::Arc<std::sync::OnceLock<std::sync::Weak<SecretManager>>>,
        session_override: bool,
        persistent_rotation: bool,
    }

    impl SecretStore for ChangedWhileReading {
        fn get_secret(&self, key: &SecretKey) -> Result<Option<SecretValue>, String> {
            let value = self.inner.get_secret(key)?;
            if let Some(manager) = self.manager.get().and_then(std::sync::Weak::upgrade) {
                if self.persistent_rotation {
                    manager.store_secret(key, SecretValue::new("rotated"), false)?;
                } else if self.session_override {
                    manager.store_secret(key, SecretValue::new("session-override"), true)?;
                } else {
                    manager.delete_secret(key)?;
                }
            }
            Ok(value)
        }
        fn set_secret(&self, key: &SecretKey, secret: &SecretValue) -> Result<(), String> {
            self.inner.set_secret(key, secret)
        }
        fn delete_secret(&self, key: &SecretKey) -> Result<(), String> {
            self.inner.delete_secret(key)
        }
        fn has_secret(&self, key: &SecretKey) -> Result<bool, String> {
            self.inner.has_secret(key)
        }
        fn backend_kind(&self) -> StorageBackendKind {
            StorageBackendKind::Keyring
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_read_that_races_a_delete_does_not_bring_the_credential_back() {
        let key = SecretKey::new(CloudProvider::Modal, "modal-prod", "fp", SecretRole::Runtime);
        let inner = MemorySecretStore::new();
        inner.set_secret(&key, &SecretValue::new("old")).unwrap();
        let slot = std::sync::Arc::new(std::sync::OnceLock::new());
        let primary = ChangedWhileReading { inner, manager: slot.clone(), session_override: false, persistent_rotation: false };
        let manager = std::sync::Arc::new(SecretManager::new(Box::new(primary)));
        slot.set(std::sync::Arc::downgrade(&manager)).ok();

        // The in-flight read cannot hand the deleted value to a caller.
        assert_eq!(manager.get_secret(&key).unwrap_err(), "credential changed during read");
        assert!(!manager.unlocked.has_secret(&key).unwrap());
        assert!(!manager.get_summary(&key).unwrap().present);
    }

    #[test]
    fn a_read_that_races_a_session_override_cannot_return_the_old_secret() {
        let key = SecretKey::new(CloudProvider::Modal, "modal-prod", "fp", SecretRole::Runtime);
        let inner = MemorySecretStore::new();
        inner.set_secret(&key, &SecretValue::new("old")).unwrap();
        let slot = std::sync::Arc::new(std::sync::OnceLock::new());
        let primary = ChangedWhileReading { inner, manager: slot.clone(), session_override: true, persistent_rotation: false };
        let manager = std::sync::Arc::new(SecretManager::new(Box::new(primary)));
        slot.set(std::sync::Arc::downgrade(&manager)).ok();

        assert_eq!(manager.get_secret(&key).unwrap_err(), "credential changed during read");
        assert!(!manager.unlocked.has_secret(&key).unwrap());
        assert_eq!(manager.get_secret(&key).unwrap().unwrap().expose_str().unwrap(), "session-override");
    }

    #[test]
    fn a_read_that_races_a_persistent_save_cannot_replace_the_saved_cache() {
        let key = SecretKey::new(CloudProvider::Modal, "modal-prod", "fp", SecretRole::Runtime);
        let inner = MemorySecretStore::new();
        inner.set_secret(&key, &SecretValue::new("old")).unwrap();
        let slot = Arc::new(OnceLock::new());
        let primary = ChangedWhileReading {
            inner, manager: slot.clone(), session_override: false, persistent_rotation: true,
        };
        let manager = Arc::new(SecretManager::new(Box::new(primary)));
        slot.set(Arc::downgrade(&manager)).ok();

        assert_eq!(manager.get_secret(&key).unwrap_err(), "credential changed during read");
        assert_eq!(manager.get_secret(&key).unwrap().unwrap().expose_str().unwrap(), "rotated");
    }

    /// Against the real login keychain, for an item another program wrote (so
    /// this test binary is not on its access list): the probe must answer
    /// without a prompt. Run by hand with an item made by `security`, e.g.
    /// `security add-generic-password -s com.mangacleaner.studio.cloud -a <account> -w x`,
    /// then `MC_PROBE_ACCOUNT=<account> cargo test -- --ignored macos_probe`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn macos_probe_answers_without_reading_data() {
        let account = std::env::var("MC_PROBE_ACCOUNT").expect("MC_PROBE_ACCOUNT");
        let parts: Vec<&str> = account.split(':').collect();
        assert_eq!(parts.len(), 4, "account is provider:profile:fingerprint:role");
        let key = SecretKey::new(CloudProvider::Modal, parts[1], parts[2], SecretRole::Runtime);
        assert_eq!(key.as_keyring_user(), account);
        assert!(OsKeyringSecretStore.has_secret(&key).unwrap());
        let missing = SecretKey::new(CloudProvider::Modal, parts[1], "no-such-fingerprint", SecretRole::Runtime);
        assert!(!OsKeyringSecretStore.has_secret(&missing).unwrap());
    }

    #[test]
    fn memory_secret_store_poison_recovery() {
        use std::sync::Arc;

        let store = Arc::new(MemorySecretStore::new());
        let key = SecretKey::new(
            CloudProvider::Modal,
            "modal-test",
            "fingerprint-test",
            SecretRole::Runtime,
        );

        // Force a thread to panic while holding the memory store mutex
        let store_clone = Arc::clone(&store);
        let _ = std::thread::spawn(move || {
            let _guard = store_clone.store.lock().unwrap();
            panic!("intentional memory store poison panic");
        })
        .join();

        // Safe recovery: operations must succeed normally
        store.set_secret(&key, &SecretValue::new("token123")).expect("set after poison");
        assert!(store.has_secret(&key).expect("has after poison"));
        assert_eq!(
            store.get_secret(&key).expect("get after poison").unwrap().expose_str().unwrap(),
            "token123"
        );
        store.delete_secret(&key).expect("delete after poison");
        assert!(!store.has_secret(&key).expect("has after delete"));
    }

    #[test]
    fn modal_compound_secret_roundtrip_and_no_summary_leak() {
        let tid = "ak-genuine-token-id-12345";
        let tsec = "as-genuine-secret-67890";

        let compound = encode_modal_runtime_secret(tid, tsec).expect("encode valid compound");
        let (decoded_id, decoded_sec) = decode_modal_runtime_secret(&compound).expect("decode valid compound");

        assert_eq!(decoded_id, tid);
        assert_eq!(decoded_sec.expose_str().unwrap(), tsec);

        // Prove summaries never contain secret or token ID
        let store = Box::new(MemorySecretStore::new());
        let manager = SecretManager::new(store);
        let key = SecretKey::new(
            CloudProvider::Modal,
            "modal-prof-1",
            "fingerprint-1234",
            SecretRole::Runtime,
        );

        let summary = manager.store_secret(&key, compound, false).expect("store secret");
        let serialized_summary = serde_json::to_string(&summary).expect("serialize summary");

        assert!(!serialized_summary.contains(tid), "Token ID must not leak into summary");
        assert!(!serialized_summary.contains(tsec), "Token secret must not leak into summary");
        assert!(summary.present);
        assert_eq!(summary.provider, CloudProvider::Modal);
    }

    #[test]
    fn modal_compound_secret_fails_closed_on_legacy_or_malformed() {
        // Legacy plain secret (e.g. from prior versions) fails closed
        let legacy_plain = SecretValue::new("just-a-plain-raw-secret");
        let err = decode_modal_runtime_secret(&legacy_plain).unwrap_err();
        assert!(err.contains("legacy or invalid Modal credential format"));

        // Empty token ID fails
        assert!(encode_modal_runtime_secret("", "valid-secret").is_err());
        assert!(encode_modal_runtime_secret("   ", "valid-secret").is_err());

        // Empty token secret fails
        assert!(encode_modal_runtime_secret("valid-id", "").is_err());
        assert!(encode_modal_runtime_secret("valid-id", "   ").is_err());

        // Control characters fail
        assert!(encode_modal_runtime_secret("valid-id\n", "valid-secret").is_err());
        assert!(encode_modal_runtime_secret("valid-id", "valid-secret\0").is_err());

        // Oversized fails
        let long_id = "a".repeat(257);
        assert!(encode_modal_runtime_secret(&long_id, "valid-secret").is_err());
        let long_sec = "b".repeat(1025);
        assert!(encode_modal_runtime_secret("valid-id", &long_sec).is_err());

        // Wrong version fails closed
        let bad_ver_json = SecretValue::new(r#"{"v":2,"token_id":"id","token_secret":"sec"}"#);
        assert!(decode_modal_runtime_secret(&bad_ver_json).is_err());

        // Missing field fails closed
        let incomplete_json = SecretValue::new(r#"{"v":1,"token_id":"id"}"#);
        assert!(decode_modal_runtime_secret(&incomplete_json).is_err());
    }
}
