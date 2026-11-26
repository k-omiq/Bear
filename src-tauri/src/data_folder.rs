//! The data folder shared with the Pear typesetter.
//!
//! The contract both apps keep is `docs/shared-data-folder.md`; this module is
//! the cleaner's half of sections 3 to 7. In short:
//!
//! - A pointer in the app state directory (which never moves) names the
//!   folder. No pointer is legacy mode: the library stays at
//!   `<app data>/library`, as before.
//! - The folder holds `Cleaner/` (this app's library), `Typesetter/`,
//!   `handoff/` and `locks/`, and a marker whose `folderId` must match the
//!   pointer's.
//! - Only setup and move create the folder. A pointer whose folder is gone (an
//!   unplugged drive) is Unavailable, and [`library_root`] refuses, so nothing
//!   downstream can create an empty library in its place.
//! - Every setup and move is journaled beside the pointer and recovered at
//!   the next start, so a crash leaves the data whole in one of the two places.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use serde::Serialize;

/// Bundle identifiers of the released apps. Any other build (the
/// side-by-side Next build, a test build) gets the `-dev` state directory so
/// it never touches the real folder.
const RELEASE_IDS: [&str; 2] = ["com.mangacleaner.studio", "com.mangatypesetter.app"];
const STATE_DIR: &str = "MangaLibrary";
const STATE_DIR_DEV: &str = "MangaLibrary-dev";
const POINTER_FILE: &str = "data-folder.json";
const MOVE_LOCK: &str = "move.lock";
const JOURNAL: &str = "move-journal.json";
/// Old trees a cross-volume move left behind, kept until the user deletes them.
const OLD_COPIES: &str = "old-copies.json";
/// A full file path that replaces the pointer location (and so the state
/// directory: the move lock and journal sit beside it). Tests use it.
pub const POINTER_ENV: &str = "MANGA_LIBRARY_POINTER";

pub const MARKER: &str = "manga-library.json";
pub const CLEANER_DIR: &str = "Cleaner";
pub const HANDOFF_DIR: &str = "handoff";
pub const TO_TYPESETTER: &str = "to-typesetter";
const LOCKS_DIR: &str = "locks";
const OWN_LOCK: &str = "cleaner.lock";
const TYPESETTER_LOCK: &str = "typesetter.lock";
/// Under the user's home folder, for a first setup. Not Documents: see the
/// contract, section 7.
pub const DEFAULT_NAME: &str = "Manga Library";
/// The legacy library, under the app data directory.
const LEGACY_DIR: &str = "library";
/// The library's own id, a file old builds never touch (they drop unknown
/// fields when they save `index.json`).
pub const LIBRARY_ID_FILE: &str = "library-id.json";

/* ------------------------------------------------------------------ */
/* Ids and small file helpers                                          */
/* ------------------------------------------------------------------ */

/// A random (version 4) UUID.
pub fn new_uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("the OS random source");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Temp file beside it, fsync, rename, fsync the folder: a reader sees the
/// old file or the new one and never half of one, and a power cut after the
/// call keeps the new one.
pub(crate) fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut file = File::create(&tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

/// Flushes a file just copied. Windows flushes only through a handle that can
/// write, so a read-only open is refused there ("Access is denied"); a file
/// that is itself read-only gives no such handle and is left to the system.
fn flush_copied(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        match std::fs::OpenOptions::new().write(true).open(path) {
            Ok(f) => f.sync_all(),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(windows))]
    {
        File::open(path)?.sync_all()
    }
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}

/// Windows has no directory handle to flush; NTFS journals the rename.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| format!("{} has characters this app cannot store. Pick a folder with a plainer name.", shown(path)))
}

fn has_content(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

/// A path as the user is shown it: without the Windows `\\?\` (or
/// `\\?\UNC\`) prefix a canonical path carries (contract section 11).
pub fn shown(path: &Path) -> String {
    shown_text(&path.display().to_string())
}

/// [`shown`] for a path already turned into text.
pub fn shown_text(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text.to_owned()
    }
}

/// The plain words for an OS error met while setting up or moving the data
/// folder (contract section 11). The raw text goes to the log only.
pub fn plain_io(path: &Path, error: &std::io::Error) -> String {
    eprintln!("manga-cleaner: data folder: {}: {error}", path.display());
    plain_words(error).to_owned()
}

fn plain_words(error: &std::io::Error) -> &'static str {
    plain_words_or(error, "The move could not finish.")
}

/// [`plain_words`], with `other` for an error none of the common ones is.
pub(crate) fn plain_words_or(error: &std::io::Error, other: &'static str) -> &'static str {
    use std::io::ErrorKind;
    const NO_SPACE: &str = "There is not enough free space on that drive.";
    const NOT_ALLOWED: &str = "This app is not allowed to write there.";
    const GONE: &str = "The drive was disconnected.";
    const IN_USE: &str = "A file is in use by another app.";

    match error.kind() {
        ErrorKind::StorageFull | ErrorKind::QuotaExceeded => return NO_SPACE,
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => return NOT_ALLOWED,
        ErrorKind::ResourceBusy | ErrorKind::ExecutableFileBusy => return IN_USE,
        _ => {}
    }
    match error.raw_os_error() {
        // ENOSPC, EDQUOT (macOS, Linux).
        Some(28 | 69 | 122) if cfg!(unix) => NO_SPACE,
        // EROFS, EACCES, EPERM.
        Some(30 | 13 | 1) if cfg!(unix) => NOT_ALLOWED,
        // ENXIO, EIO, ENODEV: what a write to an unplugged drive meets.
        Some(6 | 5 | 19) if cfg!(unix) => GONE,
        // EBUSY, ETXTBSY.
        Some(16 | 26) if cfg!(unix) => IN_USE,
        // ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL.
        Some(112 | 39) if cfg!(windows) => NO_SPACE,
        // ERROR_ACCESS_DENIED, ERROR_WRITE_PROTECT.
        Some(5 | 19) if cfg!(windows) => NOT_ALLOWED,
        // ERROR_NOT_READY, ERROR_DEV_NOT_EXIST, ERROR_DEVICE_NOT_CONNECTED.
        Some(21 | 55 | 1167) if cfg!(windows) => GONE,
        // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
        Some(32 | 33) if cfg!(windows) => IN_USE,
        _ => other,
    }
}

/// An error that is not an OS error and not one of this module's own plain
/// sentences (a manifest that would not save, say): logged, and said as "the
/// move could not finish".
fn plain_other(context: &str, raw: impl std::fmt::Display) -> String {
    eprintln!("manga-cleaner: data folder: {context}: {raw}");
    "The move could not finish.".into()
}

/* ------------------------------------------------------------------ */
/* Is the sister app present? (contract section 4.1)                   */
/* ------------------------------------------------------------------ */

/// The sister app's release identifier. A dev build checks the release one
/// too: it shares the dev pointer, but a real Pear still counts.
pub const SISTER_ID: &str = "com.mangatypesetter.app";
/// `1` or `0` overrides the sister test, for tests.
pub const SISTER_ENV: &str = "MANGA_SISTER_PRESENT";

/// Every place whose existence says Pear has run on this computer: the
/// pointer, Pear's app data folder in each of the system's app data
/// locations, its legacy library, and (macOS) its WebKit data folder, which
/// is the reliable sign there because Pear's Rust side does not always make
/// an app data folder.
pub fn sister_paths(pointer: &Path, app_data_bases: &[PathBuf], home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![pointer.to_path_buf()];
    paths.extend(app_data_bases.iter().map(|base| base.join(SISTER_ID)));
    if let Some(home) = home {
        paths.push(home.join("Documents").join("MangaTypesetter"));
        if cfg!(target_os = "macos") {
            paths.push(home.join("Library").join("WebKit").join(SISTER_ID));
        }
    }
    paths
}

/// Whether any of `paths` is there. Reads only: nothing is created, and a
/// broken symlink still counts as there. `env` is the override's value.
pub fn sister_present_at(paths: &[PathBuf], env: Option<&str>) -> bool {
    match env.map(str::trim) {
        Some("1") => return true,
        Some("0") => return false,
        _ => {}
    }
    paths.iter().any(|path| std::fs::symlink_metadata(path).is_ok())
}

pub fn sister_present(app: &tauri::AppHandle) -> bool {
    use tauri::Manager;
    let env = std::env::var(SISTER_ENV).ok();
    if matches!(env.as_deref().map(str::trim), Some("1" | "0")) {
        return sister_present_at(&[], env.as_deref());
    }
    let paths = app.path();
    let bases: Vec<PathBuf> = [paths.data_dir(), paths.local_data_dir(), paths.config_dir()].into_iter().flatten().collect();
    let pointer = pointer_file(app).unwrap_or_default();
    sister_present_at(&sister_paths(&pointer, &bases, paths.home_dir().ok().as_deref()), env.as_deref())
}

/* ------------------------------------------------------------------ */
/* The pointer and the marker                                          */
/* ------------------------------------------------------------------ */

/// `<state dir>/MangaLibrary/data-folder.json`. The state dir is the one
/// that never roams: Application Support on macOS, `%LOCALAPPDATA%` on
/// Windows, the XDG config dir on Linux.
pub fn pointer_path(state_dir: &Path, identifier: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(POINTER_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    let dir = if RELEASE_IDS.contains(&identifier) { STATE_DIR } else { STATE_DIR_DEV };
    state_dir.join(dir).join(POINTER_FILE)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    pub path: PathBuf,
    pub folder_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointerRead {
    None,
    Good(Pointer),
    /// There, but not something this build may act on: a newer schema, a
    /// relative path, a file that does not parse. Never overwritten.
    Unknown(String),
}

pub fn read_pointer(path: &Path) -> PointerRead {
    let bytes = match std::fs::read(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return PointerRead::None,
        Err(error) => return PointerRead::Unknown(format!("{}: {error}", path.display())),
        Ok(bytes) => bytes,
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return PointerRead::Unknown(format!("{} cannot be read.", path.display()));
    };
    if value.get("schema").and_then(|v| v.as_u64()) != Some(1) {
        return PointerRead::Unknown("The data folder was set up by a newer version. Update this app.".into());
    }
    let (Some(root), Some(folder_id)) = (
        value.get("path").and_then(|v| v.as_str()).map(PathBuf::from),
        value.get("folderId").and_then(|v| v.as_str()),
    ) else {
        return PointerRead::Unknown(format!("{} is incomplete.", path.display()));
    };
    if !root.is_absolute() {
        return PointerRead::Unknown(format!("{} names a relative path.", path.display()));
    }
    PointerRead::Good(Pointer { path: root, folder_id: folder_id.to_owned() })
}

pub fn write_pointer(path: &Path, pointer: &Pointer) -> Result<(), String> {
    let dir = path.parent().ok_or("the pointer has no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| plain_io(dir, &e))?;
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": 1,
        "path": path_str(&pointer.path)?,
        "folderId": pointer.folder_id,
    }))
    .map_err(|e| e.to_string())?;
    write_atomic(path, &body).map_err(|e| plain_io(path, &e))
}

/// `(schema, folderId)` of a data folder's marker.
pub fn read_marker(root: &Path) -> Option<(u64, String)> {
    let value = read_json(&root.join(MARKER))?;
    Some((value.get("schema")?.as_u64()?, value.get("folderId")?.as_str()?.to_owned()))
}

pub fn is_data_folder(dir: &Path) -> bool {
    matches!(read_marker(dir), Some((1, _)))
}

/// Make `root` a data folder, or return the id it already has.
fn ensure_marker(root: &Path) -> Result<String, String> {
    if let Some((schema, id)) = read_marker(root) {
        return if schema == 1 { Ok(id) } else { Err("That folder was set up by a newer version.".into()) };
    }
    std::fs::create_dir_all(root).map_err(|e| plain_io(root, &e))?;
    let id = new_uuid();
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": 1,
        "folderId": id,
        "createdBy": "manga-cleaner",
        "createdAt": rfc3339_now(),
    }))
    .map_err(|e| e.to_string())?;
    write_atomic(&root.join(MARKER), &body).map_err(|e| plain_io(root, &e))?;
    Ok(id)
}

