//! The dev log: what this process wrote to stderr, kept where a user can read
//! it (Settings > Dev log) and send it to the developer.
//!
//! ## Why stderr itself
//!
//! Both crates report trouble with `eprintln!`, and a packaged Windows app has
//! no console, so those lines reach nobody. Taking the stream at its source
//! means every one of them is kept without touching a call site, a line added
//! later is kept without anyone remembering to, and so is a panic's message.
//! The process's stderr becomes a pipe. One thread reads it, a second passes the
//! bytes on to the real stderr (a terminal in development), and a third appends
//! each line here. A terminal that stops reading loses bytes and stops nothing.
//!
//! **Those threads must not stop.** With no reader, the next `eprintln!` panics
//! on the broken pipe, so nothing in [`pump`] can fail or unwrap. It also must
//! not print, which would feed itself.
//!
//! ## Why a file and no memory ring
//!
//! The lines that matter most come just before a crash, a GPU driver's fault
//! included, and only a file is still there after the restart. Each line is
//! written as it arrives, into `dev.log` and then `dev.log.1`, so the log never
//! exceeds twice [`MAX_FILE_BYTES`].
//!
//! ## The last lines
//!
//! What is still in the pipe dies with the process, so a process on its way out
//! calls [`drain`]: at exit, in the panic hook, and in a handler for the fatal
//! signals. That sends a [`MARK`] down the pipe and waits, at most [`DRAIN_LIMIT`],
//! for it to come out the far end. The pipe keeps order, so every line before it
//! is on the disk by then. A kill or a power cut gives no chance to do this.
//!
//! ## What is kept out
//!
//! [`redact`] runs before anything reaches the disk: this log is made to be
//! sent to someone. Secrets are not supposed to reach stderr at all (the cloud
//! errors are sanitized, secret values format as `[REDACTED]`, the provisioner's
//! stderr is dropped), so this is the second lock, not the first. It covers what
//! those do not see: a transport error quoting a signed URL, or a native
//! library's own words. The home folder goes too, since it holds a person's name.
//! The rest of a path stays, because a failed read is no use without it.
//!
//! Each Rust child process has its stream piped, sent to a file or discarded
//! where it is started, so nothing a provider's SDK prints can arrive here.

use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::{Captures, Regex};

/// The size at which `dev.log` becomes `dev.log.1`. A long run writes well under
/// this, and it stays small enough to show and paste whole.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// A longer line is cut: a child's progress bar has no newline to wait for.
const MAX_LINE_BYTES: u64 = 8 * 1024;
/// What stands where something was taken out.
const HIDDEN: &str = "[hidden]";
/// How long a process on its way out waits for its last lines. It returns as
/// soon as they are written, so this only costs its whole length when the
/// thread that writes them is stuck.
const DRAIN_LIMIT: Duration = Duration::from_millis(500);
/// How long text with no newline waits for the rest of its line.
const QUIET: Duration = Duration::from_millis(100);
/// The byte [`drain`] sends. It is dropped from the log and the terminal. A
/// stray one in a library's output ends a drain early, and costs nothing else.
const MARK: u8 = 0;
/// Reads waiting for the line thread. Past this the pipe fills and writers wait,
/// which loses nothing.
const CHUNKS_WAITING: usize = 4;
/// Reads waiting for the terminal. Past this they are dropped from the terminal
/// only.
const PASS_ON_WAITING: usize = 64;

static LOG: OnceLock<Log> = OnceLock::new();
static CAPTURED: AtomicBool = AtomicBool::new(false);
/// How many marks went into the pipe, and how many came out the far end.
static SENT: AtomicUsize = AtomicUsize::new(0);
static SEEN: AtomicUsize = AtomicUsize::new(0);

/// Start capturing. Failing leaves stderr as it was and the log empty, which
/// costs the log and nothing else, so the app starts regardless.
pub fn install(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Ok(dir) = app.path().app_data_dir() else { return };
    if capture(dir).is_ok() {
        let report = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            report(info);
            drain();
        }));
    }
}

fn capture(dir: PathBuf) -> io::Result<()> {
    fs::create_dir_all(&dir)?;
    let log = LOG.get_or_init(|| Log::new(dir));
    let (reader, writer) = io::pipe()?;
    // Copied before the redirect, or the copy would be the pipe.
    let terminal = original_stderr();
    #[cfg(windows)]
    note_reader(&reader);
    // Started before the redirect: a stream with no reader yet can fill. If the
    // redirect then fails, the writer closes and this thread ends at the EOF.
    std::thread::Builder::new().name("dev-log".into()).spawn(move || {
        log.append(&format!(
            "{} Manga Cleaner {} on {} {}",
            stamp(), env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH,
        ));
        pump(reader, terminal, |line| log.append(&format!("{} {}", stamp(), redact(line, home()))));
    })?;
    redirect_stderr(writer)?;
    if !CAPTURED.swap(true, Ordering::SeqCst) {
        drain_when_dying();
    }
    Ok(())
}

