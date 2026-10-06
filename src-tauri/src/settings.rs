//! `readSettings` and `writeSettings`.
//!
//! A patch is **merged**
//! and the whole snapshot comes back. Two consequences the implementation turns
//! on:
//!
//! - The merge is shallow, one level, matching what the interface sends: it
//!   writes `{cloud: {allowed: true}}`-shaped patches only at the top level, and
//!   a deep merge would make "unset this key" unexpressible.
//! - The vocabulary belongs to the interface, not here. This module stores and
//!   returns JSON without knowing what a setting means, for the same reason
//!   the manifest's `settings` snapshot does: a second definition of the
//!   settings shape is a second thing to disagree with the first.
//!
//! The defaults therefore come from the interface too - the adapter sends them
//! once at startup, and until it does, an unwritten settings file reads as `{}`
//! and the interface's own defaults apply.
//!
//! ## One key does not live here
//!
//! `hfToken` arrives through the same patch as every other setting and is the
//! one that never lands in the file. [`write`] lifts it out and hands it to
//! [`crate::weights::store_token`], which puts it in the operating system's
//! credential store; the merged snapshot that comes back has no such key. Only
//! when there is no usable store - a headless Linux box, a keychain that will
//! not answer - does it stay here, which is still better than a token
//! the user has to paste again every launch.
//!
//! The interception is in `write` rather than in the command wrapper so that
//! there is one place a secret can leak into this file from, and so that a
//! second caller cannot acquire the old behaviour by accident. The *other*
//! direction is filtered at the commands instead, by `without_token`: `write`
//! and `read` are what `weights` itself calls to find the fallback copy, and a
//! filter down there would hide the token from the only code entitled to it.
//!
//! A Clear the credential store refuses is an `Err`, not a quiet success. The
//! two failures look identical from here - no store at all, or a store that
//! will not let go - and `weights::write_token` is what tells them apart; only
//! the second one can leave a secret behind, and the user has to hear about it.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Map, Value};

/// Where the file lives, under the app's config directory.
const FILE: &str = "settings.json";
static SETTINGS_WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(FILE))
        .map_err(|e| e.to_string())
}

pub fn read(app: &tauri::AppHandle) -> Result<Value, String> {
    let path = path(app)?;
    read_file(&path)
}