/// What this app's own subfolder of the data folder holds (contract 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Own {
    /// Not there, or an empty folder: "no subfolder yet".
    Absent,
    /// Has `index.json`, the cleaner's library file.
    Library,
    /// Has something, but not its library file.
    Damaged,
}

/// Lock files, Finder's `.DS_Store` and macOS `._` files say nothing about
/// whether a library is there.
fn incidental(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._") || name.ends_with(".lock")
}

pub fn own_state(own: &Path) -> Own {
    if own.join(crate::library::INDEX_FILE).is_file() {
        return Own::Library;
    }
    let Ok(entries) = std::fs::read_dir(own) else { return Own::Absent };
    let any = entries.flatten().any(|entry| !incidental(&entry.file_name().to_string_lossy()));
    if any { Own::Damaged } else { Own::Absent }
}

/// Absent, or a folder with nothing in it but what [`incidental`] ignores.
fn absent_or_empty(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Err(_) => !dir.exists(),
        Ok(entries) => entries.flatten().all(|entry| entry.file_name() == ".DS_Store"),
    }
}

/// How many titles the cleaner library at `root` holds. A library whose
/// index cannot be read counts as having some: it is not a first launch.
pub fn titles_in(root: &Path) -> usize {
    if !root.join(crate::library::INDEX_FILE).exists() {
        return 0;
    }
    crate::library::Library::at(root).index().map(|index| index.projects.len()).unwrap_or(1)
}

/// The id of the cleaner library at `root`, made on first ask. `None` where
/// it cannot be written (a read-only library), which only means a handoff
/// cannot be keyed yet.
pub fn library_id(root: &Path) -> Option<String> {
    let path = root.join(LIBRARY_ID_FILE);
    if let Some(id) = read_json(&path).and_then(|v| v.get("libraryId")?.as_str().map(str::to_owned)) {
        return Some(id);
    }
    if !root.is_dir() {
        return None;
    }
    // Written whole under a name of its own, then linked to the real name,
    // which fails if another writer got there first: two first Saves at once
    // must not mint two ids.
    let id = new_uuid();
    let body = serde_json::to_vec_pretty(&serde_json::json!({ "schema": 1, "libraryId": id })).ok()?;
    let tmp = root.join(format!(".{LIBRARY_ID_FILE}.{id}.tmp"));
    std::fs::write(&tmp, &body).ok()?;
    let linked = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Some(id),
        // No hard links here (exFAT, some shares): create-new is the next
        // claim that only one writer can win.
        Err(_) if !path.exists() => {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).ok()?;
            file.write_all(&body).ok()?;
            file.sync_all().ok()?;
            Some(id)
        }
        Err(_) => read_json(&path).and_then(|v| v.get("libraryId")?.as_str().map(str::to_owned)),
    }
}

/* ------------------------------------------------------------------ */
/* Modes                                                               */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// No pointer. The library is the old private one.
    Legacy,
    /// A good pointer, but this app's library is still in its old place.
    Pending,
    /// The library is `<root>/Cleaner`.
    Shared,
    /// A pointer whose folder cannot be used. No library at all.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub mode: Mode,
    pub pointer: Option<Pointer>,
    /// `None` only in Unavailable mode.
    pub library: Option<PathBuf>,
    pub legacy: PathBuf,
    /// Why, in Unavailable mode.
    pub reason: Option<String>,
}

impl Resolved {
    pub fn root(&self) -> Option<&Path> {
        self.pointer.as_ref().map(|p| p.path.as_path())
    }
}

/// Which library a pointer and a legacy folder add up to. Reads only.
pub fn resolve(pointer: PointerRead, legacy: &Path) -> Resolved {
    let legacy = legacy.to_path_buf();
    let unavailable = |pointer: Option<Pointer>, reason: String| Resolved {
        mode: Mode::Unavailable, pointer, library: None, legacy: legacy.clone(), reason: Some(reason),
    };
    let pointer = match pointer {
        PointerRead::None => {
            return Resolved { mode: Mode::Legacy, pointer: None, library: Some(legacy.clone()), legacy, reason: None };
        }
        PointerRead::Unknown(reason) => return unavailable(None, reason),
        PointerRead::Good(pointer) => pointer,
    };
    let root = pointer.path.clone();
    if !root.is_dir() {
        return unavailable(Some(pointer), format!("The data folder is not at {}. Reconnect the drive, or choose another folder.", shown(&root)));
    }
    match read_marker(&root) {
        None => return unavailable(Some(pointer), format!("{} is not a data folder any more (its {MARKER} is missing).", shown(&root))),
        Some((schema, _)) if schema != 1 => return unavailable(Some(pointer), "The data folder was set up by a newer version. Update this app.".into()),
        Some((_, id)) if id != pointer.folder_id => {
            return unavailable(Some(pointer), format!("{} is a different data folder than the one this computer set up.", shown(&root)));
        }
        Some(_) => {}
    }
    // Version 2 (contract 5.1): Shared only once `Cleaner/` holds its library
    // file. No subfolder, or an empty one, is Pending, whatever the legacy
    // place holds: the start flows (Join) decide, never this read.
    let own = root.join(CLEANER_DIR);
    match own_state(&own) {
        Own::Library => Resolved { mode: Mode::Shared, pointer: Some(pointer), library: Some(own), legacy, reason: None },
        Own::Damaged => unavailable(Some(pointer), "The Cleaner folder inside the data folder is damaged.".into()),
        Own::Absent => Resolved { mode: Mode::Pending, pointer: Some(pointer), library: Some(legacy.clone()), legacy, reason: None },
    }
}

static RESOLVED: RwLock<Option<Resolved>> = RwLock::new(None);
/// `locks/cleaner.lock`, held while this process runs with a shared folder.
static HELD: Mutex<Option<(PathBuf, File)>> = Mutex::new(None);

/// The directory that holds the pointer, the move lock and the journal.
pub fn state_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    #[cfg(windows)]
    let base = app.path().local_data_dir();
    #[cfg(not(windows))]
    let base = app.path().config_dir();
    let base = base.map_err(|e| e.to_string())?;
    let pointer = pointer_path(&base, &app.config().identifier);
    Ok(pointer.parent().map(Path::to_path_buf).unwrap_or(base))
}

fn pointer_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    #[cfg(windows)]
    let base = app.path().local_data_dir();
    #[cfg(not(windows))]
    let base = app.path().config_dir();
    Ok(pointer_path(&base.map_err(|e| e.to_string())?, &app.config().identifier))
}

fn legacy_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join(LEGACY_DIR))
}

fn current(app: &tauri::AppHandle) -> Result<Resolved, String> {
    if let Some(resolved) = RESOLVED.read().unwrap_or_else(|p| p.into_inner()).clone() {
        return Ok(resolved);
    }
    refresh(app)
}

/// Read the pointer again, settle the library, and take or let go of this
/// app's lock to match. Creates nothing: `Cleaner/` and its index are made
/// only by a setup or a Join (contract 5.1).
pub fn refresh(app: &tauri::AppHandle) -> Result<Resolved, String> {
    let blocked = settle_journal(&state_root(app)?);
    let mut resolved = resolve(read_pointer(&pointer_file(app)?), &legacy_dir(app)?);
    if let Some(reason) = blocked {
        // While a journal exists no library opens at either end of the move.
        resolved.mode = Mode::Unavailable;
        resolved.library = None;
        resolved.reason = Some(reason);
    }
    *RESOLVED.write().unwrap_or_else(|p| p.into_inner()) = Some(resolved.clone());
    hold_lock(if resolved.mode == Mode::Shared { resolved.root() } else { None });
    Ok(resolved)
}

/// A journal left by either app: finish it now if that can be done, else say
/// why not. Not while this process is the one moving.
fn settle_journal(state: &Path) -> Option<String> {
    if !journal_exists(state) {
        return None;
    }
    if MOVING.load(std::sync::atomic::Ordering::SeqCst) {
        return Some("data_folder_moving: The data folder is being moved.".into());
    }
    match recover(state) {
        Ok(said) => {
            if said.is_some() {
                *RECOVERED.lock().unwrap_or_else(|p| p.into_inner()) = said;
            }
            journal_exists(state).then(|| "A move of the data folder is not finished.".into())
        }
        Err(reason) => Some(reason),
    }
}

fn hold_lock(root: Option<&Path>) {
    let mut held = HELD.lock().unwrap_or_else(|p| p.into_inner());
    // Kept, not let go and taken again: in that gap the typesetter would see
    // this app as closed.
    if let (Some(root), Some((at, _))) = (root, held.as_ref()) {
        if at == root {
            return;
        }
    }
    *held = None;
    let Some(root) = root else { return };
    let dir = root.join(LOCKS_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    match cleaner_core::project::lock::try_exclusive(&dir.join(OWN_LOCK)) {
        Ok(Some(file)) => *held = Some((root.to_path_buf(), file)),
        Ok(None) => eprintln!("manga-cleaner: another Manga Cleaner holds {}", dir.join(OWN_LOCK).display()),
        Err(error) => eprintln!("manga-cleaner: {}: cannot lock: {error}", dir.join(OWN_LOCK).display()),
    }
}

fn release_lock() {
    *HELD.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Where the library is now. [`crate::library::Library::for_app`] asks this,
/// so an Unavailable folder refuses every library call instead of letting one
/// create an empty library where the drive used to be.
pub fn library_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if MOVING.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("data_folder_moving: The data folder is being moved. Wait until it is done.".into());
    }
    let mut resolved = current(app)?;
    // The folder can go while the app runs (a drive unplugged, a share
    // unmounted). One look at the marker per call catches it before anything
    // below creates a folder where the library used to be.
    // So can a journal appear: the typesetter starting a move on a drive that
    // cannot lock, after its user said this app was closed.
    let journal = state_root(app).is_ok_and(|state| journal_exists(&state));
    if journal || (resolved.mode == Mode::Shared && !resolved.root().is_some_and(|root| root.join(MARKER).is_file())) {
        resolved = refresh(app)?;
    }
    resolved.library.ok_or_else(|| format!("data_folder_unavailable: {}", resolved.reason.unwrap_or_default()))
}