/// Wait until every line written so far is in the file, or [`DRAIN_LIMIT`]. Runs
/// in a signal handler, so it takes no lock and allocates nothing, and it only
/// writes when the pipe has room: a full pipe with a stuck reader must not turn
/// a crash into a hang.
pub fn drain() {
    if !CAPTURED.load(Ordering::SeqCst) {
        return;
    }
    let step = Duration::from_millis(1);
    let mut ticket = None;
    for _ in 0..DRAIN_LIMIT.as_millis() {
        match ticket {
            None if pipe_has_room() => {
                if !write_mark() {
                    return;
                }
                ticket = Some(SENT.fetch_add(1, Ordering::SeqCst) + 1);
            }
            Some(mine) if SEEN.load(Ordering::SeqCst) >= mine => return,
            _ => {}
        }
        std::thread::sleep(step);
    }
}

#[cfg(unix)]
fn original_stderr() -> Option<File> {
    use std::os::fd::AsFd;
    io::stderr().as_fd().try_clone_to_owned().ok().map(File::from)
}

/// In a packaged app the windows subsystem starts with no stderr. This is then a
/// null handle, and each write to it fails and is ignored.
#[cfg(windows)]
fn original_stderr() -> Option<File> {
    use std::os::windows::io::AsHandle;
    io::stderr().as_handle().try_clone_to_owned().ok().map(File::from)
}

