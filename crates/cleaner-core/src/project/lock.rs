//! One writer per job, inside this process and across processes.
//!
//! A manifest is rewritten whole, so every write to one is a read-modify-write
//! over the entire file, and two that interleave lose the earlier one. A lock
//! inside one process closed that for threads. It did not close it for a
//! second process over the same library: the GUI and a headless runner once
//! did exactly that, and the GUI's save of a manifest it had read seven seconds
//! earlier took two committed cloud patches with it.
//!
//! So a job's lock is two locks taken in order:
//!
//! 1. **This process.** A slot per job, waited on through one condition
//!    variable. It also records which thread holds it, which is what lets
//!    [`Job::flush`](super::Job::flush) and the other writers tell a caller that
//!    already holds the job from one that does not.
//! 2. **Every other process.** An OS advisory lock on `<job>.lock`, the file
//!    beside the manifest (`flock` on unix, `LockFile` on Windows). The kernel
//!    drops it when the holder exits however it exits, so there is no owner to
//!    record and no staleness rule to get wrong. The lock is taken on a file of
//!    its own rather than on the manifest because the manifest is replaced by
//!    rename on every save, and a lock on a replaced file protects nothing.
//!
//! `libc` and `windows-sys` rather than `std::fs::File::lock`: the workspace
//! builds with Rust 1.82, `File::lock` needs 1.89, and the cloud attempt journal
//! already locks its files this way with these two crates.
//!
//! **Waiting on another process is bounded.** A process can hold a job for a
//! long time: a headless run holds its chapter from the first page to the last.
//! So a writer waits [`PATIENCE`] for one and then gives up with
//! [`LockError::Busy`], which the interface says as "another Manga Cleaner
//! process is using this chapter" instead of a command that never answers. A
//! run waits through [`lock_until`], with its own patience and its cancel.
//! Waiting on this process's own threads is not bounded: what they hold, and
//! for how long, is this process's own business.
//!
//! **A lock file goes with its job.** A lock is dropped with the file removed
//! when the manifest it guards is not there any more (a deleted chapter, a job
//! that was never made), so a library does not collect a `.lock` for every
//! chapter it once had. That is safe because a waiter that gets a lock checks
//! that the file it locked is still the one at the path, and starts again when
//! it is not.
//!
//! **Where it cannot lock**, because the lock file cannot be created (a
//! read-only volume, a job whose directory does not exist yet) or the file
//! system has no locks, the in-process half still holds and the gap is said on
//! stderr. Nothing can be written to such a job anyway, and the stale-manifest
//! check in [`Job::flush`](super::Job::flush) still refuses to overwrite a
//! manifest that moved.

use std::collections::HashMap;
use std::fs::File;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// How long [`lock`] waits for another process before giving up with
/// [`LockError::Busy`]. Long enough to outlast another process's single save
/// or edit, short enough that a command the user is waiting on answers.
pub const PATIENCE: Duration = Duration::from_secs(5);

/// How long a wait on another process goes before it is said on stderr.
const SAY_AFTER: Duration = Duration::from_secs(2);

/// How often a waiter looks again: at another process's lock, and at its own
/// `stop` while this process's other threads hold the job.
const POLL: Duration = Duration::from_millis(50);

/// Why a job was not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LockError {
    /// Another process held the job for longer than the caller would wait.
    /// The display starts with the code `job_busy`, which the interface reads
    /// wherever the message ends up.
    #[error("job_busy: another Manga Cleaner process is using {}", path.display())]
    Busy { path: PathBuf },
    /// The caller's `stop` answered first.
    #[error("stopped waiting for {}", path.display())]
    Stopped { path: PathBuf },
}

/// `<job>.lock`, beside the manifest: `c32.mtclean` is locked through
/// `c32.mtclean.lock`. Never deleted: a lock file removed while another
/// process waits on it would let a third lock a new file of the same name.
pub fn lock_path(job: &Path) -> PathBuf {
    let mut name = job.file_name().map(|name| name.to_os_string()).unwrap_or_default();
    name.push(".lock");
    job.with_file_name(name)
}