/// The shared folder, when this app uses it.
pub fn shared_root(app: &tauri::AppHandle) -> Option<PathBuf> {
    current(app).ok().filter(|r| r.mode == Mode::Shared).and_then(|r| r.root().map(Path::to_path_buf))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Running {
    No,
    Yes,
    /// The file system has no locks.
    CannotTell,
}

/// Whether the typesetter runs with this folder: its lock is held.
pub fn typesetter_running(root: &Path) -> Running {
    let path = root.join(LOCKS_DIR).join(TYPESETTER_LOCK);
    if !path.exists() {
        return Running::No;
    }
    match cleaner_core::project::lock::try_exclusive(&path) {
        Ok(Some(_)) => Running::No,
        Ok(None) => Running::Yes,
        Err(_) => Running::CannotTell,
    }
}

/* ------------------------------------------------------------------ */
/* Moving a tree                                                       */
/* ------------------------------------------------------------------ */

/// The move lock beside the pointer, held by whichever app is setting up,
/// adopting or moving.
pub struct MoveLock(#[allow(dead_code)] File);

pub fn take_move_lock(state: &Path) -> Result<MoveLock, String> {
    std::fs::create_dir_all(state).map_err(|e| plain_io(state, &e))?;
    match cleaner_core::project::lock::try_exclusive(&state.join(MOVE_LOCK)) {
        Ok(Some(file)) => Ok(MoveLock(file)),
        Ok(None) => Err("The data folder is being moved right now. Try again when that is done.".into()),
        Err(error) => Err(plain_io(&state.join(MOVE_LOCK), &error)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Copying,
    Copied,
    Switched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// One app's own library into `<root>/<App>/`: a first setup or an adopt.
    Setup,
    /// The whole data folder.
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Rename,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalPointer {
    pub path: PathBuf,
    pub folder_id: String,
}

/// `move-journal.json`, the one schema both apps write and read (contract
/// section 7), so either app can finish a move the other started.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub schema: u32,
    pub app: String,
    pub kind: Kind,
    pub method: Method,
    pub from: PathBuf,
    pub to: PathBuf,
    pub pointer_file: PathBuf,
    pub pointer: JournalPointer,
    pub started_at: String,
    pub phase: Phase,
}

fn write_journal(state: &Path, journal: &Journal) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(journal).map_err(|e| e.to_string())?;
    write_atomic(&state.join(JOURNAL), &body).map_err(|e| plain_io(state, &e))
}

/// `Ok(None)`: no journal. `Err`: a journal this build cannot act on, which
/// is left exactly where it is.
fn read_journal(state: &Path) -> Result<Option<Journal>, String> {
    let bytes = match std::fs::read(state.join(JOURNAL)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(plain_io(&state.join(JOURNAL), &error)),
        Ok(bytes) => bytes,
    };
    match serde_json::from_slice::<Journal>(&bytes) {
        Ok(journal) if journal.schema == 1 => Ok(Some(journal)),
        _ => Err("An unfinished move of the data folder needs a newer version of this app to finish.".into()),
    }
}

pub fn journal_exists(state: &Path) -> bool {
    state.join(JOURNAL).exists()
}

fn partial_of(to: &Path) -> PathBuf {
    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    to.with_file_name(format!("{name}.partial"))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OldCopy {
    pub path: PathBuf,
    pub bytes: u64,
    pub moved_at: String,
}

pub fn old_copies(state: &Path) -> Vec<OldCopy> {
    let list: Vec<OldCopy> = std::fs::read(state.join(OLD_COPIES))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    list.into_iter().filter(|c| c.path.exists()).collect()
}

fn save_old_copies(state: &Path, list: &[OldCopy]) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?;
    write_atomic(&state.join(OLD_COPIES), &body).map_err(|e| e.to_string())
}

/// The old tree of a copied move, renamed aside and listed for the user.
fn set_aside(state: &Path, from: &Path) -> Result<(), String> {
    if !from.exists() {
        return Ok(());
    }
    let stamp = rfc3339_now().replace([':', '.'], "-");
    let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let aside = from.with_file_name(format!("{name}.moved-{stamp}"));
    std::fs::rename(from, &aside).map_err(|e| plain_io(from, &e))?;
    let mut list = old_copies(state);
    list.push(OldCopy { bytes: tree_bytes(&aside), path: aside, moved_at: rfc3339_now() });
    save_old_copies(state, &list)
}

/// Finish or undo whatever move a crash interrupted, the contract's section 7
/// recovery, whichever app wrote the journal. Idempotent. `Ok(Some)` says
/// what was done; `Err` is why it cannot be done yet, and the app stays
/// Unavailable until it can (no library opens at `from` or `to` while a
/// journal exists).
pub fn recover(state: &Path) -> Result<Option<String>, String> {
    if read_journal(state)?.is_none() {
        return Ok(None);
    }
    let _lock = take_move_lock(state)?;
    // Read again under the lock: the other app may have just finished it.
    let Some(mut journal) = read_journal(state)? else { return Ok(None) };
    if journal.kind == Kind::Move {
        for root in [&journal.from, &journal.to] {
            if typesetter_running(root) == Running::Yes {
                return Err("Close Pear so the data folder move can finish.".into());
            }
        }
    }
    let pointer = Pointer { path: journal.pointer.path.clone(), folder_id: journal.pointer.folder_id.clone() };
    if journal.phase == Phase::Copying {
        match journal.method {
            Method::Rename if journal.to.exists() && !journal.from.exists() => journal.phase = Phase::Copied,
            Method::Rename => {
                let _ = std::fs::remove_file(state.join(JOURNAL));
                return Ok(Some(format!("A move of the data folder was stopped. Your data is still at {}.", shown(&journal.from))));
            }
            // Only while `from` is whole: the copy started from an absent or
            // empty `to`, so anything there is this move's own copy.
            Method::Copy if journal.from.exists() => {
                let _ = std::fs::remove_dir_all(partial_of(&journal.to));
                if journal.to.exists() {
                    std::fs::remove_dir_all(&journal.to).map_err(|e| plain_io(&journal.to, &e))?;
                }
                let _ = std::fs::remove_file(state.join(JOURNAL));
                return Ok(Some(format!("A move of the data folder was stopped. Your data is still at {}.", shown(&journal.from))));
            }
            Method::Copy if journal.to.exists() => journal.phase = Phase::Copied,
            Method::Copy => return Err(format!("Neither {} nor {} is there. Reconnect the drive.", shown(&journal.from), shown(&journal.to))),
        }
    }
    if journal.phase == Phase::Copied {
        write_pointer(&journal.pointer_file, &pointer)?;
        journal.phase = Phase::Switched;
        write_journal(state, &journal)?;
    }
    if journal.method == Method::Copy {
        set_aside(state, &journal.from)?;
    }
    let _ = std::fs::remove_file(state.join(JOURNAL));
    Ok(Some(format!("A move of the data folder was finished. Your data is at {}.", shown(&journal.to))))
}

/// How a move is to be checked and run. `force_copy` is for tests, which
/// have one volume.
pub struct MoveRequest<'a> {
    pub state: &'a Path,
    pub pointer_file: &'a Path,
    pub kind: Kind,
    pub from: &'a Path,
    pub to: &'a Path,
    pub pointer: Pointer,
    pub force_copy: bool,
    /// Only for the cleaner's own library: page hashes to check the copy by.
    pub verify_pages: bool,
    /// A quiet setup (contract 5.2): one rename or nothing. A move that would
    /// have to copy is given up, and nothing is changed.
    pub rename_only: bool,
}

/// Bytes copied so far and the total of the copy in flight, for the
/// dialog's "Copying 5.5 of 9.7 GB" (contract 5.4). Both zero when no copy
/// runs.
static COPY_DONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static COPY_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `(copied, total)` of the copy in flight.
pub fn copy_progress() -> (u64, u64) {
    use std::sync::atomic::Ordering;
    (COPY_DONE.load(Ordering::Relaxed), COPY_TOTAL.load(Ordering::Relaxed))
}

struct CopyCounter;
impl CopyCounter {
    fn start(total: u64) -> CopyCounter {
        use std::sync::atomic::Ordering;
        COPY_DONE.store(0, Ordering::Relaxed);
        COPY_TOTAL.store(total, Ordering::Relaxed);
        CopyCounter
    }
}
impl Drop for CopyCounter {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        COPY_DONE.store(0, Ordering::Relaxed);
        COPY_TOTAL.store(0, Ordering::Relaxed);
    }
}

/// Set while this process moves a tree: every library call is refused, so no
/// chapter opens and no run starts mid-move.
static MOVING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct Moving;
impl Moving {
    fn start() -> Moving {
        MOVING.store(true, std::sync::atomic::Ordering::SeqCst);
        Moving
    }
}
impl Drop for Moving {
    fn drop(&mut self) {
        MOVING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Move `from` to `to`, journaled, ending with the pointer written. `to`
/// must not exist or be empty. One rename on one volume; across volumes a
/// copy into `<to>.partial` that keeps hard links and is fsynced and checked,
/// then renamed into place, and the old tree is set aside rather than
/// deleted. A failure before the pointer is written leaves `from` as it was.
pub fn run_move(request: MoveRequest) -> Result<(), String> {
    let MoveRequest { state, pointer_file, kind, from, to, pointer, force_copy, verify_pages, rename_only } = request;
    if !from.is_dir() {
        return Err(format!("{} is not a folder", shown(from)));
    }
    if to.starts_with(from) {
        return Err("The new place is inside the old one.".into());
    }
    if to.exists() {
        if has_content(to) {
            return Err(format!("{} is not empty", shown(to)));
        }
        std::fs::remove_dir(to).map_err(|e| plain_io(to, &e))?;
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| plain_io(parent, &e))?;
    }
    let mut journal = Journal {
        schema: 1,
        app: "manga-cleaner".into(),
        kind,
        method: Method::Rename,
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        pointer_file: pointer_file.to_path_buf(),
        pointer: JournalPointer { path: pointer.path.clone(), folder_id: pointer.folder_id.clone() },
        started_at: rfc3339_now(),
        phase: Phase::Copying,
    };
    write_journal(state, &journal)?;
    let abandon = |error: String| {
        let _ = std::fs::remove_dir_all(partial_of(to));
        let _ = std::fs::remove_file(state.join(JOURNAL));
        Err(error)
    };
    let renamed = if force_copy {
        false
    } else {
        match std::fs::rename(from, to) {
            Ok(()) => true,
            Err(error) if cross_device(&error) => false,
            Err(error) => return abandon(plain_io(from, &error)),
        }
    };
    if !renamed && rename_only {
        return abandon("The library is on another drive than the new place, so it was left where it is.".into());
    }
    if !renamed {
        // Said before the first byte is copied, so a recovery knows that
        // anything at `to` is this move's own copy.
        journal.method = Method::Copy;
        write_journal(state, &journal)?;
        let partial = partial_of(to);
        let _ = std::fs::remove_dir_all(&partial);
        if let Err(error) = std::fs::create_dir_all(&partial) {
            return abandon(plain_io(&partial, &error));
        }
        let links = links_work(&partial);
        if let Err(error) = preflight_copy(from, &partial, links) {
            return abandon(error);
        }
        let _counter = CopyCounter::start(tree_bytes(from));
        let copied = copy_tree(from, &partial, links, &mut Default::default())
            .and_then(|()| verify_copy(from, &partial, links))
            .and_then(|()| if verify_pages { verify_page_hashes(&partial) } else { Ok(()) })
            .and_then(|()| std::fs::rename(&partial, to).map_err(|e| plain_io(to, &e)));
        if let Err(error) = copied {
            let _ = std::fs::remove_dir_all(to);
            return abandon(error);
        }
    }
    journal.phase = Phase::Copied;
    write_journal(state, &journal)?;
    write_pointer(pointer_file, &pointer)?;
    journal.phase = Phase::Switched;
    write_journal(state, &journal)?;
    if journal.method == Method::Copy {
        set_aside(state, from)?;
    }
    let _ = std::fs::remove_file(state.join(JOURNAL));
    Ok(())
}

fn cross_device(error: &std::io::Error) -> bool {
    // `ErrorKind::CrossesDevices` needs Rust 1.85; the workspace builds on 1.82.
    (cfg!(unix) && error.raw_os_error() == Some(18))
        || (cfg!(windows) && error.raw_os_error() == Some(17))
}

/// Nothing evicted to the cloud, and room for the copy.
fn preflight_copy(from: &Path, to: &Path, links: bool) -> Result<(), String> {
    let offline = cloud_only_files(from);
    if offline > 0 {
        return Err(format!(
            "{offline} files in {} are only in the cloud. Download them first (in Finder or File Explorer), then try again.",
            shown(from)
        ));
    }
    let need = copy_bytes(from, links, cluster_size(to));
    if let Some(free) = crate::weights::free_space(to) {
        let need_with_margin = need + need / 20;
        if free < need_with_margin {
            return Err(format!(
                "Not enough space at {}: the move needs {:.1} GB and {:.1} GB is free.",
                shown(to),
                need_with_margin as f64 / 1e9,
                free as f64 / 1e9
            ));
        }
    }
    Ok(())
}

/// A file id: device and inode, so hard links count once.
#[cfg(unix)]
fn file_id(_path: &Path, meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (meta.nlink() > 1).then(|| (meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn file_id(path: &Path, meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    // Volume serial and file index, the Windows inode, from the file's own
    // handle (`MetadataExt::file_index` is unstable). Without it a hard-linked
    // page was copied twice on a move to another drive, and the verify step
    // could not see the links it lost.
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
    if !meta.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: an all-zero BY_HANDLE_FILE_INFORMATION is a valid out value, and
    // the handle stays open for the whole call.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 || info.nNumberOfLinks < 2 {
        return None;
    }
    Some((u64::from(info.dwVolumeSerialNumber), (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)))
}

#[cfg(not(any(unix, windows)))]
fn file_id(_path: &Path, _meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Bytes of a tree, each hard-linked file once.
fn tree_bytes(root: &Path) -> u64 {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    walk(root, &mut |path, meta| {
        if meta.is_file() && file_id(path, meta).is_none_or(|id| seen.insert(id)) {
            total += meta.len();
        }
    });
    total
}

/// What a copy of `root` takes at the new place: each file rounded up to the
/// volume's cluster, a hard-linked file once where links work.
fn copy_bytes(root: &Path, links: bool, cluster: u64) -> u64 {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    walk(root, &mut |path, meta| {
        if meta.is_file() && (!links || file_id(path, meta).is_none_or(|id| seen.insert(id))) {
            total += meta.len().div_ceil(cluster) * cluster;
        }
    });
    total
}

/// Whether `dir` can hold hard links. exFAT, FAT and some network shares
/// cannot: there a linked file is copied once per name (contract R4: raws are
/// then stored twice, never lost).
fn links_work(dir: &Path) -> bool {
    let (a, b) = (dir.join(".link-probe"), dir.join(".link-probe-2"));
    let ok = std::fs::write(&a, b"").is_ok() && std::fs::hard_link(&a, &b).is_ok();
    let _ = std::fs::remove_file(&b);
    let _ = std::fs::remove_file(&a);
    ok
}

/// The smallest unit the volume holding `path` gives a file, so the space
/// check counts each file rounded up to it: exFAT clusters reach 128 KB, and a
/// library of small files then needs far more than its byte total.
#[cfg(unix)]
fn cluster_size(path: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let mut p = path;
    while !p.exists() {
        match p.parent() {
            Some(parent) => p = parent,
            None => return 4096,
        }
    }
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return 4096 };
    // SAFETY: an all-zero statvfs is a valid out value, and `c` is a valid C
    // string for the whole call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return 4096;
    }
    #[allow(clippy::unnecessary_cast)]
    (st.f_frsize as u64).clamp(512, 1 << 20)
}

#[cfg(not(unix))]
fn cluster_size(_path: &Path) -> u64 {
    4096
}

/// macOS keeps extended attributes on exFAT and FAT as `._<name>` files
/// beside each file it writes. They are not part of the library, so the
/// copy check leaves them out on both sides.
fn apple_double(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("._"))
}

fn walk(root: &Path, visit: &mut dyn FnMut(&Path, &std::fs::Metadata)) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if meta.is_dir() {
                stack.push(path.clone());
            }
            visit(&path, &meta);
        }
    }
}