#[cfg(unix)]
fn redirect_stderr(writer: io::PipeWriter) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // Safety: both descriptors are open, and `dup2` swaps fd 2 atomically.
    match unsafe { libc::dup2(writer.as_raw_fd(), libc::STDERR_FILENO) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

/// Rust's stderr asks for this handle on every write, so `eprintln!` follows.
/// C++ libraries write through their own runtime's stderr and do not; see
/// "What has not been measured" in `docs/findings.md`.
#[cfg(windows)]
fn redirect_stderr(writer: io::PipeWriter) -> io::Result<()> {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    // The handle stays open for the life of the process: stderr owns it now.
    // Safety: a valid handle, and this call takes no pointer but the handle.
    match unsafe { SetStdHandle(STD_ERROR_HANDLE, writer.into_raw_handle()) } {
        0 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

/// Whether one more byte goes in without waiting.
#[cfg(unix)]
fn pipe_has_room() -> bool {
    let mut stream = libc::pollfd { fd: libc::STDERR_FILENO, events: libc::POLLOUT, revents: 0 };
    // Safety: one valid `pollfd`, and a zero timeout.
    unsafe { libc::poll(&mut stream, 1, 0) > 0 }
}

#[cfg(unix)]
fn write_mark() -> bool {
    // Safety: a one-byte buffer that lives through the call.
    unsafe { libc::write(libc::STDERR_FILENO, [MARK].as_ptr().cast(), 1) == 1 }
}

/// The exit handler, and a handler for each signal that ends the process.
#[cfg(unix)]
fn drain_when_dying() {
    const FATAL: [libc::c_int; 5] = [libc::SIGABRT, libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE];

    extern "C" fn at_exit() {
        drain();
    }

    /// The handler before this one goes back in place and this returns. A fault
    /// then happens again and reaches that handler with its real details, which
    /// Rust's report of a stack overflow needs. `abort` raises its signal again.
    /// A signal sent by hand with `kill` is taken once and does not end the
    /// process, which the second one does.
    extern "C" fn on_signal(signal: libc::c_int) {
        drain();
        if let Some(found) = PREVIOUS.get().and_then(|all| all.iter().find(|(which, _)| *which == signal)) {
            // Safety: a valid signal number and a valid saved action.
            unsafe { libc::sigaction(signal, &found.1, std::ptr::null_mut()) };
        }
    }

    static PREVIOUS: OnceLock<[(libc::c_int, libc::sigaction); FATAL.len()]> = OnceLock::new();
    // Safety: all-zero is a valid `sigaction`, the calls read and fill only these
    // structs, and the handler has the signature `sa_sigaction` wants with no
    // SA_SIGINFO. SA_ONSTACK keeps a stack overflow's fault on the spare stack.
    unsafe {
        let saved = FATAL.map(|signal| {
            let mut action: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut action);
            (signal, action)
        });
        if PREVIOUS.set(saved).is_err() {
            return;
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
        action.sa_flags = libc::SA_ONSTACK;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in FATAL {
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
        libc::atexit(at_exit);
    }
}

#[cfg(windows)]
static READER: std::sync::atomic::AtomicPtr<std::ffi::c_void> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// The read end stays open as long as the thread that owns it runs, and a null
/// one only makes [`pipe_has_room`] say no.
#[cfg(windows)]
fn note_reader(reader: &io::PipeReader) {
    use std::os::windows::io::AsRawHandle;
    READER.store(reader.as_raw_handle().cast(), Ordering::SeqCst);
}

/// The smallest pipe Windows gives is 4 KiB, so half of that is free space on
/// any of them, and the other writers are not far behind this one.
#[cfg(windows)]
fn pipe_has_room() -> bool {
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let mut waiting = 0u32;
    // Safety: a handle or null, which only fails, and one valid out pointer.
    let answered = unsafe {
        PeekNamedPipe(
            READER.load(Ordering::SeqCst).cast(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut waiting,
            std::ptr::null_mut(),
        )
    };
    answered != 0 && waiting < 2048
}

#[cfg(windows)]
fn write_mark() -> bool {
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    let mut written = 0u32;
    // Safety: the handle `redirect_stderr` set, a one-byte buffer and one valid
    // out pointer.
    unsafe {
        WriteFile(GetStdHandle(STD_ERROR_HANDLE), [MARK].as_ptr(), 1, &mut written, std::ptr::null_mut()) != 0
            && written == 1
    }
}

/// An unhandled fault. Rust's `abort` on Windows ends the process with no
/// handler at all, and a normal exit drains from `lib.rs`; see "What has not been
/// measured" in `docs/findings.md`.
#[cfg(windows)]
fn drain_when_dying() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER,
    };
    const CONTINUE_SEARCH: i32 = 0;

    static PREVIOUS: OnceLock<LPTOP_LEVEL_EXCEPTION_FILTER> = OnceLock::new();

    unsafe extern "system" fn on_fault(info: *const EXCEPTION_POINTERS) -> i32 {
        drain();
        match PREVIOUS.get().copied().flatten() {
            // Safety: the filter that was in place, with what this was given.
            Some(previous) => unsafe { previous(info) },
            None => CONTINUE_SEARCH,
        }
    }

    // Safety: a function with the signature the call wants, for the whole process.
    let previous = unsafe { SetUnhandledExceptionFilter(Some(on_fault)) };
    let _ = PREVIOUS.set(previous);
}

/// Read `pipe` until it closes, pass the bytes on to `terminal`, and hand each
/// line to `each`. The reading has its own thread, so that text with no newline
/// can be logged after [`QUIET`] with the pipe still open.
fn pump(mut pipe: impl Read + Send, terminal: Option<File>, each: impl FnMut(&str)) {
    let mut lines = Lines {
        each,
        terminal: terminal.and_then(start_pass_on),
        line: Vec::new(),
        skipping: false,
        held: false,
        value_follows: false,
    };
    let threaded = std::thread::scope(|scope| {
        let (chunks, arriving) = mpsc::sync_channel(CHUNKS_WAITING);
        let reader = &mut pipe;
        let started = std::thread::Builder::new()
            .name("dev-log-read".into())
            .spawn_scoped(scope, move || read_chunks(reader, |chunk| chunks.send(chunk).is_ok()));
        if started.is_err() {
            return false;
        }
        loop {
            let next = if lines.waiting_for_quiet() {
                arriving.recv_timeout(QUIET)
            } else {
                arriving.recv().map_err(|_| RecvTimeoutError::Disconnected)
            };
            match next {
                Ok(chunk) => lines.take(chunk),
                Err(RecvTimeoutError::Timeout) => lines.quiet(),
                Err(RecvTimeoutError::Disconnected) => return true,
            }
        }
    });
    // With no thread to be had, the lines still get read. Only the wait goes.
    if !threaded {
        read_chunks(&mut pipe, |chunk| {
            lines.take(chunk);
            true
        });
    }
    lines.finish();
}

fn read_chunks(pipe: &mut impl Read, mut give: impl FnMut(Vec<u8>) -> bool) {
    let mut buffer = [0; MAX_LINE_BYTES as usize];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => {
                if !give(buffer[..count].to_vec()) {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// The bytes for the terminal and the marks that came with them. A mark counts as
/// seen when its bytes are written, so a drain covers a panic's message on the
/// terminal too.
fn start_pass_on(mut terminal: File) -> Option<SyncSender<(Vec<u8>, usize)>> {
    let (bytes, arriving) = mpsc::sync_channel::<(Vec<u8>, usize)>(PASS_ON_WAITING);
    std::thread::Builder::new()
        .name("dev-log-terminal".into())
        .spawn(move || {
            for (chunk, marks) in arriving {
                let _ = terminal.write_all(&chunk);
                SEEN.fetch_add(marks, Ordering::SeqCst);
            }
        })
        .ok()?;
    Some(bytes)
}

/// The stream turned into lines.
struct Lines<F> {
    each: F,
    terminal: Option<SyncSender<(Vec<u8>, usize)>>,
    /// The line so far.
    line: Vec<u8>,
    /// A cut line's tail, which is passed on and not logged.
    skipping: bool,
    /// Text that may be a secret's first part, waiting for more.
    held: bool,
    /// The last line ended in `token=`, so this one starts with its value.
    value_follows: bool,
}

impl<F: FnMut(&str)> Lines<F> {
    fn take(&mut self, mut chunk: Vec<u8>) {
        self.held = false;
        for part in chunk.split_inclusive(|byte| *byte == b'\n' || *byte == MARK) {
            let (body, end) = match part.split_last() {
                Some((last, body)) if *last == b'\n' || *last == MARK => (body, Some(*last)),
                _ => (part, None),
            };
            if !self.skipping {
                self.line.extend_from_slice(body);
            }
            match end {
                Some(b'\n') => {
                    if !self.skipping {
                        self.end_line();
                    }
                    self.skipping = false;
                }
                _ if self.line.len() as u64 > MAX_LINE_BYTES => {
                    self.cut();
                    self.skipping = true;
                }
                // The process may be dying: this text does not wait.
                Some(_) => self.give_line(),
                None => {}
            }
        }
        let marks = chunk.iter().filter(|byte| **byte == MARK).count();
        if marks > 0 {
            chunk.retain(|byte| *byte != MARK);
        }
        let dropped = match &self.terminal {
            Some(terminal) => terminal.try_send((chunk, marks)).is_err(),
            None => true,
        };
        if dropped {
            SEEN.fetch_add(marks, Ordering::SeqCst);
        }
    }

    fn end_line(&mut self) {
        if self.line.last() == Some(&b'\r') {
            self.line.pop();
        }
        if self.line.len() as u64 > MAX_LINE_BYTES {
            self.cut();
        } else {
            self.give_line();
        }
    }

    /// The part kept ends at a whole character, and is logged now: the tail may
    /// never end.
    fn cut(&mut self) {
        let mut end = MAX_LINE_BYTES as usize;
        while end > MAX_LINE_BYTES as usize - 3 && self.line[end] & 0xC0 == 0x80 {
            end -= 1;
        }
        self.line.truncate(end);
        let text = format!("{} [cut]", String::from_utf8_lossy(&self.line));
        self.line.clear();
        self.give(&text);
    }

    fn give_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        let text = String::from_utf8_lossy(&line);
        self.give(text.trim_end_matches(['\r', '\n']));
        self.line = line;
        self.line.clear();
    }

    fn give(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.value_follows {
            (self.each)(&hide_first_value(text));
        } else {
            (self.each)(text);
        }
        self.value_follows = open_secret(text) == Some(true);
    }

    fn waiting_for_quiet(&self) -> bool {
        !self.line.is_empty() && !self.skipping && !self.held
    }

    /// Nothing more came for now, so the text so far is logged, unless it ends in
    /// a secret's name or part of its value: logged in two parts, the second
    /// part of the value would pass redaction.
    fn quiet(&mut self) {
        if open_secret(&String::from_utf8_lossy(&self.line)).is_some() {
            self.held = true;
        } else {
            self.give_line();
        }
    }

    fn finish(&mut self) {
        if !self.skipping {
            self.end_line();
        }
    }
}

fn stamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    crate::library::iso8601(now.as_secs())
}

fn home() -> Option<&'static str> {
    static HOME: OnceLock<Option<String>> = OnceLock::new();
    HOME.get_or_init(|| std::env::home_dir().and_then(|path| path.to_str().map(str::to_owned)))
        .as_deref()
        // A root home would turn every `/` into `~`.
        .filter(|home| home.len() > 3)
}

/// One line with its secrets and the home folder taken out.
///
/// The shapes follow `provisioner/redaction.py`, which guards the helper's
/// output the same way, and go wider on URLs: a whole URL goes but the end of
/// its host. A signed URL's secret is in its query, a Modal host names the
/// workspace, and what a report needs is which service it was.
fn redact(line: &str, home: Option<&str>) -> String {
    static PATTERNS: OnceLock<[Regex; 8]> = OnceLock::new();
    let [colour, windows_home, unix_home, url, bare_host, bearer, token, opaque] = PATTERNS.get_or_init(|| {
        [
            // A colour code between a name and its value would hide the pair.
            r"\x1b\[[0-9;?]*[ -/]*[@-~]",
            // A name may have spaces in it when a separator ends it. Up to three
            // words: a longer run is a sentence that happens to hold a path.
            r#"(?i)\b[a-z]:[\\/]+users[\\/]+(?:[^\\/\s"':,]+(?: [^\\/\s"':,]+){0,2}([\\/]+)|[^\\/\s"']+)"#,
            r#"/(?:Users|home)/(?:[^/\s"':,]+(?: [^/\s"':,]+){0,2}(/)|[^/\s"']+)"#,
            // The second part is a password, which may hold one space.
            r#"(?i)\b([a-z][a-z0-9+.-]*)://((?:[^\s/?#"<>@]*\s)?[^\s/?#"<>@]*@)?([^\s"<>]*)"#,
            // What a TLS or DNS library names has no scheme. Only endings that
            // are not file types, and not inside a path or a longer name.
            r#"(?i)(^|[^A-Za-z0-9_./\\@:-])((?:[a-z0-9-]+\.)+(?:com|net|org|run|io|ai|co|cloud))\b((?::\d+)?[/?#][^\s"'<>]*)?"#,
            r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{8,}",
            // Known prefixes, unless the tail of a longer name: `studio-ws--mc`
            // is a Modal host for the workspace `studio-ws`. The letter before
            // an escape counts as no name at all (`\n`, as `{:?}` writes it), and
            // a letter outside ASCII is no part of a token's name. A bad byte
            // inside a token does not end it.
            r"(^|[^A-Za-z0-9_.-]|\\[nrt])(?:(?:as|ak|ws|sk)-|b9_|beam_|hf_|gh[pousr]_|github_pat_|AKIA|eyJ)[A-Za-z0-9_.\x{FFFD}-]{8,}",
            r"[A-Za-z0-9+/_-]{32,}={0,2}",
        ]
        .map(|pattern| Regex::new(pattern).expect("a constant pattern compiles"))
    });

    let text = colour.replace_all(line, "");
    let text = match home {
        Some(home) => hide_home(text, home),
        None => text,
    };
    let keep_end = |found: &Captures| format!("~{}", found.get(1).map_or("", |end| end.as_str()));
    let text = windows_home.replace_all(&text, keep_end);
    let text = unix_home.replace_all(&text, keep_end);
    let text = url.replace_all(&text, |found: &Captures| hide_url(&found[1], &found[3]));
    let text = bare_host.replace_all(&text, |found: &Captures| {
        let tail = found.get(3).map_or("", |tail| tail.as_str());
        let tail = if tail.is_empty() { "" } else { "/[hidden]" };
        format!("{}{}{tail}", &found[1], shown_host(&found[2]))
    });
    let text = hide_pairs(&text);
    let text = bearer.replace_all(&text, |found: &Captures| format!("{} {HIDDEN}", &found[1]));
    let text = token.replace_all(&text, |found: &Captures| format!("{}{HIDDEN}", &found[1]));
    let text = opaque.replace_all(&text, |found: &Captures| {
        if looks_opaque(&found[0]) { HIDDEN.to_owned() } else { found[0].to_owned() }
    });
    text.into_owned()
}

/// The home folder, however this line spells it: Windows paths come with either
/// slash and in any case, and `{:?}` doubles each backslash.
fn hide_home<'a>(text: Cow<'a, str>, home: &str) -> Cow<'a, str> {
    let mut out = String::new();
    let mut copied = 0;
    for (start, _) in text.char_indices() {
        if start < copied {
            continue;
        }
        if let Some(length) = home_length(&text[start..], home) {
            out.push_str(&text[copied..start]);
            out.push('~');
            copied = start + length;
        }
    }
    if copied == 0 {
        return text;
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// How many bytes of `text` the home folder takes, when it starts with it.
fn home_length(text: &str, home: &str) -> Option<usize> {
    let mut rest = text;
    for wanted in home.chars() {
        if wanted == '/' || wanted == '\\' {
            rest = rest.strip_prefix("\\\\").or_else(|| rest.strip_prefix(['\\', '/']))?;
        } else {
            let found = rest.chars().next()?;
            if !found.to_lowercase().eq(wanted.to_lowercase()) {
                return None;
            }
            rest = &rest[found.len_utf8()..];
        }
    }
    // `/Users/ana` is no part of `/Users/anabel`.
    if rest.chars().next().is_some_and(char::is_alphanumeric) {
        return None;
    }
    Some(text.len() - rest.len())
}

/// `name: value` for a name that says what it holds. The names are found here and
/// the value by [`value_length`], because a value can hold anything.
fn pair_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            // A short name stands alone, or `bypass` and `signal` would count.
            // Then an optional closing quote, which may be escaped, and what
            // joins the name to the value. [`secret_length`] judges that.
            r#"(?i)((?:--)?\b(?:pass|auth|sig)\b|[\w-]*(?:authorization|api[_ -]?key|token|secret(?:[_ -]?key)?|password|passwd|pwd|passphrase|credential|cookie|signature|private[_ -]?key|modal[_-]?key)[\w-]*)(\\?["'])?(\s*[:=]\s*|\s*,\s*|\(\s*|\s+)"#,
        )
        .expect("a constant pattern compiles")
    })
}

