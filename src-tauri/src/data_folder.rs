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
    path.to_str().ok_or_else(|| format!("{} has characters this app cannot store. Pick a folder with a plainer name.", path.display()))
}

fn has_content(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
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
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": 1,
        "path": path_str(&pointer.path)?,
        "folderId": pointer.folder_id,
    }))
    .map_err(|e| e.to_string())?;
    write_atomic(path, &body).map_err(|e| format!("{}: {e}", path.display()))
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
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let id = new_uuid();
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": 1,
        "folderId": id,
        "createdBy": "manga-cleaner",
        "createdAt": rfc3339_now(),
    }))
    .map_err(|e| e.to_string())?;
    write_atomic(&root.join(MARKER), &body).map_err(|e| format!("{}: {e}", root.display()))?;
    Ok(id)
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
    let id = new_uuid();
    let body = serde_json::to_vec_pretty(&serde_json::json!({ "schema": 1, "libraryId": id })).ok()?;
    write_atomic(&path, &body).ok()?;
    Some(id)
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
        return unavailable(Some(pointer), format!("The data folder is not at {}. Reconnect the drive, or choose another folder.", root.display()));
    }
    match read_marker(&root) {
        None => return unavailable(Some(pointer), format!("{} is not a data folder any more (its {MARKER} is missing).", root.display())),
        Some((schema, _)) if schema != 1 => return unavailable(Some(pointer), "The data folder was set up by a newer version. Update this app.".into()),
        Some((_, id)) if id != pointer.folder_id => {
            return unavailable(Some(pointer), format!("{} is a different data folder than the one this computer set up.", root.display()));
        }
        Some(_) => {}
    }
    let own = root.join(CLEANER_DIR);
    if own.is_dir() || !has_content(&legacy) {
        Resolved { mode: Mode::Shared, pointer: Some(pointer), library: Some(own), legacy, reason: None }
    } else {
        Resolved { mode: Mode::Pending, pointer: Some(pointer), library: Some(legacy.clone()), legacy, reason: None }
    }
}

static RESOLVED: RwLock<Option<Resolved>> = RwLock::new(None);
/// `locks/cleaner.lock`, held while this process runs with a shared folder.
static HELD: Mutex<Option<File>> = Mutex::new(None);

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
/// app's lock to match. Shared mode with no `Cleaner/` yet (nothing was
/// there to move) creates it: the marker has just proved the folder is the
/// right one, so this is not the implicit creation the contract forbids.
pub fn refresh(app: &tauri::AppHandle) -> Result<Resolved, String> {
    let resolved = resolve(read_pointer(&pointer_file(app)?), &legacy_dir(app)?);
    if resolved.mode == Mode::Shared {
        if let Some(own) = &resolved.library {
            let _ = std::fs::create_dir(own);
        }
    }
    *RESOLVED.write().unwrap_or_else(|p| p.into_inner()) = Some(resolved.clone());
    hold_lock(if resolved.mode == Mode::Shared { resolved.root() } else { None });
    Ok(resolved)
}

