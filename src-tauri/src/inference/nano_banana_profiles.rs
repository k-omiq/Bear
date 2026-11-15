//! Nano Banana profiles: more than one Gemini key, each with its own name and
//! model, and one of them active.
//!
//! The list is kept in `settings.json` under [`PROFILES_KEY`], as
//! `{active, list: [{id, name, model}]}`. It holds no secret: each profile's
//! key is its own item in the OS credential store, under an account named by
//! the profile's id ([`account`]). Only the profile commands here write the
//! list; a settings patch from the webview that names it is refused
//! (`settings.rs#write_settings`).
//!
//! **The single key of before is the first profile.** A settings file with no
//! list reads as one profile, [`LEGACY_ID`], whose key is the item the single
//! key was always kept in. Nothing is copied and nothing is read: the item
//! stays where it is, so no key is lost and no keychain prompt is added. The
//! list is written the first time Settings lists the profiles and that item
//! is there, and until then a render reads it as that one profile.
//!
//! **Keys are read lazily.** Listing asks the store whether each item is
//! there, which never prompts ([`super::secrets::account_exists`]). Only a
//! render reads a key, and only the key of the profile its window was
//! granted on.

use std::sync::{Mutex, PoisonError};

use cleaner_core::engines::nano_banana::Model;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::nano_banana::{self, KeyError};

pub const PROFILES_KEY: &str = "nanoBananaProfiles";
/// The id of the profile the single key of before became.
pub const LEGACY_ID: &str = "default";
/// The name it gets. A product name, so it is the same in every language,
/// and the person can rename it.
const LEGACY_NAME: &str = "Nano Banana";
/// More than anyone keeps; a bound so the list stays a list.
pub const MAX_PROFILES: usize = 20;
/// Counted in characters, as the name field's `maxlength` counts them, so
/// a name the field takes is a name this takes (a CJK name is 3 bytes each).
const MAX_NAME_CHARS: usize = 128;

/// Why a profile command failed, as the stable code the webview looks up.
pub(crate) const BAD_NAME: &str = "nanoBanana.profile.badName";
pub(crate) const BAD_MODEL: &str = "nanoBanana.profile.badModel";
pub(crate) const MISSING: &str = "nanoBanana.profile.missing";
pub(crate) const TOO_MANY: &str = "nanoBanana.profile.tooMany";
pub(crate) const NOT_SAVED: &str = "nanoBanana.profile.notSaved";

/// One profile as the list keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// A model id. An unknown one reads as the default model.
    pub model: String,
}

/// The list as settings hold it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profiles {
    pub active: Option<String>,
    pub list: Vec<Profile>,
    /// No list is stored yet: this is the single key of before, read as one
    /// profile ([`LEGACY_ID`]).
    pub implicit: bool,
}

/// The profile a window was granted on: what a render reads the key of, and
/// what its record names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Chosen {
    pub id: String,
    pub name: String,
}

/// The credential store account a profile's key lives in. The first profile
/// keeps the single key's own account, which is how it is migrated.
pub(crate) fn account(id: &str) -> String {
    if id == LEGACY_ID { nano_banana::KEYRING_ACCOUNT.to_owned() } else { format!("{}.{id}", nano_banana::KEYRING_ACCOUNT) }
}

/// An id this module made: it names a credential store account, so it is
/// short and plain.
fn valid_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A name as typed: no space at either end, 1 to 128 characters, no control
/// characters. The same rule the cloud endpoints' names follow.
fn valid_name(name: &str) -> Result<&str, &'static str> {
    let fits = !name.is_empty() && name == name.trim() && name.chars().count() <= MAX_NAME_CHARS && !name.chars().any(char::is_control);
    if fits { Ok(name) } else { Err(BAD_NAME) }
}

/// A model id this build sends to, or refused.
fn valid_model(model: &str) -> Result<&'static str, &'static str> {
    Model::ALL.iter().find(|known| known.id == model).map(|known| known.id).ok_or(BAD_MODEL)
}

