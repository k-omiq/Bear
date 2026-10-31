//! The dev log: what this process wrote to stderr, kept where a user can read
//! it (Settings > Dev log) and send it to the developer.
//!
//! ## Why stderr itself
//!
//! Both crates report trouble with `eprintln!`, and a packaged Windows app has
//! no console, so those lines reach nobody. Taking the stream at its source
//! means every one of them is kept without touching a call site, a line added
//! later is kept without anyone remembering to, and so is a panic's message.
//! The process's stderr becomes a pipe. One thread reads it, passes the bytes on
//! to the real stderr (a terminal in development), and appends each line here.
//!
//! **That thread must not stop.** With no reader, the next `eprintln!` panics
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
//! No child process inherits this stderr: each one's stream is piped, sent to a
//! file or discarded where it is started. So nothing a provider's SDK prints can
//! arrive here.

use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::{Captures, Regex};

/// The size at which `dev.log` becomes `dev.log.1`. A long run writes well under
/// this, and it stays small enough to show and paste whole.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// A longer line is cut: a child's progress bar has no newline to wait for.
const MAX_LINE_BYTES: u64 = 8 * 1024;
/// What stands where something was taken out.
const HIDDEN: &str = "[hidden]";
/// How long a panic waits for its message to cross the pipe, for the panic that
/// ends the process.
const PANIC_DRAIN: Duration = Duration::from_millis(100);

static LOG: OnceLock<Log> = OnceLock::new();

/// Start capturing. Failing leaves stderr as it was and the log empty, which
/// costs the log and nothing else, so the app starts regardless.
pub fn install(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Ok(dir) = app.path().app_data_dir() else { return };
    if capture(dir).is_ok() {
        let report = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            report(info);
            std::thread::sleep(PANIC_DRAIN);
        }));
    }
}

fn capture(dir: PathBuf) -> io::Result<()> {
    fs::create_dir_all(&dir)?;
    let log = LOG.get_or_init(|| Log::new(dir));
    let (reader, writer) = io::pipe()?;
    // Copied before the redirect, or the copy would be the pipe.
    let terminal = original_stderr();
    // Started before the redirect: a stream with no reader yet can fill. If the
    // redirect then fails, the writer closes and this thread ends at the EOF.
    std::thread::Builder::new().name("dev-log".into()).spawn(move || {
        log.append(&format!(
            "{} Manga Cleaner {} on {} {}",
            stamp(), env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH,
        ));
        pump(reader, terminal, |line| log.append(&format!("{} {}", stamp(), redact(line, home()))));
    })?;
    redirect_stderr(writer)
}

#[cfg(unix)]
fn original_stderr() -> Option<File> {
    use std::os::fd::AsFd;
    io::stderr().as_fd().try_clone_to_owned().ok().map(File::from)
}

/// `None` in a packaged app: the windows subsystem starts with no stderr.
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