fn hide_pairs(text: &str) -> Cow<'_, str> {
    let pair = pair_pattern();
    let mut out = String::new();
    let mut copied = 0;
    let mut from = 0;
    while let Some(found) = pair.captures_at(text, from) {
        let name_end = found.get(0).map_or(text.len(), |whole| whole.end());
        from = name_end;
        match secret_length(&found[1], &found[3], &text[name_end..]) {
            None | Some(0) => {}
            Some(length) => {
                out.push_str(&text[copied..name_end]);
                out.push_str(HIDDEN);
                copied = name_end + length;
                from = copied;
            }
        }
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// Some(true) when `text` ends in a secret's name with no value yet, and
/// Some(false) when it ends inside the value.
fn open_secret(text: &str) -> Option<bool> {
    let pair = pair_pattern();
    let mut open = None;
    let mut from = 0;
    while let Some(found) = pair.captures_at(text, from) {
        from = found.get(0).map_or(text.len(), |whole| whole.end());
        let rest = &text[from..];
        match secret_length(&found[1], &found[3], rest) {
            Some(_) if rest.trim().is_empty() => open = Some(true),
            Some(length) if length == rest.len() => open = Some(false),
            _ => open = None,
        }
    }
    open
}

/// The length of the value after `name` and `joint`, or `None` when this is not
/// a pair: `the token could not be read` is no secret, `token 'abc'` is.
fn secret_length(name: &str, joint: &str, rest: &str) -> Option<usize> {
    let lower = name.to_ascii_lowercase();
    let flag = lower.starts_with("--");
    let assigned = joint.contains(['=', ':']);
    if !flag && matches!(lower.as_str(), "pass" | "auth" | "sig") && !joint.contains('=') {
        return None;
    }
    let (start, _) = wrappers(rest);
    if !assigned && !flag && quote_at(&rest[start..]).is_none() {
        return None;
    }
    // Free text to the end of the line: a password can hold any character, and
    // these headers hold a list.
    let whole_line = ["pass", "pwd", "authorization", "cookie"].iter().any(|word| lower.contains(word));
    Some(value_length(rest, whole_line))
}

/// `Some(`, `Secret(` and `[`, however deep, and how deep.
fn wrappers(rest: &str) -> (usize, usize) {
    let mut at = 0;
    let mut depth = 0;
    loop {
        let tail = &rest[at..];
        let word = tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':')).unwrap_or(tail.len());
        match tail[word..].chars().next() {
            Some('(' | '[') => {
                at += word + 1;
                at += rest[at..].len() - rest[at..].trim_start().len();
                depth += 1;
            }
            _ => return (at, depth),
        }
    }
}