fn new_id() -> Result<String, &'static str> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| NOT_SAVED)?;
    Ok(format!("p{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

impl Profiles {
    /// Lenient: a profile that does not read is left out, an unknown model is
    /// the default, and an active id that names no profile is the first one.
    pub fn from_settings(settings: Option<&Value>) -> Profiles {
        let Some(stored) = settings.and_then(|value| value.get(PROFILES_KEY)).filter(|value| !value.is_null()) else {
            let model = settings.and_then(|value| value.get(nano_banana::MODEL_KEY)).and_then(Value::as_str);
            return Profiles {
                active: Some(LEGACY_ID.to_owned()),
                list: vec![Profile { id: LEGACY_ID.to_owned(), name: LEGACY_NAME.to_owned(), model: Model::from_setting(model).id.to_owned() }],
                implicit: true,
            };
        };
        let mut list: Vec<Profile> = Vec::new();
        for entry in stored.get("list").and_then(Value::as_array).into_iter().flatten() {
            let Ok(mut profile) = serde_json::from_value::<Profile>(entry.clone()) else { continue };
            if !valid_id(&profile.id) || valid_name(&profile.name).is_err() || list.iter().any(|kept| kept.id == profile.id) {
                continue;
            }
            profile.model = Model::from_setting(Some(&profile.model)).id.to_owned();
            list.push(profile);
            if list.len() == MAX_PROFILES {
                break;
            }
        }
        let named = stored.get("active").and_then(Value::as_str).filter(|id| list.iter().any(|p| p.id == *id));
        let active = named.map(str::to_owned).or_else(|| list.first().map(|p| p.id.clone()));
        Profiles { active, list, implicit: false }
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        let id = self.active.as_deref()?;
        self.list.iter().find(|profile| profile.id == id)
    }

    fn find(&self, id: &str) -> Result<usize, &'static str> {
        self.list.iter().position(|profile| profile.id == id).ok_or(MISSING)
    }

    /// The patch that stores this list.
    fn patch(&self) -> Value {
        json!({ PROFILES_KEY: { "active": self.active, "list": self.list } })
    }
}

/// The profile a render on these settings pays with, if any.
pub(crate) fn chosen(settings: Option<&Value>) -> Option<(Chosen, Model)> {
    let profiles = Profiles::from_settings(settings);
    let profile = profiles.active_profile()?;
    Some((Chosen { id: profile.id.clone(), name: profile.name.clone() }, Model::from_setting(Some(&profile.model))))
}

/// One profile as Settings shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    pub id: String,
    pub name: String,
    pub model: String,
    /// Whether its key is saved; null when the store could not say.
    pub has_key: Option<bool>,
    /// The key is held for this run only: the system has no credential store.
    pub session_only: bool,
}

/// What Settings lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesView {
    pub active: Option<String>,
    pub profiles: Vec<ProfileView>,
}

/// Where the list is read from and written to. A trait so the tests run
/// without an app or a credential store.
pub(crate) trait Home {
    fn read(&self) -> Result<Value, String>;
    fn write(&self, patch: Value) -> Result<(), String>;
    /// Whether the key of `account` is saved, asked without reading it.
    fn has_key(&self, account: &str) -> Result<bool, KeyError>;
    fn session_only(&self, account: &str) -> bool;
    fn store_key(&self, account: &str, key: &str) -> Result<(), KeyError>;
    fn delete_key(&self, account: &str) -> Result<(), KeyError>;
}

/// One profile command at a time, so two never write over each other's list.
static LOCK: Mutex<()> = Mutex::new(());

fn view(home: &dyn Home, profiles: &Profiles) -> ProfilesView {
    let profiles_view = profiles.list.iter().map(|profile| {
        let account = account(&profile.id);
        let session_only = home.session_only(&account);
        let has_key = match home.has_key(&account) {
            Ok(found) => Some(found),
            Err(KeyError::NoStore) => Some(session_only),
            Err(_) => None,
        };
        ProfileView { id: profile.id.clone(), name: profile.name.clone(), model: profile.model.clone(), has_key, session_only }
    }).collect();
    ProfilesView { active: profiles.active.clone(), profiles: profiles_view }
}