/// Read lines from `pipe` until it closes, pass the bytes on to `terminal`, and
/// hand each line to `each`.
fn pump(pipe: impl Read, mut terminal: Option<File>, mut each: impl FnMut(&str)) {
    let mut pipe = BufReader::new(pipe);
    let mut raw = Vec::new();
    let mut rest = Vec::new();
    loop {
        raw.clear();
        match pipe.by_ref().take(MAX_LINE_BYTES).read_until(b'\n', &mut raw) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
        let mut pass_on = |bytes: &[u8]| {
            if let Some(file) = terminal.as_mut() {
                let _ = file.write_all(bytes);
            }
        };
        pass_on(&raw);
        // A line past the limit: the part kept is logged, and the rest is only
        // passed on. It must be read, or it would become the next "line".
        let cut = raw.len() as u64 == MAX_LINE_BYTES && raw.last() != Some(&b'\n');
        if cut {
            loop {
                rest.clear();
                match pipe.by_ref().take(MAX_LINE_BYTES).read_until(b'\n', &mut rest) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => pass_on(&rest),
                }
                if rest.last() == Some(&b'\n') {
                    break;
                }
            }
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches(['\r', '\n']);
        if cut {
            each(&format!("{line} [cut]"));
        } else if !line.is_empty() {
            each(line);
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
    let mut text = Cow::Borrowed(line);
    if let Some(home) = home {
        if text.contains(home) {
            text = Cow::Owned(text.replace(home, "~"));
        }
        // How `{:?}` prints a Windows path.
        let escaped = home.replace('\\', "\\\\");
        if text.contains(&escaped) {
            text = Cow::Owned(text.replace(&escaped, "~"));
        }
    }

    static PATTERNS: OnceLock<[Regex; 7]> = OnceLock::new();
    let [windows_home, unix_home, url, pair, bearer, token, opaque] = PATTERNS.get_or_init(|| {
        [
            r#"(?i)\b[a-z]:[\\/]+users[\\/]+[^\\/\s"']+"#,
            r#"/(?:Users|home)/[^/\s"']+"#,
            r#"(?i)\b([a-z][a-z0-9+.-]*)://([^\s"'<>)\]]+)"#,
            // `name: value` for a name that says what it holds. The value may be
            // quoted with spaces in it, carry a scheme, or be Rust's `Some("…")`.
            r#"(?i)([\w-]*(?:authorization|api[_-]?key|token|secret|password|passwd|pwd|credential|cookie|signature|private[_-]?key)[\w-]*)(["']?\s*[:=]\s*)(?:some\()?(?:(?:bearer|basic|token)\s+)?(?:"[^"]*"|'[^']*'|[^\s"',;&})\]]+)"#,
            r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{8,}",
            // Known prefixes, unless the tail of a longer name: `studio-ws--mc`
            // is a Modal host for the workspace `studio-ws`.
            r"(^|[^\w.-])(?:(?:as|ak|ws|sk)-|b9_|beam_|hf_|gh[pousr]_|github_pat_|AKIA|eyJ)[A-Za-z0-9_.-]{8,}",
            r"[A-Za-z0-9+/_-]{32,}={0,2}",
        ]
        .map(|pattern| Regex::new(pattern).expect("a constant pattern compiles"))
    });

    let text = windows_home.replace_all(&text, "~");
    let text = unix_home.replace_all(&text, "~");
    let text = url.replace_all(&text, |found: &Captures| hide_url(&found[1], &found[2]));
    let text = pair.replace_all(&text, |found: &Captures| format!("{}{}{HIDDEN}", &found[1], &found[2]));
    let text = bearer.replace_all(&text, |found: &Captures| format!("{} {HIDDEN}", &found[1]));
    let text = token.replace_all(&text, |found: &Captures| format!("{}{HIDDEN}", &found[1]));
    let text = opaque.replace_all(&text, |found: &Captures| {
        if looks_opaque(&found[0]) { HIDDEN.to_owned() } else { found[0].to_owned() }
    });
    text.into_owned()
}

/// `scheme://[hidden].modal.run/[hidden]`, with a loopback host kept whole.
fn hide_url(scheme: &str, rest: &str) -> String {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    let labels: Vec<&str> = host.split('.').collect();
    let shown = if host == "localhost" || host == "127.0.0.1" || labels.len() == 1 {
        host.to_owned()
    } else if labels.iter().all(|label| label.bytes().all(|b| b.is_ascii_digit())) {
        HIDDEN.to_owned()
    } else if labels.len() == 2 {
        host.to_owned()
    } else {
        format!("{HIDDEN}.{}", labels[labels.len() - 2..].join("."))
    };
    let tail = if tail.len() > 1 { format!("/{HIDDEN}") } else { String::new() };
    format!("{scheme}://{shown}{tail}")
}

/// A digest or key, not a name. All hex, or a mix of every kind of character:
/// file and model names, and a run's id, are neither.
fn looks_opaque(word: &str) -> bool {
    let body = word.trim_end_matches('=');
    let has = |test: fn(&u8) -> bool| body.as_bytes().iter().any(test);
    body.bytes().all(|b| b.is_ascii_hexdigit())
        || (has(u8::is_ascii_digit) && has(u8::is_ascii_uppercase) && has(u8::is_ascii_lowercase))
}

/// The two files, and the lock that keeps a roll from crossing a read.
struct Log {
    current: PathBuf,
    previous: PathBuf,
    lock: Mutex<()>,
}

impl Log {
    fn new(dir: PathBuf) -> Self {
        Self { current: dir.join("dev.log"), previous: dir.join("dev.log.1"), lock: Mutex::new(()) }
    }

    /// Opened per line, so nothing sits in a buffer when the process dies and no
    /// handle stays open to stop a clear on Windows. A failed write loses that
    /// line: with the disk full there is nowhere to report it anyway.
    fn append(&self, line: &str) {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let size = fs::metadata(&self.current).map(|meta| meta.len()).unwrap_or(0);
        if size + line.len() as u64 + 1 > MAX_FILE_BYTES {
            let _ = fs::rename(&self.current, &self.previous);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.current) {
            let _ = writeln!(file, "{line}");
        }
    }

    fn read(&self) -> String {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut text = read_lossy(&self.previous);
        text.push_str(&read_lossy(&self.current));
        text
    }

    fn clear(&self) -> io::Result<()> {
        let _held = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        for path in [&self.previous, &self.current] {
            match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
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