fn hold_lock(root: Option<&Path>) {
    let mut held = HELD.lock().unwrap_or_else(|p| p.into_inner());
    *held = None;
    let Some(root) = root else { return };
    let dir = root.join(LOCKS_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    match cleaner_core::project::lock::try_exclusive(&dir.join(OWN_LOCK)) {
        Ok(Some(file)) => *held = Some(file),
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
    let resolved = current(app)?;
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
    std::fs::create_dir_all(state).map_err(|e| format!("{}: {e}", state.display()))?;
    match cleaner_core::project::lock::try_exclusive(&state.join(MOVE_LOCK)) {
        Ok(Some(file)) => Ok(MoveLock(file)),
        Ok(None) => Err("The typesetter is moving the data folder right now. Try again when it is done.".into()),
        Err(error) => Err(format!("{}: {error}", state.join(MOVE_LOCK).display())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Copying,
    Copied,
    Switched,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub schema: u32,
    pub app: String,
    pub from: PathBuf,
    pub to: PathBuf,
    /// The pointer file, and what it says once the move is done.
    pub pointer_file: PathBuf,
    pub pointer_path: PathBuf,
    pub folder_id: String,
    pub started_at: String,
    pub phase: Phase,
    /// The move went by copy, so the old tree is to be set aside.
    #[serde(default)]
    pub copied: bool,
}

fn write_journal(state: &Path, journal: &Journal) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(journal).map_err(|e| e.to_string())?;
    write_atomic(&state.join(JOURNAL), &body).map_err(|e| format!("{}: {e}", state.display()))
}

fn read_journal(state: &Path) -> Option<Journal> {
    serde_json::from_slice(&std::fs::read(state.join(JOURNAL)).ok()?).ok()
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
    std::fs::rename(from, &aside).map_err(|e| format!("{}: {e}", from.display()))?;
    let mut list = old_copies(state);
    list.push(OldCopy { bytes: tree_bytes(&aside), path: aside, moved_at: rfc3339_now() });
    save_old_copies(state, &list)
}

/// Finish or undo whatever move a crash interrupted. Run at start, before
/// anything reads the library. Idempotent.
pub fn recover(state: &Path) -> Result<Option<String>, String> {
    let Some(mut journal) = read_journal(state) else { return Ok(None) };
    let _lock = take_move_lock(state)?;
    let pointer = Pointer { path: journal.pointer_path.clone(), folder_id: journal.folder_id.clone() };
    if journal.phase == Phase::Copying {
        if journal.to.exists() && !journal.from.exists() {
            // The one-rename move happened; only the pointer was missing.
            journal.phase = Phase::Copied;
        } else {
            let _ = std::fs::remove_dir_all(partial_of(&journal.to));
            let _ = std::fs::remove_file(state.join(JOURNAL));
            return Ok(Some(format!("A move of the data folder was stopped. Your data is still at {}.", journal.from.display())));
        }
    }
    if journal.phase == Phase::Copied {
        write_pointer(&journal.pointer_file, &pointer)?;
        journal.phase = Phase::Switched;
        write_journal(state, &journal)?;
    }
    if journal.copied {
        set_aside(state, &journal.from)?;
    }
    let _ = std::fs::remove_file(state.join(JOURNAL));
    Ok(Some(format!("A move of the data folder was finished. Your data is at {}.", journal.to.display())))
}

/// How a move is to be checked and run. `force_copy` is for tests, which
/// have one volume.
pub struct MoveRequest<'a> {
    pub state: &'a Path,
    pub pointer_file: &'a Path,
    pub from: &'a Path,
    pub to: &'a Path,
    pub pointer: Pointer,
    pub force_copy: bool,
    /// Only for the cleaner's own library: page hashes to check the copy by.
    pub verify_pages: bool,
}

/// Move `from` to `to`, journaled, ending with the pointer written. `to`
/// must not exist or be empty. One rename on one volume; across volumes a
/// copy into `<to>.partial` that keeps hard links and is fsynced and checked,
/// then renamed into place, and the old tree is set aside rather than
/// deleted. A failure before the pointer is written leaves `from` as it was.
pub fn run_move(request: MoveRequest) -> Result<(), String> {
    let MoveRequest { state, pointer_file, from, to, pointer, force_copy, verify_pages } = request;
    if !from.is_dir() {
        return Err(format!("{} is not a folder", from.display()));
    }
    if to.starts_with(from) {
        return Err("The new place is inside the old one.".into());
    }
    if to.exists() {
        if has_content(to) {
            return Err(format!("{} is not empty", to.display()));
        }
        std::fs::remove_dir(to).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut journal = Journal {
        schema: 1,
        app: "manga-cleaner".into(),
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        pointer_file: pointer_file.to_path_buf(),
        pointer_path: pointer.path.clone(),
        folder_id: pointer.folder_id.clone(),
        started_at: rfc3339_now(),
        phase: Phase::Copying,
        copied: false,
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
            Err(error) => return abandon(format!("{} -> {}: {error}", from.display(), to.display())),
        }
    };
    if !renamed {
        if let Err(error) = preflight_copy(from, to) {
            return abandon(error);
        }
        let partial = partial_of(to);
        let _ = std::fs::remove_dir_all(&partial);
        let copied = copy_tree(from, &partial, &mut Default::default())
            .and_then(|()| verify_copy(from, &partial))
            .and_then(|()| if verify_pages { verify_page_hashes(&partial) } else { Ok(()) })
            .and_then(|()| std::fs::rename(&partial, to).map_err(|e| format!("{}: {e}", to.display())));
        if let Err(error) = copied {
            return abandon(error);
        }
        journal.copied = true;
    }
    journal.phase = Phase::Copied;
    write_journal(state, &journal)?;
    write_pointer(pointer_file, &pointer)?;
    journal.phase = Phase::Switched;
    write_journal(state, &journal)?;
    if journal.copied {
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
fn preflight_copy(from: &Path, to: &Path) -> Result<(), String> {
    let offline = cloud_only_files(from);
    if offline > 0 {
        return Err(format!(
            "{offline} files in {} are only in the cloud. Download them first (in Finder or File Explorer), then try again.",
            from.display()
        ));
    }
    let need = tree_bytes(from);
    if let Some(free) = crate::weights::free_space(to) {
        let need_with_margin = need + need / 20;
        if free < need_with_margin {
            return Err(format!(
                "Not enough space at {}: the move needs {:.1} GB and {:.1} GB is free.",
                to.display(),
                need_with_margin as f64 / 1e9,
                free as f64 / 1e9
            ));
        }
    }
    Ok(())
}

/// A file id: device and inode, so hard links count once.
#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (meta.nlink() > 1).then(|| (meta.dev(), meta.ino()))
}

#[cfg(windows)]
fn file_id(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    // `MetadataExt::file_index` is unstable. A hard-linked file on Windows is
    // then copied, not linked: it costs space, never data.
    None
}

#[cfg(not(any(unix, windows)))]
fn file_id(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Bytes of a tree, each hard-linked file once.
fn tree_bytes(root: &Path) -> u64 {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    walk(root, &mut |_, meta| {
        if meta.is_file() && file_id(meta).is_none_or(|id| seen.insert(id)) {
            total += meta.len();
        }
    });
    total
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

/// Copy a tree: hard links kept (a file already copied under another name is
/// linked to its copy), symlinks as symlinks, every file and folder fsynced.
fn copy_tree(from: &Path, to: &Path, linked: &mut std::collections::HashMap<(u64, u64), PathBuf>) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&source).map_err(|e| format!("{}: {e}", source.display()))?;
        if meta.file_type().is_symlink() {
            copy_symlink(&source, &target)?;
        } else if meta.is_dir() {
            copy_tree(&source, &target, linked)?;
        } else {
            if let Some(id) = file_id(&meta) {
                if let Some(first) = linked.get(&id) {
                    std::fs::hard_link(first, &target).map_err(|e| format!("{}: {e}", target.display()))?;
                    continue;
                }
                linked.insert(id, target.clone());
            }
            std::fs::copy(&source, &target).map_err(|e| format!("{}: {e}", source.display()))?;
            File::open(&target).and_then(|f| f.sync_all()).map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    sync_dir(to);
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, target: &Path) -> Result<(), String> {
    let link = std::fs::read_link(source).map_err(|e| format!("{}: {e}", source.display()))?;
    std::os::unix::fs::symlink(link, target).map_err(|e| format!("{}: {e}", target.display()))
}

#[cfg(not(unix))]
fn copy_symlink(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::copy(source, target).map(|_| ()).map_err(|e| format!("{}: {e}", source.display()))
}

/// Same relative paths and sizes, and the same files linked together.
fn verify_copy(from: &Path, to: &Path) -> Result<(), String> {
    let (a, b) = (listing(from)?, listing(to)?);
    if a.files != b.files {
        let first = a.files.iter().find(|entry| !b.files.contains(entry)).or_else(|| b.files.iter().find(|e| !a.files.contains(e)));
        return Err(format!(
            "The copy does not match the original ({} vs {} files{}). Nothing was changed.",
            a.files.len(),
            b.files.len(),
            first.map(|(p, _)| format!(", first difference {}", p.display())).unwrap_or_default()
        ));
    }
    if cfg!(unix) && a.groups != b.groups {
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
        return Err(format!("{} is not a folder", root.display()));
    }
    walk(root, &mut |path, meta| {
        if meta.is_dir() {
            return;
        }
        let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
        if let Some(id) = file_id(meta) {
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
                return Err(format!("{} is missing from the copy. Nothing was changed.", path.display()));
            };
            let got = format!("{:x}", Sha256::digest(&bytes));
            if got != sha {
                return Err(format!("{} did not copy correctly. Nothing was changed.", path.display()));
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
    }
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
    match state_root(app).and_then(|state| recover(&state)) {
        Ok(Some(said)) => *RECOVERED.lock().unwrap_or_else(|p| p.into_inner()) = Some(said),
        Ok(None) => {}
        Err(error) => eprintln!("manga-cleaner: data folder recovery: {error}"),
    }
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

/// What every change needs first: nothing running here, and the typesetter
/// closed when a shared folder is involved.
fn ready_to_move(roots: &[&Path], confirmed_closed: bool) -> Result<(), String> {
    if crate::jobs::running() > 0 {
        return Err("Wait for the running jobs to finish first.".into());
    }
    for root in roots {
        match typesetter_running(root) {
            Running::Yes => return Err("Close the typesetter first. It is using the data folder.".into()),
            Running::CannotTell if !confirmed_closed => {
                return Err("typesetter_unknown: This drive cannot tell whether the typesetter is open. Close it, then confirm.".into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Before the cleaner library moves: every origin path that leaves it becomes
/// absolute, so the `..` chain from a manifest to the user's scan folder is
/// not broken by the move. Paths inside the library stay relative.
fn absolutize_origins(library: &Path) -> Result<(), String> {
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
            if absolutize_sources(&mut job.project, &path.parent().unwrap_or(library).to_path_buf()) {
                job.flush().map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// The rewrite itself. `true` when anything changed.
pub fn absolutize_sources(project: &mut cleaner_core::project::Project, manifest_dir: &Path) -> bool {
    let mut changed = false;
    let mut fix = |path: &mut PathBuf| {
        if path.is_relative() && path.components().next() == Some(std::path::Component::ParentDir) {
            *path = lexical_normalise(&manifest_dir.join(&*path));
            changed = true;
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
    changed
}

fn lexical_normalise(path: &Path) -> PathBuf {
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
/// `<target>/Cleaner`. Pending: the same into the existing folder.
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
                return Err("The typesetter already set up a data folder. Move this library into that one.".into());
            }
            _ => {}
        }
        if target.exists() && has_content(&target) && !is_data_folder(&target) {
            return Err(format!("{} is not empty. Pick an empty folder, or one that is already a data folder.", target.display()));
        }
        let own = target.join(CLEANER_DIR);
        if own.exists() && has_content(&own) {
            return Err(format!("{} already has a cleaner library.", own.display()));
        }
        // Only this app's own library moves, into a subfolder nothing else
        // writes, so the typesetter may stay open.
        ready_to_move(&[], confirmed_closed.unwrap_or(false))?;
        let folder_id = ensure_marker(&target)?;
        let pointer = Pointer { path: target.clone(), folder_id };
        if has_content(&resolved.legacy) {
            absolutize_origins(&resolved.legacy)?;
            run_move(MoveRequest {
                state: &state,
                pointer_file: &pointer_file(&app)?,
                from: &resolved.legacy,
                to: &own,
                pointer,
                force_copy: false,
                verify_pages: true,
            })?;
        } else {
            std::fs::create_dir_all(&own).map_err(|e| format!("{}: {e}", own.display()))?;
            write_pointer(&pointer_file(&app)?, &pointer)?;
        }
        let resolved = refresh(&app)?;
        Ok(status_of(&app, &resolved))
    })
    .await
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
        if let Some(library) = &resolved.library {
            absolutize_origins(library)?;
        }
        // Let go of the lock first: Windows will not rename a folder with an
        // open file inside it.
        release_lock();
        let moved = run_move(MoveRequest {
            state: &state,
            pointer_file: &pointer_file(&app)?,
            from: &root,
            to: &target,
            pointer: Pointer { path: target.clone(), folder_id: pointer.folder_id },
            force_copy: false,
            verify_pages: true,
        });
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
            return Err(format!("{} is not a data folder (it has no {MARKER}).", target.display()));
        };
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
        let copy = list.remove(at);
        std::fs::remove_dir_all(&copy.path).map_err(|e| format!("{}: {e}", copy.path.display()))?;
        save_old_copies(&state, &list)?;
        let resolved = refresh(&app)?;
        Ok(status_of(&app, &resolved))
    })
    .await
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
        #[cfg(unix)]
        std::fs::hard_link(root.join("p1/c2.mtclean.d/pages/001.png"), root.join("p1/link-of-001.png")).unwrap();
    }

    fn good(root: &Path) -> Pointer {
        Pointer { path: root.to_path_buf(), folder_id: ensure_marker(root).unwrap() }
    }

    fn request<'a>(s: &'a Scratch, pointer_file: &'a Path, from: &'a Path, to: &'a Path, force_copy: bool) -> MoveRequest<'a> {
        MoveRequest {
            state: &s.0,
            pointer_file,
            from,
            to,
            pointer: Pointer { path: to.to_path_buf(), folder_id: "id".into() },
            force_copy,
            verify_pages: true,
        }
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
        // Good pointer and nothing to move: shared.
        let r = resolve(PointerRead::Good(pointer.clone()), &legacy);
        assert_eq!((r.mode, r.library), (Mode::Shared, Some(root.join("Cleaner"))));
        // The typesetter set it up while this library is still at home.
        tree(&legacy);
        let r = resolve(PointerRead::Good(pointer.clone()), &legacy);
        assert_eq!((r.mode, r.library), (Mode::Pending, Some(legacy.clone())));
        std::fs::create_dir_all(root.join("Cleaner")).unwrap();
        assert_eq!(resolve(PointerRead::Good(pointer), &legacy).mode, Mode::Shared);
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
        assert!(read_journal(&s.0).is_none());
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let a = std::fs::metadata(to.join("p1/c2.mtclean.d/pages/001.png")).unwrap();
            let b = std::fs::metadata(to.join("p1/link-of-001.png")).unwrap();
            assert_eq!(a.ino(), b.ino(), "the hard link survived the copy");
        }
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
        assert!(read_journal(&s.0).is_none());
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

    #[test]
    fn recovery_undoes_an_unfinished_copy_and_finishes_a_finished_one() {
        // Crashed while copying: the partial copy goes, the data stays.
        let s = Scratch::new("recover");
        let from = s.0.join("old");
        tree(&from);
        let to = s.0.join("new");
        std::fs::create_dir_all(partial_of(&to)).unwrap();
        let pointer_file = s.0.join(POINTER_FILE);
        let mut journal = Journal {
            schema: 1, app: "manga-cleaner".into(), from: from.clone(), to: to.clone(),
            pointer_file: pointer_file.clone(), pointer_path: to.clone(), folder_id: "id".into(), started_at: rfc3339_now(),
            phase: Phase::Copying, copied: false,
        };
        write_journal(&s.0, &journal).unwrap();
        assert!(recover(&s.0).unwrap().is_some());
        assert!(!partial_of(&to).exists() && from.exists());
        assert_eq!(read_pointer(&pointer_file), PointerRead::None);
        // Crashed after the copy was renamed in, before the pointer.
        copy_tree(&from, &to, &mut Default::default()).unwrap();
        journal.phase = Phase::Copied;
        journal.copied = true;
        write_journal(&s.0, &journal).unwrap();
        recover(&s.0).unwrap();
        assert!(matches!(read_pointer(&pointer_file), PointerRead::Good(p) if p.path == to));
        assert!(!from.exists(), "the old tree is set aside");
        assert_eq!(old_copies(&s.0).len(), 1);
        assert!(read_journal(&s.0).is_none());
        // Crashed right after a one-rename move.
        let s2 = Scratch::new("recover-rename");
        let (from2, to2) = (s2.0.join("old"), s2.0.join("new"));
        tree(&to2);
        journal = Journal { from: from2, to: to2.clone(), pointer_file: s2.0.join(POINTER_FILE), pointer_path: to2.clone(), phase: Phase::Copying, copied: false, ..journal };
        write_journal(&s2.0, &journal).unwrap();
        recover(&s2.0).unwrap();
        assert!(matches!(read_pointer(&s2.0.join(POINTER_FILE)), PointerRead::Good(p) if p.path == to2));
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
        assert!(absolutize_sources(&mut project, &manifest_dir));
        assert_eq!(project.sources[0].converted_from.as_deref(), Some(raws.join("001.png").as_path()));
        assert_eq!(project.sources[0].rel_path, raws.join("001.png"));
        assert!(!absolutize_sources(&mut project, &manifest_dir), "idempotent");
        project.sources[0].rel_path = PathBuf::from("c1.mtclean.d/pages/001.png");
        assert!(!absolutize_sources(&mut project, &manifest_dir), "inside the library stays relative");
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