/// The list as stored, with the single key of before made the first profile
/// when its item is there, and none when it is not.
fn current(home: &dyn Home) -> Result<(Profiles, bool), String> {
    let settings = home.read()?;
    let mut profiles = Profiles::from_settings(Some(&settings));
    if !profiles.implicit {
        return Ok((profiles, false));
    }
    profiles.implicit = false;
    let account = account(LEGACY_ID);
    let held = if home.session_only(&account) {
        Some(true)
    } else {
        match home.has_key(&account) {
            Ok(found) => Some(found),
            Err(KeyError::NoStore) => Some(false),
            Err(_) => None,
        }
    };
    match held {
        // The first time it is seen: written, so it is a profile like any other.
        Some(true) => Ok((profiles, true)),
        Some(false) => Ok((Profiles::default(), false)),
        // The store could not be asked just now. The old key may be there, so
        // its profile stays in the list, unwritten: a key added meanwhile is
        // then saved beside it, and the old item is never left with no
        // profile to name it.
        None => Ok((profiles, false)),
    }
}

fn save(home: &dyn Home, profiles: &Profiles) -> Result<(), &'static str> {
    home.write(profiles.patch()).map_err(|error| {
        eprintln!("manga-cleaner: the Nano Banana profiles could not be saved: {error}");
        NOT_SAVED
    })
}

fn locked() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn list(home: &dyn Home) -> Result<ProfilesView, String> {
    let _guard = locked();
    let (profiles, migrated) = current(home)?;
    if migrated {
        // A failed write leaves it read as before, so nothing is lost.
        let _ = save(home, &profiles);
    }
    Ok(view(home, &profiles))
}

pub(crate) fn create(home: &dyn Home, name: &str, model: &str, key: &str) -> Result<ProfilesView, String> {
    let name = valid_name(name.trim())?;
    let model = valid_model(model)?;
    let _guard = locked();
    let (mut profiles, _) = current(home)?;
    if profiles.list.len() >= MAX_PROFILES {
        return Err(TOO_MANY.into());
    }
    let id = new_id()?;
    let account = account(&id);
    home.store_key(&account, key).map_err(|error| error.code().to_owned())?;
    profiles.list.push(Profile { id: id.clone(), name: name.to_owned(), model: model.to_owned() });
    if profiles.active.is_none() {
        profiles.active = Some(id);
    }
    if let Err(code) = save(home, &profiles) {
        // No profile names the key just stored: it goes too.
        let _ = home.delete_key(&account);
        return Err(code.into());
    }
    Ok(view(home, &profiles))
}

pub(crate) fn update(home: &dyn Home, id: &str, name: Option<&str>, model: Option<&str>) -> Result<ProfilesView, String> {
    let name = name.map(|name| valid_name(name.trim())).transpose()?;
    let model = model.map(valid_model).transpose()?;
    let _guard = locked();
    let (mut profiles, _) = current(home)?;
    let at = profiles.find(id)?;
    if let Some(name) = name {
        profiles.list[at].name = name.to_owned();
    }
    if let Some(model) = model {
        profiles.list[at].model = model.to_owned();
    }
    save(home, &profiles)?;
    Ok(view(home, &profiles))
}

pub(crate) fn select(home: &dyn Home, id: &str) -> Result<ProfilesView, String> {
    let _guard = locked();
    let (mut profiles, _) = current(home)?;
    profiles.find(id)?;
    profiles.active = Some(id.to_owned());
    save(home, &profiles)?;
    Ok(view(home, &profiles))
}

pub(crate) fn store_key(home: &dyn Home, id: &str, key: &str) -> Result<ProfilesView, String> {
    let _guard = locked();
    let (profiles, migrated) = current(home)?;
    profiles.find(id)?;
    home.store_key(&account(id), key).map_err(|error| error.code().to_owned())?;
    if migrated {
        let _ = save(home, &profiles);
    }
    Ok(view(home, &profiles))
}

/// Delete the profile and its key. The key goes first: a key the store would
/// not delete keeps its profile, so it is never left behind unnamed.
pub(crate) fn delete(home: &dyn Home, id: &str) -> Result<ProfilesView, String> {
    let _guard = locked();
    let (mut profiles, _) = current(home)?;
    let at = profiles.find(id)?;
    home.delete_key(&account(id)).map_err(|error| error.code().to_owned())?;
    profiles.list.remove(at);
    if profiles.active.as_deref() == Some(id) {
        profiles.active = profiles.list.first().map(|profile| profile.id.clone());
    }
    save(home, &profiles)?;
    Ok(view(home, &profiles))
}

/// The app's settings file and the OS credential store.
struct AppHome(tauri::AppHandle);