fn quote_at(text: &str) -> Option<&'static str> {
    ["\\\"", "\\'", "\"", "'"].into_iter().find(|quote| text.starts_with(quote))
}

/// The bytes of a value at the start of `rest`, with its quotes and wrappers. A
/// quote with no end runs to the end of the line: a line cut at the limit can
/// leave one.
fn value_length(rest: &str, whole_line: bool) -> usize {
    let (start, depth) = wrappers(rest);
    let value = &rest[start..];
    if let Some(quote) = quote_at(value) {
        let body = &value[quote.len()..];
        let mut end = start + quote.len() + closing_quote(body, quote);
        let closers = rest[end..].bytes().take(depth).take_while(|byte| matches!(byte, b')' | b']')).count();
        end += closers;
        return end;
    }
    if whole_line {
        return rest.len();
    }
    let mut word = value;
    for scheme in ["bearer", "basic", "token", "apikey", "key", "digest"] {
        let is_scheme = word.get(..scheme.len()).is_some_and(|head| head.eq_ignore_ascii_case(scheme))
            && word[scheme.len()..].starts_with(char::is_whitespace);
        if is_scheme {
            word = word[scheme.len()..].trim_start();
            break;
        }
    }
    let length = word.find(char::is_whitespace).unwrap_or(word.len());
    rest.len() - word.len() + length
}