fn read_file(path: &std::path::Path) -> Result<Value, String> {
    match std::fs::read(path) {
        // No file yet is not an error: it is a first launch, and the interface's
        // own defaults are the right answer.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(err) => Err(format!("{}: {err}", path.display())),
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// The token a patch is asking to store, as a string.
///
/// **Only a string is a token**, and the empty string is the Clear. The
/// interface sends exactly those two - `writeSettings({hfToken: value.trim()})`
/// to save and `{hfToken: ''}` to clear - so anything else is a caller that has
/// gone wrong, and the old reading (`as_str().unwrap_or_default()`) turned
/// `0`, `false`, `null` and `{}` into a silent Clear: a patch with a typo in it
/// would delete the user's token and report success. `null` is refused with the
/// rest rather than made to mean "unset", because a merge patch has no unset
/// (see the module docs) and giving one key a private one would be a second
/// rule for the interface to know.
fn token_of(value: &Value) -> Result<&str, String> {
    value.as_str().map(str::trim).ok_or_else(|| {
        format!(
            "{} must be a string; the empty string clears it",
            crate::weights::TOKEN_SETTING
        )
    })
}

/// Recursively verify that a patch contains no cloud secrets, tokens, or inference configurations.
///
/// Cloud configuration and secrets must be persisted exclusively in `inference.json` and the
/// OS keyring via [`crate::inference`], never in `settings.json`.
fn check_no_cloud_secrets(key_path: &str, value: &Value) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let next_path = if key_path.is_empty() {
                    k.clone()
                } else {
                    format!("{key_path}.{k}")
                };
                let lower_k = k.to_ascii_lowercase();
                // HuggingFace token is handled separately by weights::store_token and is exempt.
                let reserved = lower_k.contains("token")
                    || lower_k.contains("secret")
                    || lower_k.contains("apikey")
                    || lower_k.contains("api_key")
                    || matches!(lower_k.as_str(), "inference" | "inferencesettings" | "beamprofiles" | "modalprofiles");
                if reserved && !(key_path.is_empty() && k == crate::weights::TOKEN_SETTING) {
                    return Err("cloud credentials and inference profiles require dedicated storage".to_string());
                }
                check_no_cloud_secrets(&next_path, v)?;
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let next_path = format!("{key_path}[{i}]");
                check_no_cloud_secrets(&next_path, v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The few keys whose shape this module does check, because a malformed value
/// would decide where a page is sent. `analysisTargets` routes detection
/// stages to the cloud, `cleanTarget` the cleaning and `denoiseTarget` the
/// denoise, so each is refused whole rather than half-read later.
fn check_known_shapes(patch: &Value) -> Result<(), String> {
    use crate::inference::cloud_clean::{CleanTarget, CLEAN_TARGET_KEY};
    use crate::inference::cloud_denoise::{DenoiseTarget, DENOISE_TARGET_KEY};
    use crate::inference::run_analysis::{AnalysisTargets, ANALYSIS_TARGETS_KEY};
    if let Some(targets) = patch.get(ANALYSIS_TARGETS_KEY) {
        AnalysisTargets::from_value(Some(targets))?;
    }
    if let Some(target) = patch.get(CLEAN_TARGET_KEY) {
        CleanTarget::from_value(Some(target))?;
    }
    if let Some(target) = patch.get(DENOISE_TARGET_KEY) {
        DenoiseTarget::from_value(Some(target))?;
    }
    check_denoise_defaults(patch)
}

/// `denoisePreset` names one of the shipped presets, and
/// `denoiseLocalSecondsPerPage` is what `benchmark_denoise_local` measured: a
/// positive finite number. Null clears either.
fn check_denoise_defaults(patch: &Value) -> Result<(), String> {
    match patch.get(DENOISE_PRESET_KEY) {
        None | Some(Value::Null) => {}
        Some(Value::String(id)) if cleaner_core::cloud_denoise_wire::preset(id).is_some() => {}
        Some(_) => return Err(format!("{DENOISE_PRESET_KEY} must name a denoise preset")),
    }
    match patch.get(DENOISE_SECONDS_KEY) {
        None | Some(Value::Null) => Ok(()),
        Some(value) if value.as_f64().is_some_and(|seconds| seconds.is_finite() && seconds > 0.0) => Ok(()),
        Some(_) => Err(format!("{DENOISE_SECONDS_KEY} must be a positive number of seconds")),
    }
}

pub const DENOISE_PRESET_KEY: &str = "denoisePreset";
pub const DENOISE_SECONDS_KEY: &str = "denoiseLocalSecondsPerPage";

/// Merge a patch and return the whole snapshot.
pub fn write(app: &tauri::AppHandle, patch: Value) -> Result<Value, String> {
    write_patch(&path(app)?, patch)
}

fn write_patch(path: &std::path::Path, mut patch: Value) -> Result<Value, String> {
    // Reject any cloud secrets or cloud configuration attempting to be stored in settings.json
    check_no_cloud_secrets("", &patch)?;
    check_known_shapes(&patch)?;

    // Taken out of the patch before the merge so that no path through this
    // function can write it into the file by accident. See the module docs.
    let token = match &mut patch {
        Value::Object(map) => map.remove(crate::weights::TOKEN_SETTING),
        _ => None,
    };

    let _guard = SETTINGS_WRITE_LOCK.lock().map_err(|e| e.to_string())?;
    let mut current = match read_file(path)? {
        Value::Object(map) => map,
        // A settings file that is not an object is not repairable by merging
        // into it; start again rather than lose the patch.
        _ => Map::new(),
    };
    let prior_backend = (current.get("accelerator").cloned(), current.get("modelAccelerators").cloned());
    if let Value::Object(patch) = patch {
        for (key, value) in patch {
            current.insert(key, value);
        }
    }

    // A refused Clear is reported **after** the file is written, not instead of
    // writing it. The patch that carried the token can carry other settings
    // too, and an early return dropped every one of them to report a failure
    // about a different key.
    let mut refusal = None;
    if let Some(token) = token {
        let token = token_of(&token)?;
        match crate::weights::store_token(token) {
            // The credential store took it, or removed it. Either way no
            // plaintext copy survives - including any this file was still
            // holding from before the migration.
            crate::weights::TokenWrite::Stored => {
                current.remove(crate::weights::TOKEN_SETTING);
            }
            // No usable store, so this file is the store. An empty value is a
            // Clear and removes the key rather than writing `""`.
            crate::weights::TokenWrite::Fallback if !token.is_empty() => {
                current
                    .insert(crate::weights::TOKEN_SETTING.to_owned(), Value::String(token.to_owned()));
            }
            crate::weights::TokenWrite::Fallback => {
                current.remove(crate::weights::TOKEN_SETTING);
            }
            // The store still holds the secret the user asked to destroy. The
            // copy that *can* be destroyed still is - the user asked for that
            // too - and then the press fails, because answering `Ok` would
            // report a deletion that only half happened.
            crate::weights::TokenWrite::NotCleared(err) => {
                current.remove(crate::weights::TOKEN_SETTING);
                refusal = Some(err);
            }
        }
    }

    let merged = Value::Object(current);
    write_file(path, &merged)?;
    if let Value::Object(map) = &merged {
        if prior_backend != (map.get("accelerator").cloned(), map.get("modelAccelerators").cloned()) {
            use cleaner_core::registry::Kind;
            for kind in [Kind::TextDetector, Kind::BalloonDetector, Kind::ScriptGate, Kind::Ocr, Kind::Inpainter] {
                cleaner_core::residency::evict(kind);
            }
        }
    }
    match refusal {
        Some(err) => Err(format!("the credential store kept the token: {err}")),
        None => Ok(merged),
    }
}

/// Write the whole settings object, atomically and privately.
///
/// One place, because there are two callers and both of them can be holding the
/// Hugging Face token: on a machine with no credential store this file *is* the
/// store, and
/// [`cleaner_core::project::buffers::write_atomic`] creates its temporary file
/// with the process umask - 0644 on a typical Linux box, which is a secret
/// every account on the machine can read.
///
/// The permissions are narrowed unconditionally rather than only when a token
/// is present: a file that is 0600 sometimes is a file whose permissions say
/// something about its contents, and settings are small and rarely written, so
/// there is nothing to save by being clever.
///
/// **A failure to narrow is only fatal when there is something to protect.**
/// Narrowing is still attempted every time - that is the paragraph above - but
/// on unix a `chmod` of a file this process has just created does not fail, and
/// on Windows [`restrict`] can fail for a reason that is about the volume
/// rather than about this file: an exFAT stick or a redirected profile share
/// carries no ACLs at all, and `SetNamedSecurityInfoW` says so. Refusing to
/// save the theme on such a machine would trade a secret nobody stored for a
/// settings dialog that does not work. So the refusal is reported when the
/// snapshot going to disk actually carries the token, and swallowed when it
/// does not.
fn write_file(path: &std::path::Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // The same temp-and-rename the manifest gets. Settings are small and rarely
    // written, and a truncated settings file on a power cut is a first-launch
    // experience for someone who has been using the application for a year.
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    cleaner_core::project::buffers::write_atomic(path, &bytes).map_err(|e| e.to_string())?;
    let holds_secret = value.get(crate::weights::TOKEN_SETTING).is_some();
    match restrict(path) {
        Ok(()) => Ok(()),
        Err(err) if holds_secret => Err(err),
        Err(_) => Ok(()),
    }
}

/// Narrow a file to its owner.
///
/// There are two arms and no third one. A platform that is neither `unix` nor
/// `windows` fails to compile here, which is the failure to want: the arm that
/// used to catch everything else was `Ok(())`, and a silent success is how a
/// secret ends up in a world-readable file with nothing to show for it.
#[cfg(unix)]
fn restrict(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The same on Windows, where the mode bits do not decide and a DACL does.
///
/// This used to be `Ok(())` - a silent no-op in front of a file that holds the
/// Hugging Face token whenever the Credential Manager will not answer, which is
/// what `weights::read_token`'s fallback is for. The exposure that left is
/// narrower than it looks and is worth stating exactly, because it is what this
/// function is measured against:
///
/// - `%APPDATA%` under the user's profile already inherits an ACL naming that
///   user, SYSTEM and the local Administrators group, so a second *standard*
///   account on the same machine could never read it. That was true before this
///   existed and is not what changed.
/// - What could read it was anything running as an administrator or as SYSTEM,
///   and - the case that matters - anything at all that runs **as the user**,
///   because inheritance grants the whole account rather than this application.
///   A token in a file is a token every process the user starts can read.
///
/// A protected DACL with one access-allowed entry closes the first of those and
/// narrows nothing about the second: no file permission can, on any of the three
/// platforms, which is exactly why the token belongs in the credential store and
/// why this file is the fallback rather than the design. What it does buy is the
/// same statement `0600` makes on unix - the permissions on this file say
/// something about its contents - and it takes SYSTEM and Administrators off the
/// list, which is the difference between a secret an elevated process can read
/// and one it has to take ownership of first.
///
/// `PROTECTED_DACL_SECURITY_INFORMATION` is the half that does the work: without
/// it the inherited entries are merged back in and the new one changes nothing.
#[cfg(windows)]
fn restrict(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
        SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let failed = |what: &str, err: std::io::Error| format!("{}: {what}: {err}", path.display());

    // Every `W` entry point wants a wide, NUL-terminated string, and a `Path`
    // is not one.
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    // SAFETY: each call below is checked before its output is read, the token
    // handle is closed on both paths out of the pair of `GetTokenInformation`
    // calls, and the SID handed to `SetEntriesInAclW` points into `buffer`,
    // which outlives the ACL that is built from it.
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(failed("OpenProcessToken", std::io::Error::last_os_error()));
        }
        // Twice, because only the first call knows how long a SID is. The first
        // is expected to fail with `ERROR_INSUFFICIENT_BUFFER`; what is read
        // from it is the length, not the status.
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            CloseHandle(token);
            return Err(failed("GetTokenInformation", std::io::Error::last_os_error()));
        }
        let mut buffer = vec![0u8; len as usize];
        let read = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), len, &mut len);
        let last = std::io::Error::last_os_error();
        CloseHandle(token);
        if read == 0 {
            return Err(failed("GetTokenInformation", last));
        }

        let user: *const TOKEN_USER = buffer.as_ptr().cast();
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: SET_ACCESS,
            // A file inherits nothing to anything, so the flag says so rather
            // than leaving a container-shaped entry on a leaf.
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: (*user).User.Sid.cast(),
            },
        };

        // `oldacl` is null rather than the file's current ACL: this is a
        // replacement, and merging the inherited entries back in is the one
        // thing that would make the whole call pointless.
        let mut acl: *mut ACL = std::ptr::null_mut();
        let built = SetEntriesInAclW(1, &access, std::ptr::null(), &mut acl);
        if built != ERROR_SUCCESS {
            return Err(failed("SetEntriesInAclW", std::io::Error::from_raw_os_error(built as i32)));
        }
        let set = SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        );
        LocalFree(acl.cast());
        if set != ERROR_SUCCESS {
            return Err(failed(
                "SetNamedSecurityInfoW",
                std::io::Error::from_raw_os_error(set as i32),
            ));
        }
    }
    Ok(())
}