impl Home for AppHome {
    fn read(&self) -> Result<Value, String> {
        crate::settings::read(&self.0)
    }
    fn write(&self, patch: Value) -> Result<(), String> {
        crate::settings::write(&self.0, patch).map(|_| ())
    }
    fn has_key(&self, account: &str) -> Result<bool, KeyError> {
        nano_banana::has_key_in(account)
    }
    fn session_only(&self, account: &str) -> bool {
        nano_banana::session_only_in(account)
    }
    fn store_key(&self, account: &str, key: &str) -> Result<(), KeyError> {
        nano_banana::store_key_in(account, key).map(|_| ())
    }
    fn delete_key(&self, account: &str) -> Result<(), KeyError> {
        nano_banana::delete_key_in(account)
    }
}

/// Off the invoke handler: a store call can wait minutes on a prompt or on D-Bus.
async fn off_thread<T: Send + 'static>(job: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(job).await.unwrap_or_else(|_| Err(NOT_SAVED.into()))
}

#[tauri::command]
pub async fn nano_banana_profiles(app: tauri::AppHandle) -> Result<ProfilesView, String> {
    off_thread(move || list(&AppHome(app))).await
}

/// Make a profile with its key. Write-only: the key never comes back.
#[tauri::command]
pub async fn create_nano_banana_profile(app: tauri::AppHandle, name: String, model: String, key: String)
    -> Result<ProfilesView, String> {
    let key = zeroize::Zeroizing::new(key);
    off_thread(move || create(&AppHome(app), &name, &model, &key)).await
}

#[tauri::command]
pub async fn update_nano_banana_profile(app: tauri::AppHandle, profile_id: String, name: Option<String>, model: Option<String>)
    -> Result<ProfilesView, String> {
    off_thread(move || update(&AppHome(app), &profile_id, name.as_deref(), model.as_deref())).await
}

#[tauri::command]
pub async fn select_nano_banana_profile(app: tauri::AppHandle, profile_id: String) -> Result<ProfilesView, String> {
    off_thread(move || select(&AppHome(app), &profile_id)).await
}

/// Replace one profile's key. Write-only, as [`create_nano_banana_profile`].
#[tauri::command]
pub async fn store_nano_banana_key(app: tauri::AppHandle, profile_id: String, key: String) -> Result<ProfilesView, String> {
    let key = zeroize::Zeroizing::new(key);
    off_thread(move || store_key(&AppHome(app), &profile_id, &key)).await
}

#[tauri::command]
pub async fn delete_nano_banana_profile(app: tauri::AppHandle, profile_id: String) -> Result<ProfilesView, String> {
    off_thread(move || delete(&AppHome(app), &profile_id)).await
}