/// The jobs this process holds, keyed by [`key`], and the thread holding each.
fn held() -> &'static (Mutex<HashMap<PathBuf, ThreadId>>, Condvar) {
    static HELD: OnceLock<(Mutex<HashMap<PathBuf, ThreadId>>, Condvar)> = OnceLock::new();
    HELD.get_or_init(|| (Mutex::new(HashMap::new()), Condvar::new()))
}

fn registry() -> MutexGuard<'static, HashMap<PathBuf, ThreadId>> {
    held().0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One key per job however its path is spelled. The directory is resolved
/// because it exists for every job that can hold a manifest, which makes
/// `/tmp/x` and `/private/tmp/x` one job; a job whose directory does not exist
/// yet keys by its spelling, and nothing can have written it.
fn key(job: &Path) -> PathBuf {
    let (Some(parent), Some(name)) = (job.parent(), job.file_name()) else {
        return job.to_path_buf();
    };
    let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
    std::fs::canonicalize(parent)
        .map(|parent| parent.join(name))
        .unwrap_or_else(|_| job.to_path_buf())
}

/// A job held for writing, by this thread, against every other thread and
/// process. Both halves are released when it goes, the other processes' first.
/// The lock file is removed on the way when the job's manifest is gone.
///
/// Not `Send`, like a `MutexGuard`: the holder is recorded as the thread that
/// took it, and a lock carried to another thread would leave that thread's
/// writers waiting on a lock they already have.
pub struct JobLock {
    key: PathBuf,
    file: Option<File>,
    _thread: PhantomData<*const ()>,
}

impl JobLock {
    /// Whether the other processes are locked out too, rather than only this
    /// process's other threads. `false` only where the lock file could not be
    /// made or locked, which stderr has already reported.
    pub fn across_processes(&self) -> bool {
        self.file.is_some()
    }
}

impl Drop for JobLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // Removed while still locked, so nobody can be between finding the
            // file and locking it without the identity check in `attempt`
            // sending them round again. The manifest is looked for under the
            // lock too: a job created while this was held has one.
            if !self.key.exists() {
                let _ = std::fs::remove_file(lock_path(&self.key));
            }
            os::unlock(&file);
        }
        let (_, waiting) = held();
        registry().remove(&self.key);
        // Every waiter is woken because they are waiting on *different* jobs
        // through one condition variable, and the one this wakes may not be
        // the one this frees.
        waiting.notify_all();
    }
}

/// A lock refused to a command answers the command with the message, so the
/// `job_busy` code reaches the interface through a plain `?`.
impl From<LockError> for String {
    fn from(error: LockError) -> String {
        error.to_string()
    }
}

/// Take a job for writing, waiting for whichever thread has it and up to
/// [`PATIENCE`] for another process.
///
/// Not reentrant: a thread that already holds `job` and asks again would wait
/// on itself for ever, so it panics instead, naming the job.
pub fn lock(job: &Path) -> Result<JobLock, LockError> {
    lock_until(job, PATIENCE, &|| false)
}

/// [`lock`], waiting up to `patience` for another process, and giving up with
/// [`LockError::Stopped`] as soon as `stop` answers `true`. `stop` is asked
/// between looks, while this process's threads hold the job too, so it has to
/// be cheap and must not take a job itself: a cancel flag.
pub fn lock_until(job: &Path, patience: Duration, stop: &dyn Fn() -> bool) -> Result<JobLock, LockError> {
    let key = key(job);
    let me = std::thread::current().id();
    let (_, waiting) = held();
    let mut held = registry();
    loop {
        match held.get(&key) {
            None => break,
            Some(holder) if *holder == me => {
                panic!("{} is already held by this thread", job.display())
            }
            Some(_) => {
                if stop() {
                    return Err(LockError::Stopped { path: job.to_path_buf() });
                }
                held = match waiting.wait_timeout(held, POLL) {
                    Ok((held, _)) => held,
                    Err(poisoned) => poisoned.into_inner().0,
                };
            }
        }
    }
    held.insert(key.clone(), me);
    drop(held);
    // Registered first, so the only contention the file lock ever sees is
    // another process's: two threads of this one never both reach it.
    // Dropping `lock` on the way out of an error gives the slot back.
    let mut lock = JobLock { key, file: None, _thread: PhantomData };
    let wait = Wait { deadline: Instant::now() + patience, stop };
    lock.file = os_lock(job, Some(&wait))?;
    Ok(lock)
}