/// Remove one top-level key.
///
/// A merge patch cannot express "unset" - that is the module doc's first
/// consequence - so the token migration, which has to take `hfToken` out of a
/// file that already has it, needs a way in that is not a patch. Nothing is
/// written when the key was not there.
pub fn forget(app: &tauri::AppHandle, key: &str) -> Result<(), String> {
    let _guard = SETTINGS_WRITE_LOCK.lock().map_err(|e| e.to_string())?;
    let path = path(app)?;
    let Value::Object(mut current) = read_file(&path)? else { return Ok(()) };
    if current.remove(key).is_none() {
        return Ok(());
    }
    write_file(&path, &Value::Object(current))
}

/// The snapshot the interface is allowed to see.
///
/// The seam says the Hugging Face token never
/// travels in this direction, and on a machine with no credential store this
/// file is holding one - so the key is taken out at the boundary rather than
/// relied on to be absent. The fallback is unaffected: `weights::token` reads
/// the file directly and never through this.
fn without_token(mut snapshot: Value) -> Value {
    if let Value::Object(map) = &mut snapshot {
        map.remove(crate::weights::TOKEN_SETTING);
    }
    snapshot
}

#[tauri::command]
pub fn read_settings(app: tauri::AppHandle) -> Result<Value, String> {
    read(&app).map(without_token)
}