/// Files whose data is not on this computer: macOS `SF_DATALESS`, Windows
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` or `..._OFFLINE`.
fn cloud_only_files(root: &Path) -> usize {
    let mut count = 0;
    walk(root, &mut |_, meta| {
        if meta.is_file() && cloud_only(meta) {
            count += 1;
        }
    });
    count
}

#[cfg(target_os = "macos")]
fn cloud_only(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(windows)]
fn cloud_only(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const OFFLINE: u32 = 0x0000_1000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    meta.file_attributes() & (OFFLINE | RECALL_ON_DATA_ACCESS) != 0
}

#[cfg(not(any(target_os = "macos", windows)))]
fn cloud_only(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Copy a tree: hard links kept where `links` (a file already copied under
/// another name is linked to its copy), symlinks as symlinks, every file and
/// folder fsynced.
fn copy_tree(
    from: &Path,
    to: &Path,
    links: bool,
    linked: &mut std::collections::HashMap<(u64, u64), PathBuf>,
) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| plain_io(to, &e))?;
    for entry in std::fs::read_dir(from).map_err(|e| plain_io(from, &e))? {
        let entry = entry.map_err(|e| plain_io(from, &e))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&source).map_err(|e| plain_io(&source, &e))?;
        if meta.file_type().is_symlink() {
            copy_symlink(&source, &target)?;
        } else if meta.is_dir() {
            copy_tree(&source, &target, links, linked)?;
        } else {
            if let Some(id) = file_id(&source, &meta).filter(|_| links) {
                if let Some(first) = linked.get(&id) {
                    std::fs::hard_link(first, &target).map_err(|e| plain_io(&target, &e))?;
                    continue;
                }
                linked.insert(id, target.clone());
            }
            std::fs::copy(&source, &target).map_err(|e| plain_io(&source, &e))?;
            flush_copied(&target).map_err(|e| plain_io(&target, &e))?;
            COPY_DONE.fetch_add(meta.len(), std::sync::atomic::Ordering::Relaxed);
        }
    }
    sync_dir(to);
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, target: &Path) -> Result<(), String> {
    let link = std::fs::read_link(source).map_err(|e| plain_io(source, &e))?;
    std::os::unix::fs::symlink(link, target).map_err(|e| plain_io(target, &e))
}

#[cfg(not(unix))]
fn copy_symlink(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::copy(source, target).map(|_| ()).map_err(|e| plain_io(&source, &e))
}

/// Same relative paths and sizes, and the same files linked together where
/// the new place has links.
fn verify_copy(from: &Path, to: &Path, links: bool) -> Result<(), String> {
    let (a, b) = (listing(from)?, listing(to)?);
    if a.files != b.files {
        let first = a.files.iter().find(|entry| !b.files.contains(entry)).or_else(|| b.files.iter().find(|e| !a.files.contains(e)));
        return Err(format!(
            "The copy does not match the original ({} vs {} files{}). Nothing was changed.",
            a.files.len(),
            b.files.len(),
            first.map(|(p, _)| format!(", first difference {}", shown(p))).unwrap_or_default()
        ));
    }
    if links && a.groups != b.groups {
        return Err("The copy lost some hard links. Nothing was changed.".into());
    }
    Ok(())
}

#[derive(Default)]
struct Listing {
    files: std::collections::BTreeSet<(PathBuf, u64)>,
    /// Relative paths that share one file, each group sorted.
    groups: std::collections::BTreeSet<Vec<PathBuf>>,
}

fn listing(root: &Path) -> Result<Listing, String> {
    let mut out = Listing::default();
    let mut ids: std::collections::HashMap<(u64, u64), Vec<PathBuf>> = Default::default();
    if !root.is_dir() {
        return Err(format!("{} is not a folder", shown(root)));
    }
    walk(root, &mut |path, meta| {
        if meta.is_dir() || apple_double(path) {
            return;
        }
        let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
        if let Some(id) = file_id(path, meta) {
            ids.entry(id).or_default().push(rel.clone());
        }
        out.files.insert((rel, if meta.file_type().is_symlink() { 0 } else { meta.len() }));
    });
    out.groups = ids.into_values().filter(|g| g.len() > 1).map(|mut g| {
        g.sort();
        g
    }).collect();
    Ok(out)
}

/// Every page a cleaner manifest in the tree names, against its recorded
/// sha256. Only the pages inside the tree; a legacy manifest that still reads
/// a source in the user's folder is not the copy's to check.
fn verify_page_hashes(root: &Path) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let mut manifests = Vec::new();
    walk(root, &mut |path, meta| {
        if meta.is_file() && path.extension().is_some_and(|e| e == cleaner_core::project::EXTENSION) {
            manifests.push(path.to_path_buf());
        }
    });
    for manifest in manifests {
        let Some(value) = read_json(&manifest) else { continue };
        let dir = manifest.parent().unwrap_or(root);
        for source in value.get("sources").and_then(|s| s.as_array()).into_iter().flatten() {
            let (Some(rel), Some(sha)) = (source.get("rel_path").and_then(|v| v.as_str()), source.get("sha256").and_then(|v| v.as_str())) else {
                continue;
            };
            let path = dir.join(rel);
            if !path.starts_with(root) || Path::new(rel).is_absolute() || Path::new(rel).components().any(|c| c == std::path::Component::ParentDir) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                return Err(format!("{} is missing from the copy. Nothing was changed.", shown(&path)));
            };
            let got = format!("{:x}", Sha256::digest(&bytes));
            if got != sha {
                return Err(format!("{} did not copy correctly. Nothing was changed.", shown(&path)));
            }
        }
    }
    Ok(())
}

/* ------------------------------------------------------------------ */
/* Commands                                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub mode: Mode,
    pub root: Option<String>,
    pub library: Option<String>,
    pub reason: Option<String>,
    pub default_root: Option<String>,
    /// "no", "yes" or "unknown".
    pub typesetter_running: &'static str,
    /// The chosen or current folder is inside a folder a cloud service syncs.
    pub synced: bool,
    pub old_copies: Vec<OldCopy>,
    /// What the last start's recovery did, if anything.
    pub recovered: Option<String>,
    /// Pear has run on this computer (contract 4.1).
    pub sister_present: bool,
    /// A move is unfinished: its journal is still there (start row B).
    pub journal: bool,
    /// How many titles the library this app reads now holds.
    pub titles: usize,
    /// The user's home folder, so the interface can say `~/Manga Library`.
    pub home: Option<String>,
    /// Pending with a `Cleaner/` subfolder that is there but empty: a
    /// version 1 setup with nothing to move leaves this. With an accepted
    /// stored answer the interface fills it without asking
    /// (`data_folder_fill_empty`, contract 5.1).
    pub cleaner_empty: bool,
}

static RECOVERED: Mutex<Option<String>> = Mutex::new(None);

fn status_of(app: &tauri::AppHandle, resolved: &Resolved) -> Status {
    use tauri::Manager;
    let running = match resolved.root().map(typesetter_running) {
        Some(Running::Yes) => "yes",
        Some(Running::CannotTell) => "unknown",
        _ => "no",
    };
    let home = app.path().home_dir().ok();
    Status {
        mode: resolved.mode,
        root: resolved.root().map(|p| p.display().to_string()),
        library: resolved.library.as_ref().map(|p| p.display().to_string()),
        reason: resolved.reason.clone(),
        default_root: home.as_ref().map(|d| d.join(DEFAULT_NAME).display().to_string()),
        typesetter_running: running,
        synced: resolved.root().is_some_and(|r| in_synced_folder(r, home.as_deref())),
        old_copies: state_root(app).map(|s| old_copies(&s)).unwrap_or_default(),
        recovered: RECOVERED.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        sister_present: sister_present(app),
        journal: state_root(app).is_ok_and(|state| journal_exists(&state)),
        titles: resolved.library.as_deref().map(titles_in).unwrap_or(0),
        home: home.as_ref().map(|h| h.display().to_string()),
        cleaner_empty: resolved.mode == Mode::Pending && resolved.root().is_some_and(own_is_empty),
    }
}

/// `Cleaner/` under `root` is a folder, with nothing in it that counts.
fn own_is_empty(root: &Path) -> bool {
    let own = root.join(CLEANER_DIR);
    own.is_dir() && own_state(&own) == Own::Absent
}

/// Fill an empty `Cleaner/` with its library file (contract 5.1, the
/// exception for an answer already accepted). Answers whether it did: a
/// missing subfolder, or one with anything in it, is left alone.
pub fn fill_empty_own(root: &Path) -> Result<bool, String> {
    if !own_is_empty(root) {
        return Ok(false);
    }
    crate::library::Library::at(root.join(CLEANER_DIR)).ensure_index().map_err(|e| e.to_string())?;
    Ok(true)
}

/// Pending with an empty `Cleaner/`, and this app's stored answer says it
/// already set up or joined this folder: make it Shared without asking.
/// The answer is the interface's to read; this only refuses any other state.
#[tauri::command]
pub async fn data_folder_fill_empty(app: tauri::AppHandle) -> Result<Status, String> {
    crate::library::blocking(move || {
        let state = state_root(&app)?;
        let _lock = take_move_lock(&state)?;
        let resolved = refresh(&app)?;
        let root = match (resolved.mode, resolved.root()) {
            (Mode::Pending, Some(root)) => root.to_path_buf(),
            _ => return Ok(status_of(&app, &resolved)),
        };
        fill_empty_own(&root)?;
        let resolved = refresh(&app)?;
        Ok(status_of(&app, &resolved))
    })
    .await
}

/// Inside a folder iCloud, OneDrive, Dropbox or Google Drive syncs, or macOS
/// Documents (which iCloud "Desktop and Documents" often syncs).
pub fn in_synced_folder(path: &Path, home: Option<&Path>) -> bool {
    let text = path.to_string_lossy().to_lowercase();
    let synced = ["mobile documents", "library/cloudstorage", "onedrive", "dropbox", "google drive", "icloud"]
        .iter()
        .any(|needle| text.contains(needle));
    let documents = cfg!(target_os = "macos") && home.is_some_and(|h| path.starts_with(h.join("Documents")) || path.starts_with(h.join("Desktop")));
    synced || documents
}

/// At start, before anything reads the library.
pub fn on_start(app: &tauri::AppHandle) {
    // `refresh` runs the recovery of an interrupted move first.
    if let Err(error) = refresh(app) {
        eprintln!("manga-cleaner: data folder: {error}");
    }
}

#[tauri::command]
pub async fn data_folder_status(app: tauri::AppHandle) -> Result<Status, String> {
    crate::library::blocking(move || {
        let resolved = refresh(&app)?;
        Ok(status_of(&app, &resolved))
    })
    .await
}

/// What every change needs first: nothing running here (no job, no Send to
/// Pear), and Pear closed in every data folder involved (contract 6 and 7:
/// a Pending move into the folder is refused while Pear runs too).
fn ready_to_move(roots: &[&Path], confirmed_closed: bool) -> Result<(), String> {
    if crate::jobs::running() > 0 {
        return Err("Wait for the running jobs to finish first.".into());
    }
    if crate::send_to_pear::running() {
        return Err("Wait for Send to Pear to finish first.".into());
    }
    for root in roots {
        match typesetter_running(root) {
            Running::Yes => return Err(CLOSE_PEAR.into()),
            Running::CannotTell if !confirmed_closed => {
                return Err("typesetter_unknown: This drive cannot tell whether Pear is open. Close it, then confirm.".into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// The Blocked state's words (contract 5.4 and 7).
pub const CLOSE_PEAR: &str = "Close Pear first.";

/// Before the cleaner library moves: every origin path that leaves it becomes
/// absolute, so the `..` chain from a manifest to the user's scan folder is
/// not broken by the move. Paths inside the library stay relative.
/// `boundary` is the tree about to move: a relative path that stays inside it
/// keeps working after the move and is left alone.
fn absolutize_origins(library: &Path, boundary: &Path) -> Result<(), String> {
    let lib = crate::library::Library::at(library);
    let index = lib.index().map_err(|e| e.to_string())?;
    for project in &index.projects {
        for chapter in &project.chapters {
            let path = lib.job_path(&project.id, &chapter.id);
            if !path.exists() {
                continue;
            }
            let _lock = crate::run::lock_job(&path).map_err(|e| e.to_string())?;
            // A manifest that does not open cannot be cleaned or exported
            // anyway; it must not hold every other chapter's move hostage.
            let mut job = match cleaner_core::project::Job::open(&path) {
                Ok(job) => job,
                Err(error) => {
                    eprintln!("manga-cleaner: {}: left as it is before the move: {error}", path.display());
                    continue;
                }
            };
            if absolutize_sources(&mut job.project, path.parent().unwrap_or(library), boundary) {
                job.flush().map_err(|e| plain_other(&path.display().to_string(), e))?;
            }
        }
    }
    Ok(())
}

/// The rewrite itself. `true` when anything changed.
pub fn absolutize_sources(project: &mut cleaner_core::project::Project, manifest_dir: &Path, boundary: &Path) -> bool {
    let mut changed = false;
    let boundary = lexical_normalise(boundary);
    let mut fix = |path: &mut PathBuf| {
        if path.is_relative() {
            let resolved = lexical_normalise(&manifest_dir.join(&*path));
            if !resolved.starts_with(&boundary) {
                *path = resolved;
                changed = true;
            }
        }
    };
    for source in &mut project.sources {
        fix(&mut source.rel_path);
        if let Some(from) = source.converted_from.as_mut() {
            fix(from);
        }
        if let Some(conversion) = source.conversion.as_mut() {
            fix(&mut conversion.archived_original);
        }
    }
    for row in &mut project.denoised {
        fix(&mut row.path);
        if let Some(source) = row.source.as_mut() {
            fix(source);
        }
    }
    changed
}

pub(crate) fn lexical_normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn checked_target(target: &str) -> Result<PathBuf, String> {
    let target = PathBuf::from(target);
    if !target.is_absolute() {
        return Err("Pick a full folder path.".into());
    }
    path_str(&target)?;
    Ok(target)
}

/// Legacy: make `target` the data folder and move this library into
/// `<target>/Cleaner`. Pending: the same into the existing folder (Join).
/// A library with nothing in it moves nothing: `Cleaner/` and its index are
/// made, then the pointer (contract 5.5).
#[tauri::command]
pub async fn data_folder_setup(app: tauri::AppHandle, target: String, confirmed_closed: Option<bool>) -> Result<Status, String> {
    crate::library::blocking(move || {
        let state = state_root(&app)?;
        let _lock = take_move_lock(&state)?;
        let resolved = refresh(&app)?;
        let target = checked_target(&target)?;
        match resolved.mode {
            Mode::Shared => return Err("The data folder is already set up.".into()),
            Mode::Unavailable => return Err(resolved.reason.unwrap_or_default()),
            Mode::Pending if resolved.root() != Some(target.as_path()) => {
                return Err("Pear already set up a data folder. Move this library into that one.".into());
            }
            _ => {}
        }
        check_setup_target(&target)?;
        // Contract 7: a setup or a Pending move is refused while Pear runs in
        // that folder ("Close Pear first."), as any move is.
        ready_to_move(&[target.as_path()], confirmed_closed.unwrap_or(false))?;
        setup_into(&app, &state, &resolved, &target, false)?;
        let resolved = refresh(&app)?;
        Ok(status_of(&app, &resolved))
    })
    .await
}

/// A folder a setup may use: absent, empty, or already a data folder whose
/// `Cleaner/` holds nothing.
fn check_setup_target(target: &Path) -> Result<(), String> {
    if target.exists() && has_content(target) && !is_data_folder(target) {
        return Err(format!("{} is not empty. Pick an empty folder, or one that is already a data folder.", shown(target)));
    }
    let own = target.join(CLEANER_DIR);
    if own_state(&own) != Own::Absent {
        return Err(format!("{} already has a cleaner library.", shown(&own)));
    }
    Ok(())
}

/// The setup itself, under the move lock, every check already made: the
/// marker, then this library into `Cleaner/` (or a new empty one), the index
/// there, and the pointer last.
fn setup_into(app: &tauri::AppHandle, state: &Path, resolved: &Resolved, target: &Path, rename_only: bool) -> Result<(), String> {
    let own = target.join(CLEANER_DIR);
    let folder_id = ensure_marker(target)?;
    let pointer = Pointer { path: target.to_path_buf(), folder_id };
    if has_content(&resolved.legacy) {
        let _moving = Moving::start();
        absolutize_origins(&resolved.legacy, &resolved.legacy).map_err(|e| plain_other("before the move", e))?;
        run_move(MoveRequest {
            state,
            pointer_file: &pointer_file(app)?,
            kind: Kind::Setup,
            from: &resolved.legacy,
            to: &own,
            pointer: pointer.clone(),
            force_copy: false,
            verify_pages: true,
            rename_only,
        })?;
        // A legacy folder that held files but never an index (nothing was
        // ever created in it) must not read as a damaged `Cleaner/`.
        crate::library::Library::at(&own).ensure_index()?;
    } else {
        crate::library::Library::at(&own).ensure_index()?;
        write_pointer(&pointer_file(app)?, &pointer)?;
    }
    Ok(())
}

/* ------------------------------------------------------------------ */
/* Quiet setup (contract 5.2)                                          */
/* ------------------------------------------------------------------ */

/// What a quiet setup would move and where, and what it must not touch.
pub struct QuietCheck<'a> {
    pub legacy: &'a Path,
    pub default_root: &'a Path,
    pub home: Option<&'a Path>,
    pub jobs_running: bool,
}

/// Why a quiet setup must not run, or `None` when it may. Each reason is the
/// one the contract lists; the app then stays Legacy and says nothing.
pub fn quiet_refusal(check: &QuietCheck) -> Option<&'static str> {
    if check.jobs_running {
        return Some("a job is running");
    }
    if !absent_or_empty(check.default_root) {
        return Some("the default place is not empty");
    }
    let library = has_content(check.legacy);
    if library && !same_volume(check.legacy, check.default_root) {
        return Some("the library is on another volume");
    }
    if library && cloud_only_files(check.legacy) > 0 {
        return Some("some library files are only in the cloud");
    }
    // Only the place it goes to (contract 5.2). The library may sit in a
    // synced folder: moving out of it is the point, and the cloud-only
    // check above covers the risk.
    if in_synced_folder(check.default_root, check.home) {
        return Some("the default place is in a synced folder");
    }
    None
}

/// The nearest folder of `path` that exists.
fn existing_ancestor(path: &Path) -> Option<&Path> {
    let mut at = path;
    while !at.exists() {
        at = at.parent()?;
    }
    Some(at)
}

/// Whether a rename from `a` to `b` stays on one volume. `b` need not exist.
#[cfg(unix)]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| existing_ancestor(p).and_then(|p| std::fs::metadata(p).ok()).map(|m| m.dev());
    matches!((dev(a), dev(b)), (Some(x), Some(y)) if x == y)
}

/// Windows: the same drive or share. A volume mounted in a folder is the
/// rare case this misses; the quiet setup's rename then fails across
/// volumes and gives up with nothing changed (`rename_only`).
#[cfg(not(unix))]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    let prefix = |p: &Path| match p.components().next() {
        Some(std::path::Component::Prefix(prefix)) => Some(prefix.as_os_str().to_string_lossy().to_lowercase()),
        _ => None,
    };
    prefix(a).is_some() && prefix(a) == prefix(b)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietOutcome {
    /// The library now lives in the shared folder.
    pub done: bool,
    pub status: Status,
}

/// Row E of the start table: no pointer and no Pear on this computer. Set up
/// at `~/Manga Library` without asking, only when every check of contract
/// 5.2 holds, and every check made again under `move.lock`. Anything else
/// leaves the app in Legacy and answers `done: false`.
#[tauri::command]
pub async fn data_folder_quiet_setup(app: tauri::AppHandle) -> Result<QuietOutcome, String> {
    crate::library::blocking(move || {
        use tauri::Manager;
        let home = app.path().home_dir().map_err(|e| e.to_string())?;
        let default_root = home.join(DEFAULT_NAME);
        let checks = |resolved: &Resolved| -> Option<&'static str> {
            if resolved.mode != Mode::Legacy {
                return Some("not in Legacy mode");
            }
            if sister_present(&app) {
                return Some("Pear has run on this computer");
            }
            quiet_refusal(&QuietCheck {
                legacy: &resolved.legacy,
                default_root: &default_root,
                home: Some(&home),
                jobs_running: crate::jobs::running() > 0 || crate::send_to_pear::running(),
            })
        };
        let not_done = |app: &tauri::AppHandle, why: &str| -> Result<QuietOutcome, String> {
            eprintln!("manga-cleaner: data folder: no quiet setup: {why}");
            let resolved = refresh(app)?;
            Ok(QuietOutcome { done: false, status: status_of(app, &resolved) })
        };
        if let Some(why) = checks(&refresh(&app)?) {
            return not_done(&app, why);
        }
        let state = state_root(&app)?;
        let Ok(lock) = take_move_lock(&state) else { return not_done(&app, "the move lock is held") };
        let resolved = refresh(&app)?;
        if let Some(why) = checks(&resolved) {
            drop(lock);
            return not_done(&app, why);
        }
        let made = !default_root.exists();
        if let Err(error) = setup_into(&app, &state, &resolved, &default_root, true) {
            // Nothing moved: take back the folder this attempt made, so the
            // default place is as it was.
            if read_pointer(&pointer_file(&app)?) == PointerRead::None {
                let _ = std::fs::remove_file(default_root.join(MARKER));
                let _ = std::fs::remove_dir(default_root.join(CLEANER_DIR));
                if made {
                    let _ = std::fs::remove_dir(&default_root);
                }
            }
            drop(lock);
            return not_done(&app, &error);
        }
        drop(lock);
        let resolved = refresh(&app)?;
        Ok(QuietOutcome { done: resolved.mode == Mode::Shared, status: status_of(&app, &resolved) })
    })
    .await
}

/* ------------------------------------------------------------------ */
/* The dialog's facts (contract 5.4)                                   */
/* ------------------------------------------------------------------ */

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupPlan {
    /// The folder the library would go into: the data folder, not `Cleaner/`.
    pub target: String,
    /// The bytes that would move, each hard-linked file once.
    pub bytes: u64,
    /// The library holds nothing: "Nothing to move."
    pub nothing_to_move: bool,
    /// One rename: "It takes a moment."
    pub same_volume: bool,
    /// The target is inside a folder a cloud service syncs.
    pub synced: bool,
    /// The target cannot take a library (not empty and not a data folder, or
    /// one whose `Cleaner/` is already used): the plain reason.
    pub refusal: Option<String>,
}

/// What a setup at `target` (or a Join of the pointer's folder when `target`
/// is empty) would do: how much moves and whether it is one rename. Reads
/// only.
#[tauri::command]
pub async fn data_folder_plan(app: tauri::AppHandle, target: Option<String>) -> Result<SetupPlan, String> {
    crate::library::blocking(move || {
        use tauri::Manager;
        let resolved = current(&app)?;
        let target = match target.filter(|t| !t.trim().is_empty()) {
            Some(target) => checked_target(&target)?,
            None => match resolved.root() {
                Some(root) => root.to_path_buf(),
                None => app.path().home_dir().map_err(|e| e.to_string())?.join(DEFAULT_NAME),
            },
        };
        let library = has_content(&resolved.legacy) && resolved.mode != Mode::Shared;
        let home = app.path().home_dir().ok();
        Ok(SetupPlan {
            bytes: if library { tree_bytes(&resolved.legacy) } else { 0 },
            nothing_to_move: !library,
            same_volume: !library || same_volume(&resolved.legacy, &target),
            synced: in_synced_folder(&target, home.as_deref()),
            refusal: check_setup_target(&target).err(),
            target: target.display().to_string(),
        })
    })
    .await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveProgress {
    pub copied: u64,
    pub total: u64,
}

/// How far the copy in flight has come. Zero and zero when none runs.
#[tauri::command]
pub fn data_folder_progress() -> MoveProgress {
    let (copied, total) = copy_progress();
    MoveProgress { copied, total }
}

/// Open the data folder in Finder or File Explorer: the quiet setup's
/// "Show". Only ever the pointer's folder, never a path the interface names.
#[tauri::command]
pub async fn data_folder_show(app: tauri::AppHandle) -> Result<(), String> {
    let root = crate::library::blocking({
        let app = app.clone();
        move || current(&app)?.root().map(Path::to_path_buf).ok_or_else(|| "There is no data folder.".to_owned())
    })
    .await?;
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_path(root.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

/// Shared: move the whole data folder to `target`, both apps' data with it.
#[tauri::command]
pub async fn data_folder_move(app: tauri::AppHandle, target: String, confirmed_closed: Option<bool>) -> Result<Status, String> {
    crate::library::blocking(move || {
        let state = state_root(&app)?;
        let _lock = take_move_lock(&state)?;
        let resolved = refresh(&app)?;
        let (Mode::Shared, Some(pointer)) = (resolved.mode, resolved.pointer.clone()) else {
            return Err("Set up the data folder first.".into());
        };
        let root = pointer.path.clone();
        let target = checked_target(&target)?;
        if target == root {
            return Err("That is where the data folder already is.".into());
        }
        ready_to_move(&[root.as_path()], confirmed_closed.unwrap_or(false))?;
        let moving = Moving::start();
        if let Some(library) = &resolved.library {
            absolutize_origins(library, &root)?;
        }
        // Let go of the lock first: Windows will not rename a folder with an
        // open file inside it.
        release_lock();
        let moved = run_move(MoveRequest {
            state: &state,
            pointer_file: &pointer_file(&app)?,
            kind: Kind::Move,
            from: &root,
            to: &target,
            pointer: Pointer { path: target.clone(), folder_id: pointer.folder_id },
            force_copy: false,
            verify_pages: true,
            rename_only: false,
        });
        drop(moving);
        let resolved = refresh(&app)?;
        moved?;
        Ok(status_of(&app, &resolved))
    })
    .await
}

/// Point at a folder that is already a data folder and move nothing.
#[tauri::command]
pub async fn data_folder_use_existing(app: tauri::AppHandle, target: String, confirmed_closed: Option<bool>) -> Result<Status, String> {
    crate::library::blocking(move || {
        let state = state_root(&app)?;
        let _lock = take_move_lock(&state)?;
        let resolved = refresh(&app)?;
        let target = checked_target(&target)?;
        let Some((1, folder_id)) = read_marker(&target) else {
            return Err(format!("{} is not a data folder (it has no {MARKER}).", shown(&target)));
        };
        if let PointerRead::Unknown(reason) = read_pointer(&pointer_file(&app)?) {
            return Err(reason);
        }
        if journal_exists(&state) {
            return Err("A move of the data folder is not finished yet.".into());
        }
        if resolved.mode == Mode::Legacy && has_content(&resolved.legacy) {
            return Err("This computer still has its own cleaner library. Set up the shared folder with it first, or use \"Set up\" with that folder.".into());
        }
        let mut roots = vec![target.as_path()];
        if let Some(root) = resolved.root() {
            roots.push(root);
        }
        ready_to_move(&roots, confirmed_closed.unwrap_or(false))?;
        write_pointer(&pointer_file(&app)?, &Pointer { path: target.clone(), folder_id })?;
        let resolved = refresh(&app)?;
        Ok(status_of(&app, &resolved))
    })
    .await
}

/// Delete an old tree a copied move set aside. Only paths on the list.
#[tauri::command]
pub async fn data_folder_delete_old_copy(app: tauri::AppHandle, path: String) -> Result<Status, String> {
    crate::library::blocking(move || {
        let state = state_root(&app)?;
        let mut list = old_copies(&state);
        let Some(at) = list.iter().position(|c| c.path == Path::new(&path)) else {
            return Err("That folder is not an old copy this app made.".into());
        };
        let resolved = refresh(&app)?;
        let live: Vec<PathBuf> = [resolved.root().map(Path::to_path_buf), resolved.library.clone(), Some(resolved.legacy.clone())]
            .into_iter()
            .flatten()
            .collect();
        if let Some(reason) = old_copy_refusal(&list[at].path, &live) {
            return Err(reason);
        }
        let copy = list.remove(at);
        std::fs::remove_dir_all(&copy.path).map_err(|e| plain_io(&copy.path, &e))?;
        save_old_copies(&state, &list)?;
        let resolved = refresh(&app)?;
        Ok(status_of(&app, &resolved))
    })
    .await
}

/// Why an old copy must not be deleted: it is, holds or sits inside a folder
/// in use. The pointer's folder is one of those, which covers an old copy
/// picked with "use existing" after an unplug. Its marker is not a test: a
/// moved copy keeps the folder's id by design.
fn old_copy_refusal(path: &Path, live: &[PathBuf]) -> Option<String> {
    let path = lexical_normalise(path);
    for live in live {
        let live = lexical_normalise(live);
        if path.starts_with(&live) || live.starts_with(&path) {
            return Some(format!("{} is in use. It was not deleted.", shown(&path)));
        }
    }
    None
}

/// Seconds since the epoch as RFC 3339 in UTC, with milliseconds.
pub fn rfc3339_now() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    rfc3339(now.as_secs(), now.subsec_millis())
}

fn rfc3339(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) struct Scratch(pub PathBuf);
    impl Scratch {
        pub fn new(name: &str) -> Scratch {
            use std::sync::atomic::{AtomicU32, Ordering};
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join("cleaner-data-folder")
                .join(format!("{name}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree(root: &Path) {
        std::fs::create_dir_all(root.join("p1/c2.mtclean.d/pages")).unwrap();
        std::fs::write(root.join("index.json"), b"{\"version\":1}").unwrap();
        std::fs::write(root.join("p1/c2.mtclean.d/pages/001.png"), vec![7u8; 5000]).unwrap();
        // A page whose sha256 is what the manifest says.
        use sha2::{Digest, Sha256};
        let sha = format!("{:x}", Sha256::digest(vec![7u8; 5000]));
        std::fs::write(
            root.join("p1/c2.mtclean"),
            serde_json::to_vec(&serde_json::json!({ "sources": [{ "rel_path": "c2.mtclean.d/pages/001.png", "sha256": sha }] })).unwrap(),
        )
        .unwrap();
        std::fs::hard_link(root.join("p1/c2.mtclean.d/pages/001.png"), root.join("p1/link-of-001.png")).unwrap();
    }

    fn good(root: &Path) -> Pointer {
        Pointer { path: root.to_path_buf(), folder_id: ensure_marker(root).unwrap() }
    }

    fn request<'a>(s: &'a Scratch, pointer_file: &'a Path, from: &'a Path, to: &'a Path, force_copy: bool) -> MoveRequest<'a> {
        MoveRequest {
            state: &s.0,
            pointer_file,
            kind: Kind::Move,
            from,
            to,
            pointer: Pointer { path: to.to_path_buf(), folder_id: "id".into() },
            force_copy,
            verify_pages: true,
            rename_only: false,
        }
    }

    #[test]
    fn a_copy_to_a_volume_without_links_copies_each_name_and_skips_apple_double_files() {
        let s = Scratch::new("nolinks");
        let from = s.0.join("old");
        tree(&from);
        let to = s.0.join("new");
        copy_tree(&from, &to, false, &mut Default::default()).unwrap();
        let page = to.join("p1/c2.mtclean.d/pages/001.png");
        let copy = to.join("p1/link-of-001.png");
        assert_eq!(std::fs::read(&page).unwrap(), std::fs::read(&copy).unwrap());
        assert!(file_id(&page, &std::fs::metadata(&page).unwrap()).is_none(), "copied, not linked");
        // What macOS leaves on exFAT: not part of the library.
        std::fs::write(to.join("p1/._c2.mtclean"), b"xattrs").unwrap();
        verify_copy(&from, &to, false).unwrap();
        assert!(verify_copy(&from, &to, true).is_err(), "where links work, a lost link is refused");
        assert!(copy_bytes(&from, false, 4096) > copy_bytes(&from, true, 4096), "each name is counted");
        assert_eq!(copy_bytes(&from, true, 1 << 17) % (1 << 17), 0, "rounded to the cluster");
    }

    #[test]
    fn a_release_build_and_a_test_build_have_their_own_pointers() {
        if std::env::var_os(POINTER_ENV).is_some() {
            return;
        }
        let state = Path::new("/state");
        assert_eq!(pointer_path(state, "com.mangacleaner.studio"), state.join("MangaLibrary/data-folder.json"));
        assert_eq!(pointer_path(state, "com.mangatypesetter.app"), state.join("MangaLibrary/data-folder.json"));
        assert_eq!(pointer_path(state, "com.mangacleaner.next"), state.join("MangaLibrary-dev/data-folder.json"));
    }

    #[test]
    fn a_pointer_round_trips_and_a_strange_one_is_never_taken_for_legacy() {
        let s = Scratch::new("pointer");
        let file = s.0.join("state/MangaLibrary/data-folder.json");
        assert_eq!(read_pointer(&file), PointerRead::None);
        let pointer = Pointer { path: s.0.join("Manga Library"), folder_id: "f1".into() };
        write_pointer(&file, &pointer).unwrap();
        assert_eq!(read_pointer(&file), PointerRead::Good(pointer));
        for body in [&br#"{"schema":2,"path":"/x","folderId":"a"}"#[..], br#"{"schema":1,"path":"rel/x","folderId":"a"}"#, br#"{"schema":1,"path":"/x"}"#, b"garbage"] {
            std::fs::write(&file, body).unwrap();
            assert!(matches!(read_pointer(&file), PointerRead::Unknown(_)), "{}", String::from_utf8_lossy(body));
        }
    }

    #[test]
    fn the_library_is_legacy_pending_shared_or_unavailable() {
        let s = Scratch::new("resolve");
        let legacy = s.0.join("appdata/library");
        let root = s.0.join("shared");
        assert_eq!(resolve(PointerRead::None, &legacy).mode, Mode::Legacy);
        assert_eq!(resolve(PointerRead::None, &legacy).library, Some(legacy.clone()));
        // Unplugged: nothing at the path.
        let missing = Pointer { path: root.clone(), folder_id: "x".into() };
        let r = resolve(PointerRead::Good(missing.clone()), &legacy);
        assert_eq!((r.mode, r.library), (Mode::Unavailable, None));
        assert!(!root.exists(), "resolving creates nothing");
        // A folder, but no marker: an unmounted mount point looks like this.
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(resolve(PointerRead::Good(missing), &legacy).mode, Mode::Unavailable);
        // Another data folder at the same path.
        let pointer = good(&root);
        let other = Pointer { path: root.clone(), folder_id: "someone else".into() };
        assert_eq!(resolve(PointerRead::Good(other), &legacy).mode, Mode::Unavailable);
        // Newer schema anywhere.
        assert_eq!(resolve(PointerRead::Unknown("newer".into()), &legacy).mode, Mode::Unavailable);
        // Good pointer and nothing to move: still Pending (version 2). The
        // start flows ask to Join; this read never makes `Cleaner/`.
        let r = resolve(PointerRead::Good(pointer.clone()), &legacy);
        assert_eq!((r.mode, r.library), (Mode::Pending, Some(legacy.clone())));
        assert!(!root.join("Cleaner").exists(), "resolving creates nothing");
        // Pear set it up while this library is still at home.
        tree(&legacy);
        let r = resolve(PointerRead::Good(pointer.clone()), &legacy);
        assert_eq!((r.mode, r.library), (Mode::Pending, Some(legacy.clone())));
        // An empty subfolder is "no subfolder yet".
        std::fs::create_dir_all(root.join("Cleaner")).unwrap();
        std::fs::write(root.join("Cleaner/.DS_Store"), b"").unwrap();
        std::fs::write(root.join("Cleaner/index.json.lock"), b"").unwrap();
        assert_eq!(resolve(PointerRead::Good(pointer.clone()), &legacy).mode, Mode::Pending);
        // Something there, but no library file: damaged, Unavailable.
        std::fs::create_dir_all(root.join("Cleaner/p1")).unwrap();
        let r = resolve(PointerRead::Good(pointer.clone()), &legacy);
        assert_eq!((r.mode, r.library), (Mode::Unavailable, None));
        assert_eq!(r.reason.as_deref(), Some("The Cleaner folder inside the data folder is damaged."));
        // Its library file: Shared.
        crate::library::Library::at(root.join("Cleaner")).ensure_index().unwrap();
        let r = resolve(PointerRead::Good(pointer), &legacy);
        assert_eq!((r.mode, r.library), (Mode::Shared, Some(root.join("Cleaner"))));
    }

    #[test]
    fn the_sister_test_reads_each_source_and_the_override() {
        let s = Scratch::new("sister");
        let home = s.0.join("home");
        let (data, local, config) = (s.0.join("data"), s.0.join("local"), s.0.join("config"));
        let pointer = s.0.join("state/MangaLibrary/data-folder.json");
        let paths = sister_paths(&pointer, &[data.clone(), local.clone(), config.clone()], Some(&home));
        assert!(!sister_present_at(&paths, None), "nothing has run");
        assert!(sister_present_at(&paths, Some("1")));
        let mut sources = vec![
            pointer.clone(),
            data.join(SISTER_ID),
            local.join(SISTER_ID),
            config.join(SISTER_ID),
            home.join("Documents/MangaTypesetter"),
        ];
        if cfg!(target_os = "macos") {
            sources.push(home.join("Library/WebKit").join(SISTER_ID));
        }
        assert_eq!(paths.len(), sources.len());
        for source in &sources {
            assert!(paths.contains(source), "{}", source.display());
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            if source == &pointer {
                std::fs::write(source, b"{}").unwrap();
            } else {
                std::fs::create_dir_all(source).unwrap();
            }
            assert!(sister_present_at(&paths, None), "{}", source.display());
            assert!(!sister_present_at(&paths, Some("0")), "the override wins");
            if source == &pointer {
                std::fs::remove_file(source).unwrap();
            } else {
                std::fs::remove_dir_all(source).unwrap();
            }
        }
        assert!(!sister_present_at(&paths, None));
        assert!(!s.0.join("data").join(SISTER_ID).exists(), "the check creates nothing");
    }

    fn quiet<'a>(legacy: &'a Path, default_root: &'a Path, home: &'a Path) -> QuietCheck<'a> {
        QuietCheck { legacy, default_root, home: Some(home), jobs_running: false }
    }

    #[test]
    fn a_quiet_setup_runs_only_when_every_check_holds() {
        let s = Scratch::new("quiet");
        let home = s.0.join("home");
        let legacy = s.0.join("appdata/library");
        let default_root = home.join(DEFAULT_NAME);
        tree(&legacy);
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(quiet_refusal(&quiet(&legacy, &default_root, &home)), None, "absent default place, one volume");
        std::fs::create_dir_all(&default_root).unwrap();
        std::fs::write(default_root.join(".DS_Store"), b"").unwrap();
        assert_eq!(quiet_refusal(&quiet(&legacy, &default_root, &home)), None, "an empty folder is fine");
        std::fs::write(default_root.join("notes.txt"), b"x").unwrap();
        assert!(quiet_refusal(&quiet(&legacy, &default_root, &home)).is_some(), "the default place has files");
        std::fs::remove_file(default_root.join("notes.txt")).unwrap();
        assert!(quiet_refusal(&QuietCheck { jobs_running: true, ..quiet(&legacy, &default_root, &home) }).is_some());
        // A library in a synced folder may move out of it (contract 5.2).
        let synced = home.join("Library/CloudStorage/Dropbox/library");
        tree(&synced);
        assert_eq!(quiet_refusal(&quiet(&synced, &default_root, &home)), None, "a synced library moves out");
        let synced_target = home.join("Dropbox").join(DEFAULT_NAME);
        assert!(quiet_refusal(&quiet(&legacy, &synced_target, &home)).is_some(), "a synced default place");
        // No library yet: nothing to check about volumes or the cloud.
        assert_eq!(quiet_refusal(&quiet(&s.0.join("none"), &default_root, &home)), None);
    }

    #[test]
    fn an_empty_cleaner_folder_is_filled_and_then_shared() {
        let s = Scratch::new("fill-empty");
        let root = s.0.join("Manga Library");
        let at = good(&root);
        let legacy = s.0.join("legacy");
        let pointer = || PointerRead::Good(at.clone());
        // No subfolder at all: nothing to fill, still Pending.
        assert!(!fill_empty_own(&root).unwrap());
        assert!(!root.join(CLEANER_DIR).exists());
        // An empty one (Finder's file aside) is filled with its index.
        std::fs::create_dir_all(root.join(CLEANER_DIR)).unwrap();
        std::fs::write(root.join(CLEANER_DIR).join(".DS_Store"), b"").unwrap();
        assert!(own_is_empty(&root));
        assert_eq!(resolve(pointer(), &legacy).mode, Mode::Pending);
        assert!(fill_empty_own(&root).unwrap());
        assert!(root.join(CLEANER_DIR).join(crate::library::INDEX_FILE).is_file());
        assert_eq!(resolve(pointer(), &legacy).mode, Mode::Shared);
        // Filled once: a second call changes nothing.
        assert!(!fill_empty_own(&root).unwrap());
        // One with something else in it is damaged, never filled.
        let other = s.0.join("Other");
        good(&other);
        std::fs::create_dir_all(other.join(CLEANER_DIR)).unwrap();
        std::fs::write(other.join(CLEANER_DIR).join("page.png"), b"x").unwrap();
        assert!(!fill_empty_own(&other).unwrap());
        assert!(!other.join(CLEANER_DIR).join(crate::library::INDEX_FILE).exists());
    }

    #[test]
    #[cfg(unix)]
    fn volumes_are_told_apart() {
        let s = Scratch::new("volume");
        assert!(same_volume(&s.0, &s.0.join("not/yet/there")));
        // `/dev` is its own file system on macOS and Linux.
        assert!(!same_volume(&s.0, Path::new("/dev")));
    }

    #[test]
    fn a_rename_only_move_that_would_copy_changes_nothing() {
        let s = Scratch::new("rename-only");
        let from = s.0.join("old");
        tree(&from);
        let to = s.0.join("new");
        let pointer_file = s.0.join("data-folder.json");
        let refused = run_move(MoveRequest { rename_only: true, ..request(&s, &pointer_file, &from, &to, true) });
        assert!(refused.is_err());
        assert!(from.join("index.json").exists() && !to.exists() && !partial_of(&to).exists());
        assert_eq!(read_pointer(&pointer_file), PointerRead::None);
        assert!(read_journal(&s.0).unwrap().is_none());
    }

    #[test]
    fn a_setup_target_must_be_empty_or_a_data_folder_with_no_library() {
        let s = Scratch::new("target");
        assert!(check_setup_target(&s.0.join("absent")).is_ok());
        let used = s.0.join("used");
        std::fs::create_dir_all(&used).unwrap();
        std::fs::write(used.join("x"), b"x").unwrap();
        assert!(check_setup_target(&used).is_err());
        let folder = s.0.join("folder");
        good(&folder);
        assert!(check_setup_target(&folder).is_ok());
        crate::library::Library::at(folder.join(CLEANER_DIR)).ensure_index().unwrap();
        assert!(check_setup_target(&folder).is_err(), "Cleaner/ is taken");
    }

    #[test]
    fn os_errors_read_as_plain_words_and_paths_lose_the_long_prefix() {
        use std::io::{Error, ErrorKind};
        assert_eq!(plain_words(&Error::from(ErrorKind::StorageFull)), "There is not enough free space on that drive.");
        assert_eq!(plain_words(&Error::from(ErrorKind::PermissionDenied)), "This app is not allowed to write there.");
        assert_eq!(plain_words(&Error::from(ErrorKind::ResourceBusy)), "A file is in use by another app.");
        assert_eq!(plain_words(&Error::other("x")), "The move could not finish.");
        if cfg!(unix) {
            assert_eq!(plain_words(&Error::from_raw_os_error(28)), "There is not enough free space on that drive.");
            assert_eq!(plain_words(&Error::from_raw_os_error(6)), "The drive was disconnected.");
        }
        let raw = Error::other("Os { code: 28, kind: StorageFull }");
        assert!(!plain_io(Path::new("/x"), &raw).contains("Os {"), "raw text goes to the log only");
        assert_eq!(shown_text(r"\\?\C:\Manga Library"), r"C:\Manga Library");
        assert_eq!(shown_text(r"\\?\UNC\server\share\Manga"), r"\\server\share\Manga");
        assert_eq!(shown_text("/Users/me/Manga Library"), "/Users/me/Manga Library");
    }

    #[test]
    fn titles_count_the_index_projects() {
        let s = Scratch::new("titles");
        assert_eq!(titles_in(&s.0), 0);
        std::fs::write(s.0.join("index.json"), br#"{"version":1,"projects":[{"id":"p1","name":"A","mode":"single","created":0,"last_opened":0}]}"#).unwrap();
        assert_eq!(titles_in(&s.0), 1);
    }

    #[test]
    fn a_move_on_one_volume_is_one_rename_and_ends_with_the_pointer() {
        let s = Scratch::new("rename");
        let from = s.0.join("old");
        tree(&from);
        let to = s.0.join("new/Manga Library");
        let pointer_file = s.0.join("data-folder.json");
        run_move(request(&s, &pointer_file, &from, &to, false)).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read(to.join("p1/c2.mtclean.d/pages/001.png")).unwrap().len(), 5000);
        assert!(matches!(read_pointer(&pointer_file), PointerRead::Good(p) if p.path == to));
        assert!(read_journal(&s.0).unwrap().is_none());
        assert!(old_copies(&s.0).is_empty());
    }

    #[test]
    fn a_move_across_volumes_keeps_links_checks_pages_and_sets_the_old_tree_aside() {
        let s = Scratch::new("copy");
        let from = s.0.join("old");
        tree(&from);
        let to = s.0.join("new");
        let pointer_file = s.0.join("data-folder.json");
        run_move(request(&s, &pointer_file, &from, &to, true)).unwrap();
        assert!(!from.exists(), "set aside, not left in place");
        let old = old_copies(&s.0);
        assert_eq!(old.len(), 1);
        assert!(old[0].path.file_name().unwrap().to_string_lossy().starts_with("old.moved-"));
        assert_eq!(old[0].bytes, tree_bytes(&to));
        assert!(!s.0.join("new.partial").exists());
        let (a, b) = (to.join("p1/c2.mtclean.d/pages/001.png"), to.join("p1/link-of-001.png"));
        let id = |p: &Path| file_id(p, &std::fs::metadata(p).unwrap());
        assert!(id(&a).is_some(), "the copy is still linked");
        assert_eq!(id(&a), id(&b), "the hard link survived the copy");
    }

    #[test]
    fn a_copy_that_does_not_match_its_page_hash_changes_nothing() {
        let s = Scratch::new("badhash");
        let from = s.0.join("old");
        tree(&from);
        // The manifest promises other bytes than the page has.
        std::fs::write(from.join("p1/c2.mtclean"), br#"{"sources":[{"rel_path":"c2.mtclean.d/pages/001.png","sha256":"00"}]}"#).unwrap();
        let to = s.0.join("new");
        let pointer_file = s.0.join("data-folder.json");
        assert!(run_move(request(&s, &pointer_file, &from, &to, true)).is_err());
        assert!(from.join("index.json").exists());
        assert!(!to.exists() && !s.0.join("new.partial").exists());
        assert_eq!(read_pointer(&pointer_file), PointerRead::None);
        assert!(read_journal(&s.0).unwrap().is_none());
    }

    #[test]
    fn a_move_refuses_a_full_target_and_a_target_inside_itself() {
        let s = Scratch::new("refuse");
        let from = s.0.join("old");
        tree(&from);
        let full = s.0.join("full");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::write(full.join("x"), b"x").unwrap();
        let pointer_file = s.0.join("data-folder.json");
        assert!(run_move(request(&s, &pointer_file, &from, &full, false)).is_err());
        let inner = from.join("inner");
        assert!(run_move(request(&s, &pointer_file, &from, &inner, false)).is_err());
        assert!(from.join("index.json").exists());
    }

    fn journal(from: &Path, to: &Path, pointer_file: &Path, method: Method, phase: Phase) -> Journal {
        Journal {
            schema: 1,
            app: "pear".into(),
            kind: Kind::Move,
            method,
            from: from.to_path_buf(),
            to: to.to_path_buf(),
            pointer_file: pointer_file.to_path_buf(),
            pointer: JournalPointer { path: to.to_path_buf(), folder_id: "id".into() },
            started_at: rfc3339_now(),
            phase,
        }
    }

    #[test]
    fn recovery_undoes_an_unfinished_copy_including_a_finished_copy_not_yet_recorded() {
        let s = Scratch::new("recover-copy");
        let from = s.0.join("old");
        tree(&from);
        let to = s.0.join("new");
        // The copy was renamed into place, but the journal still says copying.
        copy_tree(&from, &to, true, &mut Default::default()).unwrap();
        std::fs::create_dir_all(partial_of(&to)).unwrap();
        let pointer_file = s.0.join(POINTER_FILE);
        write_journal(&s.0, &journal(&from, &to, &pointer_file, Method::Copy, Phase::Copying)).unwrap();
        assert!(recover(&s.0).unwrap().is_some());
        assert!(!partial_of(&to).exists() && !to.exists(), "no second folder claims to be the data folder");
        assert!(from.join("index.json").exists());
        assert_eq!(read_pointer(&pointer_file), PointerRead::None);
        assert!(read_journal(&s.0).unwrap().is_none());
    }

    #[test]
    fn recovery_finishes_a_copied_move_and_a_rename_written_by_the_other_app() {
        let s = Scratch::new("recover-copied");
        let (from, to) = (s.0.join("old"), s.0.join("new"));
        tree(&from);
        copy_tree(&from, &to, true, &mut Default::default()).unwrap();
        let pointer_file = s.0.join(POINTER_FILE);
        write_journal(&s.0, &journal(&from, &to, &pointer_file, Method::Copy, Phase::Copied)).unwrap();
        recover(&s.0).unwrap();
        assert!(matches!(read_pointer(&pointer_file), PointerRead::Good(p) if p.path == to));
        assert!(!from.exists(), "the old tree is set aside");
        assert_eq!(old_copies(&s.0).len(), 1);
        assert!(read_journal(&s.0).unwrap().is_none());

        let s2 = Scratch::new("recover-rename");
        let (from2, to2) = (s2.0.join("old"), s2.0.join("new"));
        tree(&to2);
        let pointer2 = s2.0.join(POINTER_FILE);
        write_journal(&s2.0, &journal(&from2, &to2, &pointer2, Method::Rename, Phase::Copying)).unwrap();
        recover(&s2.0).unwrap();
        assert!(matches!(read_pointer(&pointer2), PointerRead::Good(p) if p.path == to2));
        assert!(to2.exists());
    }

    #[test]
    fn recovery_reads_the_contract_json_and_never_deletes_a_journal_it_cannot_read() {
        let s = Scratch::new("recover-foreign");
        // Exactly the contract's field names, as Pear writes them.
        let from = s.0.join("old");
        tree(&from);
        let body = serde_json::json!({
            "schema": 1, "app": "pear", "kind": "move", "method": "rename",
            "from": from, "to": s.0.join("new"), "pointerFile": s.0.join(POINTER_FILE),
            "pointer": { "path": s.0.join("new"), "folderId": "id" },
            "startedAt": "2026-11-25T10:00:00.000Z", "phase": "copying"
        });
        std::fs::write(s.0.join(JOURNAL), serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(recover(&s.0).unwrap().is_some(), "nothing moved: the journal is dropped");
        assert!(!s.0.join(JOURNAL).exists());
        for strange in [&br#"{"schema":2}"#[..], br#"{"schema":1,"kind":"teleport"}"#, b"not json"] {
            std::fs::write(s.0.join(JOURNAL), strange).unwrap();
            assert!(recover(&s.0).is_err());
            assert_eq!(std::fs::read(s.0.join(JOURNAL)).unwrap(), strange, "left exactly as it was");
        }
    }

    #[test]
    fn recovery_waits_while_the_typesetter_holds_its_lock_in_either_folder() {
        let s = Scratch::new("recover-wait");
        let (from, to) = (s.0.join("old"), s.0.join("new"));
        tree(&from);
        copy_tree(&from, &to, true, &mut Default::default()).unwrap();
        std::fs::create_dir_all(from.join(LOCKS_DIR)).unwrap();
        let held = cleaner_core::project::lock::try_exclusive(&from.join(LOCKS_DIR).join(TYPESETTER_LOCK)).unwrap();
        let pointer_file = s.0.join(POINTER_FILE);
        write_journal(&s.0, &journal(&from, &to, &pointer_file, Method::Copy, Phase::Copied)).unwrap();
        assert!(recover(&s.0).is_err());
        assert!(from.exists() && s.0.join(JOURNAL).exists(), "nothing set aside while the typesetter works there");
        drop(held);
        recover(&s.0).unwrap();
        assert!(!s.0.join(JOURNAL).exists());
    }

    #[test]
    fn an_old_copy_in_use_is_never_deleted() {
        let s = Scratch::new("old-copy");
        let live = s.0.join("Manga Library");
        good(&live);
        let old = s.0.join("Manga Library.moved-x");
        std::fs::create_dir_all(&old).unwrap();
        // A moved copy keeps the folder id; that alone must not block it.
        std::fs::copy(live.join(MARKER), old.join(MARKER)).unwrap();
        assert!(old_copy_refusal(&old, std::slice::from_ref(&live)).is_none());
        assert!(old_copy_refusal(&live, std::slice::from_ref(&live)).is_some());
        assert!(old_copy_refusal(&s.0, std::slice::from_ref(&live)).is_some(), "holds the live folder");
        assert!(old_copy_refusal(&live.join("Cleaner"), std::slice::from_ref(&live)).is_some());
        // After an unplug and "use existing" on the old copy, it is the live folder.
        assert!(old_copy_refusal(&old, std::slice::from_ref(&old)).is_some());
    }

    #[test]
    fn two_first_asks_for_the_library_id_agree() {
        let s = Scratch::new("library-id");
        let ids: Vec<_> = (0..8)
            .map(|_| {
                let root = s.0.clone();
                std::thread::spawn(move || library_id(&root).unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert!(ids.windows(2).all(|w| w[0] == w[1]), "{ids:?}");
    }

    #[test]
    fn the_move_lock_shuts_out_a_second_mover() {
        let s = Scratch::new("movelock");
        let held = take_move_lock(&s.0).unwrap();
        assert!(take_move_lock(&s.0).is_err());
        drop(held);
        assert!(take_move_lock(&s.0).is_ok());
    }

    #[test]
    fn a_held_lock_reads_as_running() {
        let s = Scratch::new("lock");
        std::fs::create_dir_all(s.0.join(LOCKS_DIR)).unwrap();
        assert_eq!(typesetter_running(&s.0), Running::No);
        let held = cleaner_core::project::lock::try_exclusive(&s.0.join(LOCKS_DIR).join(TYPESETTER_LOCK)).unwrap();
        assert!(held.is_some());
        // flock is per open file description, so a second open in this
        // process contends like another process would.
        assert_eq!(typesetter_running(&s.0), Running::Yes);
        drop(held);
        assert_eq!(typesetter_running(&s.0), Running::No);
    }

    #[test]
    fn origins_outside_the_library_become_absolute_and_inside_stay_relative() {
        use cleaner_core::project::{Project, StripMode};
        let s = Scratch::new("origins");
        let raws = s.0.join("scans/ch1");
        std::fs::create_dir_all(&raws).unwrap();
        let bytes = cleaner_core::image::encode(&cleaner_core::image::fixtures::by_name("l8").raster, cleaner_core::image::Format::Png).unwrap();
        std::fs::write(raws.join("001.png"), &bytes).unwrap();
        let source = cleaner_core::ingest::source_ref(&raws.join("001.png"), &bytes).unwrap();
        let manifest_dir = s.0.join("appdata/library/p1");
        std::fs::create_dir_all(&manifest_dir).unwrap();
        let mut project = Project::new(&manifest_dir, "0.1.0", StripMode::Single, &[source]);
        project.sources[0].converted_from = Some(PathBuf::from("../../../scans/ch1/001.png"));
        assert!(project.sources[0].rel_path.starts_with(".."), "an old-style manifest reads the scan folder");
        let library = s.0.join("appdata/library");
        assert!(absolutize_sources(&mut project, &manifest_dir, &library));
        assert_eq!(project.sources[0].converted_from.as_deref(), Some(raws.join("001.png").as_path()));
        assert_eq!(project.sources[0].rel_path, raws.join("001.png"));
        assert!(!absolutize_sources(&mut project, &manifest_dir, &library), "idempotent");
        project.sources[0].rel_path = PathBuf::from("c1.mtclean.d/pages/001.png");
        assert!(!absolutize_sources(&mut project, &manifest_dir, &library), "inside the library stays relative");
        // A whole-folder move: a path into a sibling of the library inside the
        // folder stays relative, because it moves too.
        project.sources[0].converted_from = Some(PathBuf::from("../../Typesetter/x/raws/001.png"));
        assert!(!absolutize_sources(&mut project, &manifest_dir, &s.0.join("appdata")));
        assert!(absolutize_sources(&mut project, &manifest_dir, &library), "but it leaves the library alone");
        // Denoise rows too.
        project.denoised.push(cleaner_core::project::DenoisedPage { source: Some(PathBuf::from("../../../scans/ch1/001.png")), ..Default::default() });
        assert!(absolutize_sources(&mut project, &manifest_dir, &library));
        assert_eq!(project.denoised[0].source.as_deref(), Some(raws.join("001.png").as_path()));
    }

    #[test]
    fn synced_folders_are_recognised() {
        let home = Path::new("/Users/me");
        assert!(in_synced_folder(Path::new("/Users/me/Library/Mobile Documents/com~apple~CloudDocs/x"), Some(home)));
        assert!(in_synced_folder(Path::new("/Users/me/Library/CloudStorage/Dropbox/x"), Some(home)));
        assert!(in_synced_folder(Path::new("C:\\Users\\me\\OneDrive\\Manga"), None));
        assert!(!in_synced_folder(Path::new("/Users/me/Manga Library"), Some(home)));
        assert_eq!(in_synced_folder(Path::new("/Users/me/Documents/x"), Some(home)), cfg!(target_os = "macos"));
    }

    #[test]
    fn uuids_are_version_4() {
        let id = new_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_uuid());
    }

    #[test]
    fn rfc3339_matches_known_dates() {
        assert_eq!(rfc3339(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_764_064_800, 5), "2025-11-25T10:00:00.005Z");
        assert_eq!(rfc3339(951_782_400, 0), "2000-02-29T00:00:00.000Z");
    }
}
