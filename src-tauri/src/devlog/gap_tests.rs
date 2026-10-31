//! Gaps found by trying to break the dev log. Each test fails today and says
//! what the log should do, so each is `#[ignore]` with its reason until the bug
//! is fixed: remove the `#[ignore]` with the fix. Run them all with
//! `cargo test -p manga-cleaner --lib devlog::gap_tests -- --ignored --test-threads=1`.

use super::*;
use std::sync::Arc;

fn clean(line: &str) -> String {
    redact(line, None)
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mc-devlog-gap-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every pair is a line and a part of it that must not survive.
fn assert_hidden(cases: &[(&str, &str)]) {
    let leaks: Vec<String> = cases
        .iter()
        .filter_map(|(line, secret)| {
            let out = clean(line);
            out.contains(secret).then(|| format!("{line:?} became {out:?}"))
        })
        .collect();
    assert!(leaks.is_empty(), "{} of {} leaked:\n{}", leaks.len(), cases.len(), leaks.join("\n"));
}

/// What a pipe would hand [`pump`], as the lines it logs, redacted as `capture` does.
fn logged(bytes: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    pump(bytes, None, |line| lines.push(clean(line)));
    lines
}

/// A real token shape must not depend on what stands just before it. `{:?}` of
/// a multi-line string writes `\n` as a backslash and an `n`, and the `n` reads
/// as the end of a longer name. Modal's secrets are short enough that the
/// opaque pattern does not catch them either.
#[test]
#[ignore = "bug: a known token prefix after an escaped newline or a CJK letter is not hidden"]
fn a_token_shape_survives_after_an_escape_or_a_letter() {
    assert_hidden(&[
        (r#""upstream said:\nas-Ab3dEf7hIjKlMn0pQrStUv""#, "Ab3dEf7h"),
        (r#""upstream said:\tak-Ab3dEf7hIjKlMn0pQrStUv""#, "Ab3dEf7h"),
        (r#""upstream said:\nb9_0123456789abcdef""#, "0123456789"),
        ("トークンhf_abcdefghijklmnop", "abcdefghijklmnop"),
    ]);
}

/// `{:?}` of a string holding JSON is how a response body gets printed.
#[test]
#[ignore = "bug: the pair pattern stops at the backslash of an escaped quote"]
fn a_secret_in_escaped_json_is_hidden() {
    assert_hidden(&[
        (r#"{\"token\":\"hunter2\"}"#, "hunter2"),
        (r#""{\"api_key\": \"two words\"}""#, "two words"),
        (r#""{\"password\":\"hunter2\",\"next\":1}""#, "hunter2"),
    ]);
}

#[test]
#[ignore = "bug: only one Some( is skipped, so another wrapper leaves the value behind"]
fn a_wrapped_value_is_hidden() {
    assert_hidden(&[
        (r#"token: Some(Secret("abc123"))"#, "abc123"),
        (r#"token: Some(Some("abc123"))"#, "abc123"),
        (r#"token: ["abc123"]"#, "abc123"),
        (r#"ApiKey("abc123")"#, "abc123"),
        (r#"("authorization", "Bearer abc123")"#, "abc123"),
    ]);
}

/// A password is the one value free to hold any character.
#[test]
#[ignore = "bug: the value ends at the first space, comma, semicolon, ampersand or escaped quote"]
fn the_whole_of_an_awkward_password_is_hidden() {
    assert_hidden(&[
        ("password=ab,cd;ef", "cd;ef"),
        ("password=ab;cd", ";cd"),
        ("password=ab&cd", "&cd"),
        ("secret=ab}cd", "}cd"),
        ("password: my pass phrase", "pass phrase"),
        (r#"password="ab\"cd ef""#, "cd ef"),
    ]);
}

#[test]
#[ignore = "bug: only Bearer, Basic and Token are known as schemes, and a cookie header ends at its first semicolon"]
fn every_part_of_an_auth_header_is_hidden() {
    assert_hidden(&[
        ("Authorization: ApiKey abc123", "abc123"),
        ("Authorization: Key abc123", "abc123"),
        (r#"Authorization: Digest username="bob", response="abc123""#, "abc123"),
        ("Cookie: a=1; session_id=abc123", "abc123"),
        ("Set-Cookie: x=y; session=abc123; Path=/", "abc123"),
    ]);
}

/// `provisioner/redaction.py` knows `sig` and `key` in a query; this does not.
#[test]
#[ignore = "bug: these names are not on the list, or need a colon or equals sign"]
fn names_that_hold_a_secret_are_known() {
    assert_hidden(&[
        ("API key: abc123", "abc123"),
        ("Secret key = abc123", "abc123"),
        ("private key: abc123", "abc123"),
        ("passphrase=abc123", "abc123"),
        ("pass=abc123", "abc123"),
        ("auth=abc123", "abc123"),
        ("Modal-Key: abc123", "abc123"),
        ("store.blob.example.net/o/x.png?se=2026&sig=abc123", "abc123"),
        ("--password abc123", "abc123"),
        ("--api-key abc123", "abc123"),
        ("--token-secret abc123", "abc123"),
        ("invalid token 'abc123'", "abc123"),
    ]);
}

/// A long key of one case, or with no digit, is neither all hex nor a mix of
/// every kind of character.
#[test]
#[ignore = "bug: looks_opaque wants a digit, an upper and a lower letter, or all hex"]
fn a_long_key_of_one_case_is_hidden() {
    assert_hidden(&[
        ("key abcdefghij0123456789abcdefghij0123456789", "abcdefghij0123"),
        ("key ABCDEFGHIJ0123456789ABCDEFGHIJ0123456789", "ABCDEFGHIJ0123"),
        ("key abcdefghijABCDEFGHIJabcdefghijABCDEFGHIJ", "abcdefghijABCD"),
    ]);
}

#[test]
#[ignore = "bug: the URL pattern ends at ) ] ' and a space, and the rest of the URL stays in the line"]
fn nothing_of_a_url_survives_its_punctuation() {
    assert_hidden(&[
        ("https://x.modal.run/v1/job(1)?q=QUERYVALUE", "QUERYVALUE"),
        ("https://x.modal.run/a(b)/PATHVALUE?x=1", "PATHVALUE"),
        ("https://x.modal.run/it's/PATHVALUE", "PATHVALUE"),
        ("https://x.modal.run/[job]/PATHVALUE", "PATHVALUE"),
        ("http://[2001:db8::1]:8000/PATHVALUE?q=1", "PATHVALUE"),
        ("https://user:pa ss@host.example.com/x", "ss@host"),
        ("https://bob:pa/ss@host.example.com/x", "bob"),
        ("https://TOKENVALUE/x", "TOKENVALUE"),
    ]);
}

/// A URL with no scheme is what a TLS or DNS library names, and the code comment
/// says a Modal host names the workspace.
#[test]
#[ignore = "bug: a host or query with no scheme is not seen as a URL"]
fn a_host_with_no_scheme_is_hidden() {
    assert_hidden(&[
        ("certificate not valid for studio-ws--mc-app.modal.run", "studio-ws"),
        ("bucket.s3.amazonaws.com/o/r.png?X-Amz-Date=1&QUERYKEY=1", "QUERYKEY"),
    ]);
}

/// A library that colours its output, as `tracing` does on a terminal.
#[test]
#[ignore = "bug: escape codes between a name and its value stop the pair pattern"]
fn colour_codes_do_not_hide_a_secret_from_the_pattern() {
    assert_hidden(&[("\x1b[3mtoken\x1b[0m\x1b[2m=\x1b[0m\"abc123\"", "abc123")]);
}

/// The cut at the line limit falls inside a quoted value: with no closing quote
/// the pair pattern does not match, and all of the value up to the cut stays.
#[test]
#[ignore = "bug: redaction runs on the cut line, where a quoted value has no end"]
fn a_secret_at_the_line_limit_is_hidden() {
    let mut bytes = vec![b'x'; MAX_LINE_BYTES as usize - 24];
    bytes.extend_from_slice(b" password=\"two words and a tail past the limit\"\n");
    let lines = logged(&bytes);
    assert!(!lines[0].contains("two words"), "{}", &lines[0][lines[0].len() - 40..]);
}

/// Redaction sees one line at a time.
#[test]
#[ignore = "bug: a value on the line after its name is not linked to the name"]
fn a_value_on_the_next_line_is_hidden() {
    let lines = logged(b"token=\nabc123\n\"password\":\n  \"hunter2\"\n");
    let text = lines.join("\n");
    assert!(!text.contains("abc123") && !text.contains("hunter2"), "{text}");
}

/// One bad byte splits a token into parts too short for any pattern.
#[test]
#[ignore = "bug: a bad byte inside a token ends the match"]
fn a_bad_byte_inside_a_token_does_not_save_it() {
    let lines = logged(b"got hf_abc\xffdefghijklmnopqrstu from it\n");
    assert!(!lines[0].contains("defghijklmnopqrstu"), "{}", lines[0]);
}

/// The doc comment says the home folder goes "since it holds a person's name".
#[test]
#[ignore = "bug: the home is matched by exact text, and the fallback pattern ends at a space"]
fn the_whole_name_in_a_home_folder_goes() {
    let home = Some(r"C:\Users\Ana Maria");
    for line in [
        "C:/Users/Ana Maria/Manga/p.png",
        r"c:\users\ana maria\Manga\p.png",
        r"\\?\C:/Users/Ana Maria/Manga/p.png",
    ] {
        let out = redact(line, home);
        assert!(!out.to_lowercase().contains("maria"), "{line} became {out}");
    }
    // Not this machine's home, as a path from another account is not.
    let out = redact("/Users/ana maria/Manga", None);
    assert!(!out.contains("maria"), "{out}");
}

/// "The rest of a path stays, because a failed read is no use without it."
#[test]
#[ignore = "bug: the opaque pattern takes slashes, so a mixed-case path over 32 characters is hidden"]
fn a_path_with_capitals_and_digits_stays() {
    let line = "~/Manga/Yasai_Chapter_009/Page0012_final/mask.png could not be read";
    assert_eq!(clean(line), line);
}

/// Runs this test binary again as a child, for the tests below that must die.
fn run_self(test: &str, dir: &Path) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("devlog::gap_tests::{test}"), "--ignored", "--test-threads=1"])
        .env("MC_DEVLOG_GAP_DIR", dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    // An abort is the point of one of these; only a hang would fail here.
    let _ = status;
}

/// Capture, wait until the log has taken a first line (the patterns compile on
/// it, which is slow and not the point here), then write a burst.
fn burst() {
    let dir = PathBuf::from(std::env::var_os("MC_DEVLOG_GAP_DIR").unwrap());
    capture(dir.clone()).unwrap();
    let mut err = io::stderr();
    writeln!(err, "warm").unwrap();
    while !read_lossy(&dir.join("dev.log")).contains("warm") {
        std::thread::sleep(Duration::from_millis(1));
    }
    for i in 0..50 {
        writeln!(err, "burst line {i}").unwrap();
    }
}

fn kept(dir: &Path) -> usize {
    read_lossy(&dir.join("dev.log")).matches("burst line").count()
}

/// The module's reason for a file: "the lines that matter most come just before
/// a crash". What is still in the pipe dies with the process.
#[test]
#[ignore = "bug: lines still in the pipe when the process aborts are lost; the 100 ms drain covers panics only"]
fn the_last_line_before_an_abort_is_kept() {
    if std::env::var_os("MC_DEVLOG_GAP_DIR").is_some() {
        burst();
        std::process::abort();
    }
    let dir = folder("abort");
    run_self("the_last_line_before_an_abort_is_kept", &dir);
    assert_eq!(kept(&dir), 50, "lines kept of 50");
}

/// `jobs.rs` prints "quitting with N job(s) still stopping" and then exits.
#[test]
#[ignore = "bug: nothing drains the pipe at exit, so lines written just before it are lost"]
fn the_last_line_before_an_exit_is_kept() {
    if std::env::var_os("MC_DEVLOG_GAP_DIR").is_some() {
        burst();
        std::process::exit(0);
    }
    let dir = folder("exit");
    run_self("the_last_line_before_an_exit_is_kept", &dir);
    assert_eq!(kept(&dir), 50, "lines kept of 50");
}

/// What `pump` hands on, from a pipe that stays open, a moment after the write.
fn pumped_while_open(bytes: &[u8]) -> (Vec<String>, Vec<u8>, io::PipeWriter) {
    let dir = folder(&format!("open-{}", bytes.len()));
    let terminal_path = dir.join("terminal");
    let terminal = File::create(&terminal_path).unwrap();
    let (reader, mut writer) = io::pipe().unwrap();
    writer.write_all(bytes).unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let seen = lines.clone();
    std::thread::spawn(move || {
        pump(reader, Some(terminal), |line| seen.lock().unwrap().push(line.to_owned()));
    });
    std::thread::sleep(Duration::from_millis(500));
    let lines = lines.lock().unwrap().clone();
    (lines, fs::read(terminal_path).unwrap(), writer)
}

/// `eprint!` with no newline: a prompt, a partial message, the words of a library
/// about to fall over. It reaches neither the file nor the terminal until a
/// newline comes, so a crash takes it and a developer sees nothing.
#[test]
#[ignore = "bug: read_until holds a line with no newline until the newline or 8 KiB"]
fn a_line_with_no_newline_is_not_held_back() {
    let (lines, terminal, _open) = pumped_while_open(b"about to fall over");
    assert!(!terminal.is_empty(), "the terminal saw nothing");
    assert_eq!(lines, ["about to fall over"]);
}

/// The comment on [`MAX_LINE_BYTES`] names a progress bar with no newline. The
/// tail is read until its newline before the kept part is logged, so a bar that
/// never ends is never logged, and neither is anything after it.
#[test]
#[ignore = "bug: the kept part of a cut line waits for the end of its tail, which may never come"]
fn a_cut_line_is_logged_before_its_tail_ends() {
    let (lines, _, _open) = pumped_while_open(&vec![b'x'; 2 * MAX_LINE_BYTES as usize]);
    assert_eq!(lines.len(), 1, "nothing was logged");
}

/// Pass-on comes before the file. A parent that gave this app a pipe and stopped
/// reading it stops the log too, and the first `eprintln!` after that hangs.
#[cfg(unix)]
#[test]
#[ignore = "bug: a terminal that blocks stops the log, because pass-on runs before each append"]
fn a_terminal_that_blocks_does_not_stop_the_log() {
    use std::os::fd::OwnedFd;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (_unread, stuck) = io::pipe().unwrap();
    let terminal = File::from(OwnedFd::from(stuck));
    let line = format!("{}\n", "x".repeat(1000));
    let bytes = line.repeat(1000);
    let count = Arc::new(AtomicUsize::new(0));
    let counted = count.clone();
    std::thread::spawn(move || {
        pump(bytes.as_bytes(), Some(terminal), |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        });
    });
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(count.load(Ordering::SeqCst), 1000, "lines logged of 1000");
}

/// The cap depends on a rename whose failure is ignored. On Windows a virus
/// scanner or a backup tool holding `dev.log` is the usual cause; a folder in
/// the way gives the same error here.
#[test]
#[ignore = "bug: a failed roll is ignored and the file grows with no limit"]
fn the_cap_holds_when_the_roll_fails() {
    let dir = folder("stuck-roll");
    fs::create_dir_all(dir.join("dev.log.1").join("in-the-way")).unwrap();
    let log = Log::new(dir.clone());
    let line = "x".repeat(1024);
    for _ in 0..(3 * MAX_FILE_BYTES / 1024) {
        log.append(&line);
    }
    let size = fs::metadata(dir.join("dev.log")).unwrap().len();
    assert!(size <= 2 * MAX_FILE_BYTES, "{size} bytes");
}

/// Two copies of the app: both see a full file, both rename, and the second
/// rename puts a nearly empty file over the full `dev.log.1`. The single
/// instance plugin stops this where it works; it is the only thing that does.
#[test]
#[ignore = "bug: the lock is per process, so two instances lose history in a roll"]
fn two_instances_keep_the_older_file() {
    let dir = folder("two-rolls");
    let line = "x".repeat(200);
    let mut smallest = u64::MAX;
    let writers: Vec<_> = (0..4)
        .map(|_| {
            let (dir, line) = (dir.clone(), line.clone());
            std::thread::spawn(move || {
                let log = Log::new(dir);
                for _ in 0..(20 * MAX_FILE_BYTES / 200) {
                    log.append(&line);
                }
            })
        })
        .collect();
    while writers.iter().any(|writer| !writer.is_finished()) {
        if let Ok(meta) = fs::metadata(dir.join("dev.log.1")) {
            smallest = smallest.min(meta.len());
        }
    }
    assert!(smallest > MAX_FILE_BYTES / 2, "dev.log.1 fell to {smallest} bytes");
}

/// `writeln!` on a file makes one write for the line and one for the newline.
#[test]
#[ignore = "bug: a line and its newline are two writes, so two instances can join lines"]
fn two_instances_do_not_join_lines() {
    let dir = folder("two-appends");
    let line = "x".repeat(100);
    let writers: Vec<_> = (0..4)
        .map(|_| {
            let (dir, line) = (dir.clone(), line.clone());
            std::thread::spawn(move || {
                let log = Log::new(dir);
                for _ in 0..1000 {
                    log.append(&line);
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let text = read_lossy(&dir.join("dev.log"));
    let wrong = text.lines().filter(|found| found.len() != 100).count();
    assert_eq!(wrong, 0, "{wrong} lines are not one line");
}

/// Page and chapter names are often Japanese.
#[test]
#[ignore = "bug: the 8 KiB cut counts bytes, so it can fall inside a character"]
fn a_cut_does_not_split_a_character() {
    let mut lines = Vec::new();
    pump("あ".repeat(4000).as_bytes(), None, |line| lines.push(line.to_owned()));
    assert!(!lines[0].contains('\u{fffd}'), "the line holds a replacement character");
}

#[test]
#[ignore = "bug: a line of exactly 8 KiB followed by its newline is marked as cut"]
fn a_line_at_exactly_the_limit_is_not_marked_cut() {
    let mut bytes = vec![b'x'; MAX_LINE_BYTES as usize];
    bytes.push(b'\n');
    let mut lines = Vec::new();
    pump(&bytes[..], None, |line| lines.push(line.to_owned()));
    assert!(!lines[0].ends_with("[cut]"));
}