/// Where a quoted value's text ends, past its closing quote.
fn closing_quote(body: &str, quote: &str) -> usize {
    if quote.len() == 2 {
        return body.find(quote).map_or(body.len(), |at| at + quote.len());
    }
    let bytes = body.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            byte if byte == quote.as_bytes()[0] => return at + 1,
            _ => at += 1,
        }
    }
    body.len()
}

/// The line after `token=`, whose first word is taken for the value. A line that
/// was not one loses a word, which costs less than a secret.
fn hide_first_value(line: &str) -> String {
    let value = line.trim_start();
    let indent = &line[..line.len() - value.len()];
    format!("{indent}{HIDDEN}{}", &value[value_length(value, false)..])
}

/// `scheme://[hidden].modal.run/[hidden]`, with a loopback host kept whole.
/// Closing marks after the URL belong to the sentence around it.
fn hide_url(scheme: &str, rest: &str) -> String {
    let url = rest.trim_end_matches([')', ']', '\'', ',', '.', ';']);
    let closing = &rest[url.len()..];
    let end = url.find(['/', '?', '#']).unwrap_or(url.len());
    let (authority, tail) = url.split_at(end);
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = if host.starts_with('[') {
        host.split_inclusive(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    let tail = if tail.len() > 1 { format!("/{HIDDEN}") } else { String::new() };
    format!("{scheme}://{}{tail}{closing}", shown_host(host))
}

/// The end of a host and no more. One word may be a password or a token standing
/// where a host goes, as in `https://bob:pa/ss@host`.
fn shown_host(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]" {
        host.to_owned()
    } else if labels.len() == 1 || labels.iter().all(|label| label.bytes().all(|b| b.is_ascii_digit())) {
        HIDDEN.to_owned()
    } else if labels.len() == 2 {
        host.to_owned()
    } else {
        format!("{HIDDEN}.{}", labels[labels.len() - 2..].join("."))
    }
}

/// A digest or key, not a name. Standard base64 never mixes `/` with `-` or `_`,
/// and rarely starts with `/`, so such a word is a path, judged by its parts.
fn looks_opaque(word: &str) -> bool {
    let body = word.trim_end_matches('=');
    if body.contains('/') && (body.starts_with('/') || body.contains(['-', '_'])) {
        return body.split('/').any(|part| part.len() >= 32 && is_key(part));
    }
    is_key(body)
}

/// With no separator, nothing this long is a word. With one, file and model names
/// and a run's id keep separators close together; a key has a long run between
/// them, and a mix of every kind of character.
fn is_key(body: &str) -> bool {
    if !body.contains(['-', '_']) {
        return true;
    }
    let has = |test: fn(&u8) -> bool| body.as_bytes().iter().any(test);
    let longest = body.split(['-', '_']).map(str::len).max().unwrap_or(0);
    longest >= 16 && has(u8::is_ascii_digit) && has(u8::is_ascii_uppercase) && has(u8::is_ascii_lowercase)
}

/// The two files, and the locks that keep a roll from crossing a read. The file
/// lock is for a second copy of the app, should one get past the single instance
/// plugin.
struct Log {
    current: PathBuf,
    previous: PathBuf,
    lock: Mutex<()>,
    across_processes: Option<File>,
}

/// Holds both locks until dropped.
struct Held<'a> {
    _here: MutexGuard<'a, ()>,
    file: Option<&'a File>,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if let Some(file) = self.file {
            unlock_file(file);
        }
    }
}