/// Take a job only if nobody has it, this thread included. `None` without
/// waiting when another thread or process holds it, or when the other
/// processes cannot be locked out.
pub fn try_lock(job: &Path) -> Option<JobLock> {
    let key = key(job);
    {
        let mut held = registry();
        if held.contains_key(&key) {
            return None;
        }
        held.insert(key.clone(), std::thread::current().id());
    }
    let mut lock = JobLock { key, file: None, _thread: PhantomData };
    // Dropping `lock` without a file gives the slot back.
    lock.file = Some(os_lock(job, None).ok().flatten()?);
    Some(lock)
}

/// The job for the rest of the caller's scope: `None` when this thread holds it
/// already, which is the ordinary case of a writer inside [`lock`], and the
/// lock itself otherwise. What [`Job`](super::Job)'s writers take, so a write
/// is never made from outside the lock whoever forgot to take it.
pub fn hold(job: &Path) -> Result<Option<JobLock>, LockError> {
    if held_by_current_thread(job) { Ok(None) } else { lock(job).map(Some) }
}

/// Whether this thread holds `job`.
pub fn held_by_current_thread(job: &Path) -> bool {
    registry().get(&key(job)) == Some(&std::thread::current().id())
}

/// Whether any thread of this process holds `job`.
pub fn held_in_process(job: &Path) -> bool {
    registry().contains_key(&key(job))
}

/// How long a wait on another process may go, and what ends it early.
struct Wait<'a> {
    deadline: Instant,
    stop: &'a dyn Fn() -> bool,
}

/// One look at `<job>.lock`.
enum Attempt {
    Locked(File),
    /// Another process holds it.
    Held,
    /// The file locked was not the one at the path any more: its job went, and
    /// the holder removed it on the way out. Looked at again after the
    /// shortest pause.
    Replaced,
    /// No lock can be had here at all.
    Unlockable(std::io::Error),
}

fn attempt(path: &Path) -> Attempt {
    let file = match std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path) {
        Ok(file) => file,
        Err(error) => return Attempt::Unlockable(error),
    };
    loop {
        match os::try_lock(&file) {
            Ok(()) if os::same_file(&file, path) => return Attempt::Locked(file),
            Ok(()) => return Attempt::Replaced,
            Err(error) if os::is_contended(&error) => return Attempt::Held,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Attempt::Unlockable(error),
        }
    }
}

/// Lock `<job>.lock` against other processes. `Ok(None)` where it cannot be
/// done at all, said on stderr. Without `wait`, any holder is an `Err` at once.
fn os_lock(job: &Path, wait: Option<&Wait>) -> Result<Option<File>, LockError> {
    let path = lock_path(job);
    let busy = || LockError::Busy { path: job.to_path_buf() };
    let started = Instant::now();
    let mut pause = Duration::from_millis(1);
    let mut said = false;
    let mut replaced = false;
    loop {
        let unlockable = match attempt(&path) {
            Attempt::Locked(file) => return Ok(Some(file)),
            Attempt::Replaced => {
                replaced = true;
                None
            }
            Attempt::Held => None,
            // Windows keeps a removed file's name until its last handle
            // closes, so the one just replaced can refuse to open for a moment:
            // that is somebody else's handle, and it is waited out like one.
            // A directory that went with it is not waited on.
            Attempt::Unlockable(error) if replaced && error.kind() != std::io::ErrorKind::NotFound => None,
            Attempt::Unlockable(error) => Some(error),
        };
        let Some(wait) = wait else { return Err(busy()) };
        if let Some(error) = unlockable {
            eprintln!("manga-cleaner: {}: cannot lock against other processes: {error}", path.display());
            return Ok(None);
        }
        if (wait.stop)() {
            return Err(LockError::Stopped { path: job.to_path_buf() });
        }
        let now = Instant::now();
        if now >= wait.deadline {
            eprintln!("manga-cleaner: another process is still using {}; giving up", path.display());
            return Err(busy());
        }
        if !said && now - started >= SAY_AFTER {
            said = true;
            eprintln!("manga-cleaner: waiting for another process to release {}", path.display());
        }
        std::thread::sleep(pause.min(wait.deadline - now));
        pause = (pause * 2).min(POLL);
    }
}