#[tauri::command]
pub fn write_settings(app: tauri::AppHandle, patch: Value) -> Result<Value, String> {
    // The answer is a whole snapshot and travels the same direction a read
    // does, so it is filtered by the same rule.
    let turns_cloud = patch.get(crate::inference::CLOUD_ENGINES_KEY).is_some();
    let snapshot = write(&app, patch)?;
    // Turning cloud engines off withdraws every project's standing consent,
    // so turning them back on asks again in each project.
    if turns_cloud && !cloud_engines_allowed(&snapshot) {
        match crate::library::Library::for_app(&app).map(|library| library.clear_cloud_consents(None)) {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => eprintln!("cloud consents not cleared after turning cloud off: {err}"),
            Err(err) => eprintln!("cloud consents not cleared after turning cloud off: {err}"),
        }
    }
    Ok(without_token(snapshot))
}

/// Whether a settings snapshot has cloud engines on.
fn cloud_engines_allowed(snapshot: &Value) -> bool {
    snapshot.get(crate::inference::CLOUD_ENGINES_KEY).and_then(Value::as_str) == Some("allowed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn only_an_explicit_allowed_keeps_cloud_engines_on() {
        assert!(cloud_engines_allowed(&serde_json::json!({ "cloudEngines": "allowed" })));
        assert!(!cloud_engines_allowed(&serde_json::json!({ "cloudEngines": "off" })));
        assert!(!cloud_engines_allowed(&serde_json::json!({ "cloudEngines": null })));
        assert!(!cloud_engines_allowed(&serde_json::json!({})));
    }

    #[test]
    fn concurrent_patches_keep_both_fields() {
        let dir = std::env::temp_dir().join(format!(
            "mc-settings-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("settings.json");
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for field in ["theme", "accelerator"] {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                for n in 0..80 {
                    barrier.wait();
                    let patch = Value::Object(Map::from_iter([(field.to_owned(), Value::from(n))]));
                    write_patch(&path, patch).unwrap();
                    barrier.wait();
                }
            }));
        }
        let mut first_missing = None;
        for n in 0..80 {
            barrier.wait();
            barrier.wait();
            let snapshot = read_file(&path).unwrap();
            if (snapshot.get("theme"), snapshot.get("accelerator"))
                != (Some(&serde_json::json!(n)), Some(&serde_json::json!(n)))
                && first_missing.is_none()
            {
                first_missing = Some((n, snapshot));
            }
        }
        for worker in workers {
            worker.join().unwrap();
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(first_missing.is_none(), "concurrent writes lost a field: {first_missing:?}");
    }

    /// The seam's rule about the token, enforced at
    /// the boundary rather than assumed: on a machine with no credential store
    /// the file really is holding one, and `readSettings` still must not hand
    /// it back.
    #[test]
    fn a_snapshot_that_crosses_the_seam_carries_no_token() {
        let snapshot = serde_json::json!({
            "accelerator": "auto",
            "hfToken": "hf_secret",
            "theme": "dark",
        });
        let filtered = without_token(snapshot);

        assert!(filtered.get("hfToken").is_none(), "the token does not travel this way");
        assert_eq!(filtered.get("accelerator").and_then(|v| v.as_str()), Some("auto"));
        assert_eq!(filtered.get("theme").and_then(|v| v.as_str()), Some("dark"));
        assert!(!serde_json::to_string(&filtered).unwrap().contains("hf_secret"));
    }

    /// A settings file that is not an object is passed through rather than
    /// replaced: the filter has one job and repairing the file is `write`'s.
    #[test]
    fn a_snapshot_that_is_not_an_object_is_left_alone() {
        assert_eq!(without_token(Value::Null), Value::Null);
        assert_eq!(without_token(serde_json::json!([1, 2])), serde_json::json!([1, 2]));
    }

    /// **Only a string is a token, and only the empty string is a Clear.**
    ///
    /// The reading this replaces took `as_str().unwrap_or_default()`, which
    /// made every non-string an empty string and therefore a silent, successful
    /// deletion of the user's token. A patch with `hfToken: null` in it - a
    /// caller that meant to send nothing and sent something - must be refused,
    /// not obeyed.
    #[test]
    fn only_a_string_is_a_token_and_a_non_string_is_refused() {
        assert_eq!(token_of(&serde_json::json!("hf_abc")), Ok("hf_abc"));
        assert_eq!(token_of(&serde_json::json!("  hf_abc  ")), Ok("hf_abc"), "trimmed");
        assert_eq!(token_of(&serde_json::json!("")), Ok(""), "the empty string is the Clear");

        for wrong in [
            serde_json::json!(null),
            serde_json::json!(0),
            serde_json::json!(false),
            serde_json::json!({}),
            serde_json::json!([]),
        ] {
            let refused = token_of(&wrong).expect_err(&format!("{wrong} is not a token"));
            assert!(refused.contains("hfToken"), "{refused}");
        }
    }

    /// The fallback copy of the token is written into a file the rest of the
    /// machine can read, unless something narrows it. On a shared Linux box
    /// `write_atomic` creates 0644 through the umask, so the mode is set after
    /// the rename rather than assumed.
    #[cfg(unix)]
    #[test]
    fn the_settings_file_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("mc-settings-mode-{}", std::process::id()));
        let path = dir.join("nested").join("settings.json");
        write_file(&path, &serde_json::json!({ "hfToken": "hf_secret" })).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a file holding a token is the owner's alone");

        // And a rewrite does not widen it again.
        write_file(&path, &serde_json::json!({ "theme": "dark" })).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn analysis_targets_are_validated_before_they_are_stored() {
        let dir = std::env::temp_dir().join(format!("mc-settings-targets-{}", std::process::id()));
        let path = dir.join("settings.json");
        let stored = write_patch(&path, serde_json::json!({"analysisTargets": {"rtFull": "cloud", "samTs": "cloud"}}))
            .unwrap();
        assert_eq!(stored["analysisTargets"]["rtFull"], "cloud");
        for bad in [serde_json::json!({"analysisTargets": {"samTs": "remote"}}),
            serde_json::json!({"analysisTargets": {"ctd": "cloud"}}),
            serde_json::json!({"analysisTargets": ["cloud"]}),
            // Detection runs in one place: a split is refused, not stored.
            serde_json::json!({"analysisTargets": {"rtFull": "local", "samTs": "cloud"}})] {
            assert!(write_patch(&path, bad).is_err());
        }
        assert_eq!(read_file(&path).unwrap()["analysisTargets"]["rtFull"], "cloud", "a refused patch writes nothing");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clean_target_is_strict_and_defaults_local() {
        use crate::inference::cloud_clean::CleanTarget;
        assert_eq!(CleanTarget::from_value(None).unwrap(), CleanTarget::Local);
        assert_eq!(CleanTarget::from_value(Some(&Value::Null)).unwrap(), CleanTarget::Local);
        assert_eq!(CleanTarget::from_value(Some(&serde_json::json!("local"))).unwrap(), CleanTarget::Local);
        assert_eq!(CleanTarget::from_value(Some(&serde_json::json!("cloud"))).unwrap(), CleanTarget::Cloud);
        for bad in [serde_json::json!("Cloud"), serde_json::json!("gpu"), serde_json::json!(true),
            serde_json::json!({"target": "cloud"})] {
            assert!(CleanTarget::from_value(Some(&bad)).is_err(), "{bad}");
        }

        let dir = std::env::temp_dir().join(format!("mc-settings-clean-target-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(write_patch(&path, serde_json::json!({"cleanTarget": "cloud"})).unwrap()["cleanTarget"], "cloud");
        assert!(write_patch(&path, serde_json::json!({"cleanTarget": "remote"})).is_err());
        assert_eq!(read_file(&path).unwrap()["cleanTarget"], "cloud", "a refused patch writes nothing");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn denoise_target_is_validated_on_write() {
        let dir = std::env::temp_dir().join(format!("mc-settings-denoise-target-{}", std::process::id()));
        let path = dir.join("settings.json");
        for value in ["local", "cloud", "off"] {
            assert_eq!(write_patch(&path, serde_json::json!({"denoiseTarget": value})).unwrap()["denoiseTarget"], value);
        }
        for bad in [serde_json::json!("remote"), serde_json::json!("Cloud"), serde_json::json!(1)] {
            assert!(write_patch(&path, serde_json::json!({"denoiseTarget": bad})).is_err());
        }
        assert_eq!(read_file(&path).unwrap()["denoiseTarget"], "off", "a refused patch writes nothing");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn denoise_preset_and_local_speed_are_validated_on_write() {
        let dir = std::env::temp_dir().join(format!("mc-settings-denoise-preset-{}", std::process::id()));
        let path = dir.join("settings.json");
        for preset in cleaner_core::cloud_denoise_wire::PRESETS {
            assert_eq!(write_patch(&path, serde_json::json!({"denoisePreset": preset.id})).unwrap()["denoisePreset"], preset.id);
        }
        for bad in [serde_json::json!("custom"), serde_json::json!("Mangajanai-2x"), serde_json::json!(2)] {
            assert!(write_patch(&path, serde_json::json!({"denoisePreset": bad})).is_err(), "{bad}");
        }
        assert_eq!(read_file(&path).unwrap()["denoisePreset"], "mangajanai-4x", "a refused patch writes nothing");
        assert!(write_patch(&path, serde_json::json!({"denoisePreset": null})).is_ok());

        assert_eq!(write_patch(&path, serde_json::json!({"denoiseLocalSecondsPerPage": 32.5})).unwrap()
            ["denoiseLocalSecondsPerPage"], 32.5);
        for bad in [serde_json::json!(0), serde_json::json!(-1.0), serde_json::json!("30"), serde_json::json!(true)] {
            assert!(write_patch(&path, serde_json::json!({"denoiseLocalSecondsPerPage": bad})).is_err(), "{bad}");
        }
        assert!(write_patch(&path, serde_json::json!({"denoiseLocalSecondsPerPage": null})).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cloud_secret_keys_are_rejected_recursively() {
        // Top-level cloud tokens rejected
        assert!(check_no_cloud_secrets("", &serde_json::json!({ "beamToken": "secret-1" })).is_err());
        assert!(check_no_cloud_secrets("", &serde_json::json!({ "modalToken": "secret-2" })).is_err());
        assert!(check_no_cloud_secrets("", &serde_json::json!({ "apiKey": "secret-3" })).is_err());
        assert!(check_no_cloud_secrets("", &serde_json::json!({ "cloudSecret": "secret-4" })).is_err());

        // Nested cloud tokens rejected
        assert!(check_no_cloud_secrets("", &serde_json::json!({
            "cloud": {
                "apiKey": "nested-secret"
            }
        })).is_err());

        assert!(check_no_cloud_secrets("", &serde_json::json!({
            "inference": {
                "runtimeToken": "nested-token"
            }
        })).is_err());

        assert!(check_no_cloud_secrets("", &serde_json::json!({
            "nested": {
                "deep": {
                    "modal_token": "deep-secret"
                }
            }
        })).is_err());

        // Array-nested secrets rejected
        assert!(check_no_cloud_secrets("", &serde_json::json!({
            "items": [
                { "cloudToken": "array-secret" }
            ]
        })).is_err());

        // Only the exact top-level legacy key is intercepted by weights::store_token.
        assert!(check_no_cloud_secrets("", &serde_json::json!({"nested": {"hfToken": "secret"}})).is_err());
        assert!(check_no_cloud_secrets("", &serde_json::json!({"HFTOKEN": "secret"})).is_err());

        // Valid settings accepted
        assert!(check_no_cloud_secrets("", &serde_json::json!({
            "theme": "dark",
            "accelerator": "auto",
            "runtimeFlavour": "cuda12",
            "cloud": {
                "allowed": true
            },
            "hfToken": "hf_legitimate_token"
        })).is_ok());
    }
}