impl Log {
    fn new(dir: PathBuf) -> Self {
        let across_processes =
            OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("dev.log.lock")).ok();
        Self { current: dir.join("dev.log"), previous: dir.join("dev.log.1"), lock: Mutex::new(()), across_processes }
    }

    fn hold(&self) -> Held<'_> {
        let here = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let file = self.across_processes.as_ref().filter(|file| lock_file(file));
        Held { _here: here, file }
    }

    /// Opened per line, so nothing sits in a buffer when the process dies and no
    /// handle stays open to stop a clear on Windows. A failed write loses that
    /// line: with the disk full there is nowhere to report it anyway. One write
    /// holds the line and its newline, so no other writer gets between them.
    fn append(&self, line: &str) {
        let _held = self.hold();
        let size = fs::metadata(&self.current).map(|meta| meta.len()).unwrap_or(0);
        if size + line.len() as u64 + 1 > MAX_FILE_BYTES
            && fs::rename(&self.current, &self.previous).is_err()
            && !self.drop_older_half()
        {
            return;
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.current) {
            let _ = file.write_all(format!("{line}\n").as_bytes());
        }
    }

    /// The cap when the roll fails, as it does while a virus scanner or a backup
    /// holds the file on Windows. `false` when this fails too: the line goes, and
    /// the cap holds.
    fn drop_older_half(&self) -> bool {
        let Ok(bytes) = fs::read(&self.current) else { return false };
        let middle = bytes.len() / 2;
        let from = bytes[middle..].iter().position(|byte| *byte == b'\n').map_or(bytes.len(), |at| middle + at + 1);
        fs::write(&self.current, &bytes[from..]).is_ok()
    }

    fn read(&self) -> String {
        let _held = self.hold();
        let mut text = read_lossy(&self.previous);
        text.push_str(&read_lossy(&self.current));
        text
    }

    fn clear(&self) -> io::Result<()> {
        let _held = self.hold();
        for path in [&self.previous, &self.current] {
            match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }
}

/// Whether the lock was taken. The system frees it if this process dies. With no
/// lock the log still works, as it does with one copy of the app.
#[cfg(unix)]
fn lock_file(file: &File) -> bool {
    use std::os::fd::AsRawFd;
    loop {
        // Safety: an open descriptor that `file` keeps open through the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return true;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
    }
}

#[cfg(unix)]
fn unlock_file(file: &File) {
    use std::os::fd::AsRawFd;
    // Safety: as for `lock_file`.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

#[cfg(windows)]
fn lock_file(file: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK};
    // Safety: an open handle that `file` keeps open through the call, and a zeroed
    // OVERLAPPED, which names the file's first byte.
    unsafe {
        let mut place = std::mem::zeroed();
        LockFileEx(file.as_raw_handle().cast(), LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, &mut place) != 0
    }
}

#[cfg(windows)]
fn unlock_file(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    // Safety: as for `lock_file`.
    unsafe {
        let mut place = std::mem::zeroed();
        UnlockFileEx(file.as_raw_handle().cast(), 0, 1, 0, &mut place);
    }
}

fn read_lossy(path: &Path) -> String {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default()
}

/// The log, oldest line first. Empty when nothing was captured, as in a launch
/// check, which keeps its stderr for whoever started it.
#[tauri::command]
pub fn read_dev_log() -> String {
    LOG.get().map(Log::read).unwrap_or_default()
}