#[cfg(unix)]
mod os {
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    pub fn try_lock(file: &File) -> std::io::Result<()> {
        // SAFETY: `flock` on a descriptor this `File` owns for the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn is_contended(error: &std::io::Error) -> bool {
        error.kind() == std::io::ErrorKind::WouldBlock
    }

    pub fn unlock(file: &File) {
        // SAFETY: as above. Closing the descriptor would release it anyway.
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }

    /// Whether `file` is still the file at `path`, rather than one removed
    /// from under it.
    pub fn same_file(file: &File, path: &std::path::Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        match (file.metadata(), std::fs::metadata(path)) {
            (Ok(held), Ok(named)) => held.dev() == named.dev() && held.ino() == named.ino(),
            _ => false,
        }
    }
}

#[cfg(windows)]
mod os {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, LockFile, UnlockFile, BY_HANDLE_FILE_INFORMATION,
    };

    pub fn try_lock(file: &File) -> std::io::Result<()> {
        // SAFETY: one byte of a handle this `File` owns for the call. Beyond
        // the end of an empty file, which Windows allows, so nothing ever
        // reads or writes the locked range.
        if unsafe { LockFile(file.as_raw_handle() as _, 0, 0, 1, 0) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn is_contended(error: &std::io::Error) -> bool {
        error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32)
    }

    pub fn unlock(file: &File) {
        // SAFETY: as above. Closing the handle would release it anyway.
        unsafe {
            UnlockFile(file.as_raw_handle() as _, 0, 0, 1, 0);
        }
    }

    /// Whether `file` is still the file at `path`, rather than one removed
    /// from under it: the volume and file index of both handles agree.
    pub fn same_file(file: &File, path: &std::path::Path) -> bool {
        let Ok(named) = File::open(path) else { return false };
        match (identity(file), identity(&named)) {
            (Some(held), Some(named)) => held == named,
            _ => false,
        }
    }

    fn identity(file: &File) -> Option<(u32, u32, u32)> {
        // SAFETY: a zeroed plain-data struct the call fills, and a handle this
        // `File` owns for the call.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return None;
        }
        Some((info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow))
    }
}

#[cfg(not(any(unix, windows)))]
mod os {
    use std::fs::File;