/// Whether the active profile's key is saved, asked without reading it, so
/// opening the editor never prompts for the keychain.
#[tauri::command]
pub async fn nano_banana_has_key(app: tauri::AppHandle) -> Result<bool, String> {
    off_thread(move || {
        let settings = crate::settings::read(&app).ok();
        let Some((profile, _)) = chosen(settings.as_ref()) else { return Ok(false) };
        nano_banana::has_key_in(&account(&profile.id)).map_err(|error| error.code().to_owned())
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Settings in memory and a credential store of accounts.
    #[derive(Default)]
    struct FakeHome {
        settings: Mutex<Value>,
        keys: Mutex<BTreeMap<String, String>>,
        reads: Mutex<Vec<String>>,
        failing_write: AtomicBool,
        failing_delete: AtomicBool,
        failing_has: AtomicBool,
    }

    impl FakeHome {
        fn with(settings: Value, keys: &[(&str, &str)]) -> FakeHome {
            let home = FakeHome { settings: Mutex::new(settings), ..FakeHome::default() };
            for (account, key) in keys {
                home.keys.lock().unwrap().insert((*account).to_owned(), (*key).to_owned());
            }
            home
        }
        fn stored(&self) -> Value {
            self.settings.lock().unwrap().get(PROFILES_KEY).cloned().unwrap_or(Value::Null)
        }
    }

    impl Home for FakeHome {
        fn read(&self) -> Result<Value, String> {
            Ok(self.settings.lock().unwrap().clone())
        }
        fn write(&self, patch: Value) -> Result<(), String> {
            if self.failing_write.load(Ordering::SeqCst) {
                return Err("disk full".into());
            }
            let mut settings = self.settings.lock().unwrap();
            for (key, value) in patch.as_object().unwrap() {
                settings[key] = value.clone();
            }
            Ok(())
        }
        fn has_key(&self, account: &str) -> Result<bool, KeyError> {
            if self.failing_has.load(Ordering::SeqCst) {
                return Err(KeyError::Failed);
            }
            Ok(self.keys.lock().unwrap().contains_key(account))
        }
        fn session_only(&self, _: &str) -> bool {
            false
        }
        fn store_key(&self, account: &str, key: &str) -> Result<(), KeyError> {
            self.reads.lock().unwrap().push(format!("set {account}"));
            self.keys.lock().unwrap().insert(account.to_owned(), key.to_owned());
            Ok(())
        }
        fn delete_key(&self, account: &str) -> Result<(), KeyError> {
            if self.failing_delete.load(Ordering::SeqCst) {
                return Err(KeyError::Failed);
            }
            self.keys.lock().unwrap().remove(account);
            Ok(())
        }
    }

    const KEY_A: &str = "AIzaSyA-0123456789abcdefghijklmnopqrstu";
    const KEY_B: &str = "AIzaSyB-0123456789abcdefghijklmnopqrstu";
    const PRO: &str = "gemini-3-pro-image";

    /// **The single key of before becomes the first profile.** It keeps its
    /// own credential store item, with no read, and the old model setting.
    #[test]
    fn profiles_migrate_the_single_key_as_the_first_profile() {
        let home = FakeHome::with(json!({ nano_banana::MODEL_KEY: PRO }), &[(nano_banana::KEYRING_ACCOUNT, KEY_A)]);
        let listed = list(&home).unwrap();
        assert_eq!(listed.active.as_deref(), Some(LEGACY_ID));
        assert_eq!(listed.profiles.len(), 1);
        assert_eq!(listed.profiles[0].model, PRO);
        assert_eq!(listed.profiles[0].has_key, Some(true));
        assert_eq!(account(LEGACY_ID), nano_banana::KEYRING_ACCOUNT, "the key stays in its own item");
        assert_eq!(home.stored()["list"][0]["id"], LEGACY_ID, "written once seen");
        assert_eq!(home.keys.lock().unwrap().get(nano_banana::KEYRING_ACCOUNT).map(String::as_str), Some(KEY_A));
        assert!(home.reads.lock().unwrap().is_empty(), "nothing was copied");
    }

    #[test]
    fn profiles_with_no_old_key_start_empty_and_are_not_written() {
        let home = FakeHome::with(json!({}), &[]);
        let listed = list(&home).unwrap();
        assert_eq!(listed.profiles, vec![]);
        assert_eq!(listed.active, None);
        assert_eq!(home.stored(), Value::Null);
    }

    /// A store that cannot be asked is not a store without the key: the old
    /// key's profile stays listed, and a profile made meanwhile is saved
    /// beside it, so the old item always has a profile to name it.
    #[test]
    fn profiles_a_store_error_keeps_the_old_key_listed() {
        let home = FakeHome::with(json!({ nano_banana::MODEL_KEY: PRO }), &[(nano_banana::KEYRING_ACCOUNT, KEY_A)]);
        home.failing_has.store(true, Ordering::SeqCst);
        let listed = list(&home).unwrap();
        assert_eq!(listed.profiles.len(), 1);
        assert_eq!(listed.profiles[0].id, LEGACY_ID);
        assert_eq!(listed.profiles[0].has_key, None, "unknown, not absent");
        assert_eq!(home.stored(), Value::Null, "not written while the store cannot be asked");
        let made = create(&home, "Work", PRO, KEY_B).unwrap();
        assert_eq!(made.profiles.len(), 2);
        assert_eq!(home.stored()["list"][0]["id"], LEGACY_ID, "the old key still has its profile");
    }

    #[test]
    fn profiles_a_name_is_counted_in_characters() {
        let cjk = "あ".repeat(128);
        assert_eq!(valid_name(&cjk), Ok(cjk.as_str()));
        assert_eq!(valid_name(&"あ".repeat(129)), Err(BAD_NAME));
    }

    /// Before Settings has listed them, a render still reads the old key as
    /// the one profile.
    #[test]
    fn profiles_an_unmigrated_file_still_names_the_old_key() {
        let (chosen, model) = chosen(Some(&json!({ nano_banana::MODEL_KEY: PRO }))).unwrap();
        assert_eq!(chosen.id, LEGACY_ID);
        assert_eq!(model.id, PRO);
    }

    #[test]
    fn profiles_create_rename_select_and_delete() {
        let home = FakeHome::with(json!({}), &[]);
        let first = create(&home, "  Work  ", PRO, KEY_A).unwrap();
        let work = first.profiles[0].id.clone();
        assert_eq!(first.active.as_deref(), Some(work.as_str()), "the first profile is active");
        assert_eq!(first.profiles[0].name, "Work");
        let second = create(&home, "Home", "gemini-3.1-flash-image", KEY_B).unwrap();
        let personal = second.profiles[1].id.clone();
        assert_eq!(second.active.as_deref(), Some(work.as_str()), "a new one is not made active");
        assert_ne!(account(&work), account(&personal), "each key is its own item");
        assert_eq!(home.keys.lock().unwrap().get(&account(&personal)).map(String::as_str), Some(KEY_B));

        let renamed = update(&home, &personal, Some("Personal"), Some(PRO)).unwrap();
        assert_eq!((renamed.profiles[1].name.as_str(), renamed.profiles[1].model.as_str()), ("Personal", PRO));
        assert_eq!(select(&home, &personal).unwrap().active.as_deref(), Some(personal.as_str()));
        assert_eq!(chosen(Some(&home.read().unwrap())).unwrap().0.name, "Personal");

        let left = delete(&home, &personal).unwrap();
        assert_eq!(left.active.as_deref(), Some(work.as_str()), "the next one is active");
        assert!(!home.keys.lock().unwrap().contains_key(&account(&personal)), "its key went with it");
        assert_eq!(delete(&home, &work).unwrap().active, None);
        assert_eq!(chosen(Some(&home.read().unwrap())), None);
    }

    #[test]
    fn profiles_refuse_a_bad_name_or_model_and_an_unknown_id() {
        let home = FakeHome::with(json!({}), &[]);
        assert_eq!(create(&home, "", PRO, KEY_A).unwrap_err(), BAD_NAME);
        assert_eq!(create(&home, "a\nb", PRO, KEY_A).unwrap_err(), BAD_NAME);
        assert_eq!(create(&home, &"x".repeat(129), PRO, KEY_A).unwrap_err(), BAD_NAME);
        assert_eq!(create(&home, "Work", "../evil", KEY_A).unwrap_err(), BAD_MODEL);
        assert_eq!(select(&home, "nope").unwrap_err(), MISSING);
        assert_eq!(delete(&home, "nope").unwrap_err(), MISSING);
        assert!(home.keys.lock().unwrap().is_empty());
    }

    #[test]
    fn profiles_a_failed_save_takes_the_new_key_back_and_a_kept_key_keeps_its_profile() {
        let home = FakeHome::with(json!({}), &[]);
        home.failing_write.store(true, Ordering::SeqCst);
        assert_eq!(create(&home, "Work", PRO, KEY_A).unwrap_err(), NOT_SAVED);
        assert!(home.keys.lock().unwrap().is_empty(), "no unnamed key is left");

        home.failing_write.store(false, Ordering::SeqCst);
        let id = create(&home, "Work", PRO, KEY_A).unwrap().profiles[0].id.clone();
        home.failing_delete.store(true, Ordering::SeqCst);
        assert_eq!(delete(&home, &id).unwrap_err(), KeyError::Failed.code());
        assert_eq!(list(&home).unwrap().profiles.len(), 1);
    }

    #[test]
    fn profiles_read_leniently_and_never_name_another_account() {
        let settings = json!({ PROFILES_KEY: { "active": "gone", "list": [
            { "id": "../x", "name": "Bad id", "model": PRO },
            { "id": "pa", "name": "A", "model": "unknown" },
            { "id": "pa", "name": "Twice", "model": PRO },
            "not a profile",
        ]}});
        let profiles = Profiles::from_settings(Some(&settings));
        assert_eq!(profiles.list.len(), 1);
        assert_eq!(profiles.list[0].model, Model::DEFAULT.id);
        assert_eq!(profiles.active.as_deref(), Some("pa"), "an active id that names nothing is the first");
    }
}