#[tauri::command]
pub fn clear_dev_log() -> Result<(), String> {
    match LOG.get() {
        Some(log) => log.clear().map_err(|error| error.to_string()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod gap_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(line: &str) -> String {
        redact(line, None)
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mc-devlog-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What a pipe would hand [`pump`], as the lines it logs.
    fn lines_of(bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        pump(bytes, None, |line| lines.push(line.to_owned()));
        lines
    }

    #[test]
    fn credentials_never_survive() {
        for (line, secret) in [
            ("Authorization: Bearer abc.DEF-123_xyz", "abc.DEF"),
            ("authorization=Basic dXNlcjpwYXNz", "dXNlcjpwYXNz"),
            ("sent bearer abc.DEF-123_xyz to it", "abc.DEF"),
            ("HF_TOKEN=hf_abcdefghijklmnopqrstuvwxyz", "hf_abc"),
            ("modal_token_secret: as-0123456789abcdef", "as-0123"),
            (r#"{"api_key": "two words", "next": 1}"#, "two words"),
            (r#"Config { token: Some("abc123"), port: 1 }"#, "abc123"),
            ("password='pass phrase'", "pass phrase"),
            ("the id was ak-0123456789abcdef here", "ak-0123"),
            ("proxy ws-0123456789abcdef", "ws-0123"),
            ("beam b9_0123456789abcdef", "b9_0123"),
            ("key sk-ant-0123456789abcdef", "sk-ant"),
            ("gh ghp_0123456789abcdef0123", "ghp_0123"),
            ("id AKIAIOSFODNN7EXAMPLE", "AKIAIOSF"),
            ("jwt eyJhbGciOiJIUzI1NiJ9.e30.c2lnbmF0dXJl", "eyJhbGci"),
            ("sidecar 0123456789abcdef0123456789abcdef0123456789abcdef refused", "0123456789abcdef"),
            ("blob Ab3dEf7hIjKlMn0pQrStUvWxYz012345AbCd==", "Ab3dEf7h"),
        ] {
            let out = clean(line);
            assert!(!out.contains(secret), "{line} became {out}");
            assert!(out.contains(HIDDEN), "{line} became {out}");
        }
    }

    #[test]
    fn a_url_keeps_only_the_end_of_its_host() {
        let signed = "error sending request for url \
            (https://bucket.s3.us-east-1.amazonaws.com/o/result.png?X-Amz-Signature=abc&X-Amz-Credential=def)";
        assert_eq!(
            clean(signed),
            format!("error sending request for url (https://{HIDDEN}.amazonaws.com/{HIDDEN})"),
        );
        assert_eq!(
            clean("https://studio-ws--mc-app.modal.run/mc/v1/jobs failed"),
            format!("https://{HIDDEN}.modal.run/{HIDDEN} failed"),
        );
        assert_eq!(clean("https://user:pw@huggingface.co/x"), format!("https://huggingface.co/{HIDDEN}"));
        assert_eq!(clean("http://203.0.113.9:8000/"), format!("http://{HIDDEN}"));
        assert_eq!(clean("http://127.0.0.1:8000/health"), format!("http://127.0.0.1/{HIDDEN}"));
    }

    #[test]
    fn the_home_folder_goes_and_the_rest_of_the_path_stays() {
        assert_eq!(
            redact("/Users/ana maria/Manga/ch1.job failed", Some("/Users/ana maria")),
            "~/Manga/ch1.job failed",
        );
        assert_eq!(
            redact(r"C:\Users\Ana Maria\Manga\p1.png", Some(r"C:\Users\Ana Maria")),
            r"~\Manga\p1.png",
        );
        // `{:?}` of a path doubles each backslash.
        assert_eq!(
            redact(r#""C:\\Users\\Ana Maria\\Manga""#, Some(r"C:\Users\Ana Maria")),
            r#""~\\Manga""#,
        );
        // Not this machine's home, as a path from a shared folder is not.
        assert_eq!(clean("/home/ana/Manga/p.png"), "~/Manga/p.png");
        assert_eq!(clean(r"c:\users\ana\p.png"), r"~\p.png");
    }

    #[test]
    fn the_words_a_report_needs_stay_readable() {
        for line in [
            "manga-cleaner: SAM-TS-L did not open on DirectML, using the CPU: 887A0005",
            "manga-cleaner: detection model: comic-text-and-bubble-detector-v2-int8",
            "a clean of 7f3c9a10-5b1e-4c2a-9d33-0e1f2a3b4c5d could not be saved to undo history",
            "the token could not be read from the store",
            "an undo step of chapter-2 could not be read",
            "as-is copy of the region",
        ] {
            assert_eq!(clean(line), line);
        }
    }

    #[test]
    fn a_chunk_of_output_becomes_lines() {
        assert_eq!(lines_of(b"one\r\ntwo\n\nno newline"), ["one", "two", "no newline"]);
        assert_eq!(lines_of(b"bad \xff byte\n"), ["bad \u{fffd} byte"]);
    }

    #[test]
    fn a_line_with_no_end_is_cut_and_its_tail_does_not_become_a_line() {
        let mut bytes = vec![b'x'; 3 * MAX_LINE_BYTES as usize];
        bytes.extend_from_slice(b"tail\nnext\n");
        let lines = lines_of(&bytes);
        assert_eq!(lines.len(), 2, "{} lines", lines.len());
        assert_eq!(lines[0].len() as u64, MAX_LINE_BYTES + " [cut]".len() as u64);
        assert_eq!(lines[1], "next");
    }

    #[test]
    fn the_log_rolls_stays_bounded_and_clears() {
        let dir = folder("roll");
        let log = Log::new(dir.clone());
        assert_eq!(log.read(), "");
        let line = "x".repeat(1024);
        for _ in 0..(3 * MAX_FILE_BYTES / 1024) {
            log.append(&line);
        }
        log.append("the last line");
        let text = log.read();
        assert!(text.ends_with("the last line\n"));
        assert!(text.len() as u64 <= 2 * MAX_FILE_BYTES, "{} bytes", text.len());
        assert!(text.len() as u64 > MAX_FILE_BYTES, "the older file was read too");

        log.clear().unwrap();
        assert_eq!(log.read(), "");
        // Clearing an empty log is not a failure.
        log.clear().unwrap();
        log.append("after");
        assert_eq!(log.read(), "after\n");
        fs::remove_dir_all(dir).unwrap();
    }

    /// The real redirect, in this test process. `eprintln!` here would go to the
    /// test harness's own capture, so the test writes the stream the way a
    /// native library does; that the macro reaches the stream is std's promise.
    #[test]
    fn the_real_stderr_reaches_the_file_redacted() {
        let dir = folder("capture");
        capture(dir).unwrap();
        writeln!(io::stderr(), "capture-probe token=abc123").unwrap();
        let log = LOG.get().unwrap();
        let mut text = String::new();
        for _ in 0..200 {
            text = log.read();
            if text.contains("capture-probe") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(text.contains("Manga Cleaner "), "{text}");
        assert!(text.contains(&format!("capture-probe token={HIDDEN}")), "{text}");
        assert!(!text.contains("abc123"), "{text}");
    }
}