    pub fn try_lock(_file: &File) -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no file locks on this platform"))
    }

    pub fn is_contended(_error: &std::io::Error) -> bool {
        false
    }

    pub fn unlock(_file: &File) {}

    pub fn same_file(_file: &File, _path: &std::path::Path) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir()
            .join("cleaner-core-lock")
            .join(format!("{name}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_lock_file_sits_beside_the_manifest() {
        assert_eq!(lock_path(Path::new("/lib/p1/c2.mtclean")), PathBuf::from("/lib/p1/c2.mtclean.lock"));
    }

    #[test]
    fn a_held_job_is_seen_by_its_thread_and_refused_to_others() {
        let dir = scratch("held");
        let job = dir.join("c1.mtclean");
        assert!(!held_in_process(&job));
        let lock = lock(&job).unwrap();
        assert!(lock.across_processes(), "the lock file was not locked");
        assert!(lock_path(&job).exists());
        assert!(held_by_current_thread(&job));
        assert!(hold(&job).unwrap().is_none(), "a holder was handed a second lock");
        assert!(try_lock(&job).is_none());
        let other = {
            let job = job.clone();
            std::thread::spawn(move || (held_by_current_thread(&job), held_in_process(&job), try_lock(&job).is_none()))
                .join()
                .unwrap()
        };
        assert_eq!(other, (false, true, true));
        drop(lock);
        assert!(!held_in_process(&job));
        assert!(try_lock(&job).is_some(), "the lock was not given back");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_spellings_of_one_directory_are_one_job() {
        let dir = scratch("spelling");
        let plain = dir.join("c1.mtclean");
        let dotted = dir.join(".").join("c1.mtclean");
        let _lock = lock(&plain).unwrap();
        assert!(held_by_current_thread(&dotted));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[should_panic(expected = "already held by this thread")]
    fn taking_a_job_twice_on_one_thread_is_a_panic_not_a_hang() {
        let dir = scratch("twice");
        let job = dir.join("c1.mtclean");
        let _first = lock(&job).unwrap();
        let _second = lock(&job);
    }

    #[test]
    fn a_job_whose_directory_is_missing_still_locks_this_process() {
        let dir = scratch("missing");
        let job = dir.join("absent").join("c1.mtclean");
        let lock = lock(&job).unwrap();
        assert!(!lock.across_processes());
        assert!(held_by_current_thread(&job));
        drop(lock);
        assert!(!held_in_process(&job));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Another process, as the kernel sees one: a second open of the lock
    /// file, locked on its own. `flock` and `LockFile` both refuse a second
    /// open file even inside one process.
    fn another_process_holds(job: &Path) -> File {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path(job))
            .unwrap();
        os::try_lock(&file).unwrap();
        file
    }

    #[test]
    fn a_job_another_process_holds_is_busy_once_the_patience_runs_out() {
        let dir = scratch("busy");
        let job = dir.join("c1.mtclean");
        let other = another_process_holds(&job);
        let started = Instant::now();
        let refused = lock_until(&job, Duration::from_millis(200), &|| false).err();
        assert!(started.elapsed() >= Duration::from_millis(200), "gave up before its patience");
        assert_eq!(refused, Some(LockError::Busy { path: job.clone() }));
        assert!(refused.unwrap().to_string().starts_with("job_busy: "), "the interface reads the code");
        assert!(!held_in_process(&job), "a refused lock kept this process's slot");
        assert!(try_lock(&job).is_none());
        os::unlock(&other);
        drop(other);
        assert!(lock(&job).is_ok(), "the job was not taken once the other process let go");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stop_ends_the_wait_on_another_process_and_on_another_thread() {
        let dir = scratch("stop");
        let job = dir.join("c1.mtclean");

        let other = another_process_holds(&job);
        let started = Instant::now();
        let stopped = lock_until(&job, Duration::from_secs(60), &|| true).err();
        assert_eq!(stopped, Some(LockError::Stopped { path: job.clone() }));
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(other);

        let (taken, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        let holder = {
            let job = job.clone();
            let (taken, release) = (taken.0, release.1);
            std::thread::spawn(move || {
                let _held = lock(&job).unwrap();
                taken.send(()).unwrap();
                let _ = release.recv();
            })
        };
        taken.1.recv().unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let stopped = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            lock_until(&job, Duration::from_secs(60), &|| cancel.load(std::sync::atomic::Ordering::SeqCst)).err()
        });
        assert_eq!(stopped, Some(LockError::Stopped { path: job.clone() }));
        release.0.send(()).unwrap();
        holder.join().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_lock_file_goes_with_its_job_and_stays_with_a_live_one() {
        let dir = scratch("tidy");
        let gone = dir.join("gone.mtclean");
        drop(lock(&gone).unwrap());
        assert!(!lock_path(&gone).exists(), "a job that is not there kept a lock file");

        let live = dir.join("live.mtclean");
        std::fs::write(&live, b"{}").unwrap();
        drop(lock(&live).unwrap());
        assert!(lock_path(&live).exists(), "a live job's lock file was removed");

        // Removed under the lock, so the job is gone by the time it drops.
        let held = lock(&live).unwrap();
        std::fs::remove_file(&live).unwrap();
        drop(held);
        assert!(!lock_path(&live).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// What makes removing a lock file safe: a waiter that locked the file
    /// just before it was removed can tell, and does not count it as held.
    #[test]
    fn a_lock_file_replaced_under_its_holder_is_told_apart() {
        let dir = scratch("replaced");
        let path = lock_path(&dir.join("c1.mtclean"));
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).unwrap();
        assert!(os::same_file(&file, &path));
        std::fs::remove_file(&path).unwrap();
        assert!(!os::same_file(&file, &path), "a removed lock file still passed for the one at the path");
        std::fs::write(&path, b"").unwrap();
        assert!(!os::same_file(&file, &path), "a new lock file at the same path passed for the old one");
        let _ = std::fs::remove_dir_all(dir);
    }
}
