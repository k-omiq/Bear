//! Nano Banana: the Gemini image API, called with the user's own key.
//!
//! This is the only code that sends a page crop to Google. The crop and the
//! composite are in [`cleaner_core::engines::nano_banana`]. It does not use the
//! cloud GPU's proposal and journal: a request here is one blocking call that
//! either answers or does not, with no worker to keep paying for.
//!
//! What guards the send:
//!
//! - Settings' Cloud engines switch ([`super::cloud_allowed`]), read when a
//!   region edit opens ([`Config::when_allowed`]).
//! - The key lives in the OS credential store, or only in this process's memory
//!   on a system that has none. It never reaches the webview, `settings.json` or
//!   a log.
//! - No automatic run reaches this rung (`run.rs#reachable_automatically`).
//!   A person names it per region, or confirms a batch with its cost.
//!
//! What bounds it: one edit talks to Google at a time across the whole process
//! ([`Gate`]), each edit has a deadline, a request that moves no bytes for too
//! long is given up, and [`nano_banana_stop`] ends every edit that has begun.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use cleaner_core::engines::model::{Error as ModelError, Rendered};
use cleaner_core::engines::model::applied_mask;
use cleaner_core::engines::nano_banana::{group_for, AnswerError, Model, Prepared, SendRegion};
use cleaner_core::engines::postprocess::Choice;
use cleaner_core::fit::Fitted;
use cleaner_core::image::Raster;
use cleaner_core::mask::{Mask, Rect};
use reqwest::header::HeaderValue;
use serde::Serialize;
use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::secrets::KEYRING_SERVICE;

pub const MODEL_KEY: &str = "nanoBananaModel";
/// What the person asks Nano Banana to do beyond the built-in removal.
pub const PROMPT_KEY: &str = "nanoBananaPrompt";
/// The longest instruction sent, in characters.
pub const MAX_INSTRUCTION_CHARS: usize = 600;

const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const KEYRING_ACCOUNT: &str = "nano-banana-api-key";
/// A 2K answer in base64 with room to spare.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// How many crops of one region are offered before giving up. Google's billing
/// FAQ says a 400 or 500 error is not charged (checked 2026-11-03). A crop that
/// comes back as a 200 with no usable image (`IMAGE_RECITATION`, a kept mark) is
/// a finished request, and whether it is billed is not documented, so this
/// bounds money as well as time.
const MAX_CROPS: usize = 4;
/// How often a waiting edit looks at the stop count, its deadline and its request.
const TICK: Duration = Duration::from_millis(100);
/// The most of the request body handed to the connection per read, so progress
/// is seen in small steps on a slow link.
const UPLOAD_CHUNK: usize = 16 * 1024;

/// How long each part of an edit may take.
///
/// There is no total per request: a 2K crop is up to about 5 MB of base64, which
/// takes over ten minutes to send at 8 KB/s, and its answer as long to come back.
/// What is bounded instead is silence: a link that moves no bytes for `stall`
/// while sending or receiving is dead, and the model gets `generation` between
/// the last byte sent and the first byte of the answer.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// A route that never answers fails here.
    connect: Duration,
    stall: Duration,
    generation: Duration,
    /// One region's whole edit, every crop and retry, from when it gets the
    /// [`Gate`]. Also the HTTP stack's own cap, so a request given up mid-flight
    /// does not keep its thread past it.
    edit: Duration,
    /// The wait before a busy server's one retry, when it names none.
    retry_delay: Duration,
    /// The longest wait a busy server may ask for. A longer one is a quota that
    /// will not lift while the person watches, so the edit gives up instead.
    max_retry_wait: Duration,
}

const LIMITS: Limits = Limits {
    connect: Duration::from_secs(30),
    stall: Duration::from_secs(90),
    // Not measured: a large Pro answer with thinking, and the kernel's send
    // buffer draining after the last byte is handed over, both count here.
    generation: Duration::from_secs(600),
    edit: Duration::from_secs(60 * 60),
    retry_delay: Duration::from_secs(4),
    max_retry_wait: Duration::from_secs(60),
};

const SYSTEM_INSTRUCTION: &str = "You are a precision inpainting model for manga pages. This is a strict removal-only task.\n\
You will receive a single cropped patch image. A semi-transparent blue overlay marks the only editable region.\n\
Delete the lettering under the blue overlay and restore what it covers. Never introduce new elements.\n\
Do not alter any pixels outside the blue overlay, and leave no blue behind.\n\
Return exactly one edited image at the same dimensions as the input.";

const USER_PROMPT: &str = "Removal-only edit inside the blue-marked region only. \
Remove all lettering there: dialogue, sound effects, and their outlines. \
Rebuild what the lettering covers by continuing the surrounding paper, screentone dots, lines and artwork at the same scale and tone. \
Do not add any new text, symbols, or objects. Keep the page's black-and-white or colour style.";

/// The prompt for one request: the built-in one, and the person's own words under
/// it. Their words guide what is removed and how it is rebuilt, and stay inside
/// the mark, so they can not widen the edit.
fn user_prompt(instruction: Option<&str>) -> String {
    match instruction {
        None => USER_PROMPT.to_owned(),
        Some(note) => format!(
            "{USER_PROMPT}\nThe person cleaning this page adds: \"{note}\"\n\
Use that to decide what to remove and how to rebuild it, inside the blue-marked region only."
        ),
    }
}

/// An instruction from settings: printable characters and line breaks only, cut
/// to [`MAX_INSTRUCTION_CHARS`], and nothing when it is blank.
fn clean_instruction(text: Option<&str>) -> Option<String> {
    let kept: String = text?
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(MAX_INSTRUCTION_CHARS)
        .collect();
    let kept = kept.trim();
    (!kept.is_empty()).then(|| kept.to_owned())
}

/// What a region's edit needs from settings.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub model: Model,
    pub instruction: Option<String>,
}

impl Config {
    fn from_settings(settings: Option<&Value>) -> Config {
        let text = |key: &str| settings.and_then(|value| value.get(key)).and_then(Value::as_str);
        // A model id goes into the request's path, so only a known one is used.
        let model = Model::from_setting(text(MODEL_KEY));
        Config {
            model,
            instruction: clean_instruction(text(PROMPT_KEY)),
        }
    }

    /// `None` while Settings' Cloud engines switch is off.
    pub(crate) fn when_allowed(app: &tauri::AppHandle) -> Option<Config> {
        super::cloud_allowed(app).then(|| Config::from_settings(crate::settings::read(app).ok().as_ref()))
    }
}

/// Why no result came back. Each is a catalogue key, because a refused region
/// shows its reason to the person who asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    NoKey,
    /// The credential store would not answer: locked, refused, or failing.
    KeyStore,
    KeyRejected,
    KeyExpired,
    /// Google does not serve the Gemini API where the person is.
    Location,
    /// The key's project has no billing, a spent balance, or no quota at all.
    Billing,
    ModelRejected,
    RateLimited,
    Unreachable,
    /// A safety filter said no. Manga trips it, and another crop would not change
    /// what the page shows.
    Blocked,
    /// `IMAGE_RECITATION`: refused for this crop. The same sound effect over
    /// screentone was refused at 1:1 and 9:16 and cleaned at 1:4 and 3:4.
    Recited,
    BadAnswer,
    KeptMark,
    /// [`nano_banana_stop`] ended the edit.
    Stopped,
}

impl Failure {
    pub(crate) fn reason_key(self) -> &'static str {
        match self {
            Failure::NoKey => "decline.reason.nanoBananaNoKey",
            Failure::KeyStore => "decline.reason.nanoBananaKeyStore",
            Failure::KeyRejected => "decline.reason.nanoBananaKeyRejected",
            Failure::KeyExpired => "decline.reason.nanoBananaKeyExpired",
            Failure::Location => "decline.reason.nanoBananaLocation",
            Failure::Billing => "decline.reason.nanoBananaBilling",
            Failure::ModelRejected => "decline.reason.nanoBananaModelRejected",
            Failure::RateLimited => "decline.reason.nanoBananaRateLimited",
            Failure::Unreachable => "decline.reason.nanoBananaUnreachable",
            Failure::Blocked | Failure::Recited => "decline.reason.nanoBananaBlocked",
            Failure::BadAnswer => "decline.reason.nanoBananaBadAnswer",
            Failure::KeptMark => "decline.reason.nanoBananaKeptMark",
            Failure::Stopped => "decline.reason.nanoBananaStopped",
        }
    }
}

fn request_body(png: &[u8], region: SendRegion, instruction: Option<&str>, with_system: bool) -> Value {
    let mut body = json!({
        "contents": [{
            "role": "user",
            "parts": [
                { "text": user_prompt(instruction) },
                { "inlineData": {
                    "mimeType": "image/png",
                    "data": base64::engine::general_purpose::STANDARD.encode(png),
                } },
            ],
        }],
        "generationConfig": {
            "responseModalities": ["TEXT", "IMAGE"],
            "imageConfig": { "imageSize": region.tier.wire(), "aspectRatio": region.ratio },
        },
    });
    if with_system {
        body["systemInstruction"] = json!({ "parts": [{ "text": SYSTEM_INSTRUCTION }] });
    }
    body
}

/// The image in a response. A part marked `thought` is the model's draft, and it
/// comes before the answer on models that think.
fn read_answer(response: &Value) -> Result<Vec<u8>, Failure> {
    if let Some(reason) = response.pointer("/promptFeedback/blockReason") {
        eprintln!("manga-cleaner: Nano Banana refused the prompt: {reason}");
        return Err(Failure::Blocked);
    }
    let candidates = response.get("candidates").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    for candidate in candidates {
        let parts = candidate.pointer("/content/parts").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        for part in parts {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let inline = part.get("inlineData").or_else(|| part.get("inline_data"));
            if let Some(data) = inline.and_then(|inline| inline.get("data")).and_then(Value::as_str) {
                return base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| Failure::BadAnswer);
            }
        }
    }
    // Why no image came, for the dev log: a key has no room for Google's reason.
    for candidate in candidates {
        let text = candidate.get("finishMessage").and_then(Value::as_str).unwrap_or("");
        let reason = candidate.get("finishReason").and_then(Value::as_str).unwrap_or("none");
        eprintln!("manga-cleaner: Nano Banana gave no image: {reason} {}", text.chars().take(300).collect::<String>());
    }
    let reason_in = |reasons: &[&str]| candidates.iter()
        .filter_map(|candidate| candidate.get("finishReason")?.as_str())
        .any(|reason| reasons.contains(&reason));
    if reason_in(&["RECITATION", "IMAGE_RECITATION"]) {
        return Err(Failure::Recited);
    }
    let blocked = reason_in(&["SAFETY", "IMAGE_SAFETY", "PROHIBITED_CONTENT", "IMAGE_PROHIBITED_CONTENT", "BLOCKLIST", "SPII"]);
    Err(if blocked { Failure::Blocked } else { Failure::BadAnswer })
}

/// One call's HTTP result, before anything decides to retry it.
enum Reply {
    Answer(Value),
    /// A 400 that names the system instruction: the model does not take one,
    /// which freeditor met and answered the same way.
    NoSystem,
    /// A 429, a 408 or a 5xx: worth one more try after the wait, since the
    /// server said it made nothing.
    Busy(Failure, Duration),
    /// Not retried. A lost connection may have lost an answer already billed.
    Fatal(Failure),
}

/// What a Gemini error body says, as far as it decides anything here. The body
/// is `{"error": {"code", "message", "status", "details": [...]}}`; the status
/// adds nothing the code and message do not.
#[derive(Debug, Default)]
struct ErrorBody {
    /// `error.message` and every `fieldViolations[].field`, lowercased.
    text: String,
    /// `details[].reason`, such as `API_KEY_INVALID`.
    reasons: Vec<String>,
    /// `RetryInfo.retryDelay`.
    retry_delay: Option<Duration>,
}

impl ErrorBody {
    fn read(bytes: &[u8]) -> ErrorBody {
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else { return ErrorBody::default() };
        let error = &value["error"];
        let details = error["details"].as_array().map_or(&[][..], Vec::as_slice);
        let mut text = error["message"].as_str().unwrap_or("").to_lowercase();
        for violation in details.iter().filter_map(|detail| detail["fieldViolations"].as_array()).flatten() {
            text.push(' ');
            text.push_str(&violation["field"].as_str().unwrap_or("").to_lowercase());
        }
        ErrorBody {
            text,
            reasons: details.iter().filter_map(|detail| detail["reason"].as_str()).map(str::to_owned).collect(),
            retry_delay: details.iter().filter_map(|detail| detail["retryDelay"].as_str()).find_map(seconds),
        }
    }

    fn says(&self, words: &str) -> bool {
        self.text.contains(words)
    }

    fn reason(&self, name: &str) -> bool {
        self.reasons.iter().any(|reason| reason == name)
    }

    /// A quota whose limit is 0: an image model on a project without billing,
    /// which no wait will lift.
    fn zero_quota(&self) -> bool {
        self.text.match_indices("limit: 0").any(|(at, found)| {
            !self.text[at + found.len()..].starts_with(|c: char| c.is_ascii_digit() || c == '.')
        })
    }
}

/// A protobuf duration as Google writes it: `"41s"`, `"0.5s"`.
fn seconds(text: &str) -> Option<Duration> {
    Duration::try_from_secs_f64(text.strip_suffix('s')?.parse().ok()?).ok()
}

/// What an error status and body mean for this edit. `retry_after` is the
/// `Retry-After` header, in seconds.
fn refused(status: u16, bytes: &[u8], retry_after: Option<Duration>, limits: &Limits) -> Reply {
    let text = String::from_utf8_lossy(bytes);
    eprintln!("manga-cleaner: Nano Banana answered {status}: {}", text.chars().take(300).collect::<String>());
    let error = ErrorBody::read(bytes);
    if (400..500).contains(&status) {
        if error.reason("API_KEY_EXPIRED") || error.says("api key expired") {
            return Reply::Fatal(Failure::KeyExpired);
        }
        if error.reason("API_KEY_INVALID") || error.says("api key not valid") {
            return Reply::Fatal(Failure::KeyRejected);
        }
        // Before location: "free tier is not available in your country. Please
        // enable billing" is answered by billing.
        if status == 402 || error.says("billing") || error.says("spending cap") || (status == 429 && error.zero_quota()) {
            return Reply::Fatal(Failure::Billing);
        }
        if error.says("location") && error.says("not supported") {
            return Reply::Fatal(Failure::Location);
        }
    }
    let wait = retry_after.or(error.retry_delay);
    match status {
        // Redirects are off, so this is a captive portal or a proxy in the way.
        300..=399 => Reply::Fatal(Failure::Unreachable),
        400 if ["system_instruction", "systeminstruction", "system instruction"].iter().any(|name| error.says(name)) => Reply::NoSystem,
        401 | 403 => Reply::Fatal(Failure::KeyRejected),
        404 => Reply::Fatal(Failure::ModelRejected),
        429 => match wait {
            Some(wait) if wait > limits.max_retry_wait => Reply::Fatal(Failure::RateLimited),
            wait => Reply::Busy(Failure::RateLimited, wait.unwrap_or(limits.retry_delay)),
        },
        408 | 500..=599 => match wait {
            Some(wait) if wait > limits.max_retry_wait => Reply::Fatal(Failure::Unreachable),
            wait => Reply::Busy(Failure::Unreachable, wait.unwrap_or(limits.retry_delay)),
        },
        _ => Reply::Fatal(Failure::ModelRejected),
    }
}

/// The one Gemini edit at a time, and the stop that ends them.
///
/// One at a time across the whole process, chapters included: the link is
/// often slow and shared, two uploads at once only make both late, and Google's
/// per-minute limits count the project, not the request.
pub(crate) struct Gate {
    busy: Mutex<bool>,
    free: Condvar,
    /// Bumped by every stop. An edit remembers the count it began at and is
    /// stopped once it differs, so a stop ends what had begun and nothing later.
    stops: AtomicU64,
}

static GATE: Gate = Gate::new();

impl Gate {
    const fn new() -> Gate {
        Gate { busy: Mutex::new(false), free: Condvar::new(), stops: AtomicU64::new(0) }
    }

    fn stops(&self) -> u64 {
        self.stops.load(Ordering::SeqCst)
    }

    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
        self.free.notify_all();
    }

    /// Wait for the gate. `since` is the stop count when the edit began, so a
    /// stop while it waits in the queue ends it there.
    fn enter(&self, since: u64, limit: Duration) -> Result<Turn<'_>, Failure> {
        let mut busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if self.stops() != since {
                return Err(Failure::Stopped);
            }
            if !*busy {
                break;
            }
            busy = self.free.wait_timeout(busy, TICK).unwrap_or_else(PoisonError::into_inner).0;
        }
        *busy = true;
        Ok(Turn { gate: self, since, deadline: Instant::now() + limit })
    }
}

/// An edit holding the [`Gate`], until it drops.
struct Turn<'g> {
    gate: &'g Gate,
    since: u64,
    deadline: Instant,
}

impl Turn<'_> {
    fn check(&self) -> Result<(), Failure> {
        if self.gate.stops() != self.since {
            return Err(Failure::Stopped);
        }
        if Instant::now() >= self.deadline {
            eprintln!("manga-cleaner: Nano Banana edit passed its deadline");
            return Err(Failure::Unreachable);
        }
        Ok(())
    }

    /// Wait `wait`, unless the edit is stopped or its deadline comes first.
    fn sleep(&self, wait: Duration) -> Result<(), Failure> {
        let until = Instant::now() + wait;
        loop {
            self.check()?;
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(TICK));
        }
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        *self.gate.busy.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.gate.free.notify_all();
    }
}

/// End every Nano Banana edit that has begun, in flight or waiting for its
/// turn. Each fails with [`Failure::Stopped`]. A request already sent may still
/// be billed: Google finishes what it was given. The grants go too, so the rest
/// of a stopped batch can not send.
#[tauri::command]
pub fn nano_banana_stop() {
    crate::inference::nano_banana_consent::revoke_all();
    GATE.stop();
}

/// What one request has moved, for the watch over it.
struct Progress {
    start: Instant,
    /// Milliseconds after `start` that a byte last moved either way.
    moved: AtomicU64,
    /// The whole body is handed to the connection.
    sent: AtomicBool,
    /// The answer's status line has come.
    answering: AtomicBool,
    /// The edit gave up on this request: the body and the answer stop at their
    /// next read.
    abandoned: AtomicBool,
}

impl Progress {
    fn new() -> Progress {
        Progress {
            start: Instant::now(),
            moved: AtomicU64::new(0),
            sent: AtomicBool::new(false),
            answering: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
        }
    }

    fn moved_now(&self) {
        self.moved.store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// Whether the request has gone quiet for longer than its part may.
    fn dead(&self, limits: &Limits) -> bool {
        let quiet = self.start.elapsed().saturating_sub(Duration::from_millis(self.moved.load(Ordering::Relaxed)));
        let generating = self.sent.load(Ordering::Relaxed) && !self.answering.load(Ordering::Relaxed);
        let limit = if generating { limits.generation } else { limits.stall };
        if quiet > limit {
            let part = if generating { "the model" } else { "the link" };
            eprintln!("manga-cleaner: Nano Banana gave up: {part} was quiet for {}s", quiet.as_secs());
        }
        quiet > limit
    }
}

/// The request body, read by the HTTP stack as the connection takes it, so a
/// stalled upload shows as silence and an abandoned one stops at the next read.
struct Upload {
    body: Vec<u8>,
    at: usize,
    progress: Arc<Progress>,
}

impl Read for Upload {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.progress.abandoned.load(Ordering::Relaxed) {
            return Err(io::Error::other("the edit gave up on this request"));
        }
        let count = buffer.len().min(self.body.len() - self.at).min(UPLOAD_CHUNK);
        buffer[..count].copy_from_slice(&self.body[self.at..self.at + count]);
        self.at += count;
        if count > 0 {
            self.progress.moved_now();
        }
        if self.at == self.body.len() {
            self.progress.sent.store(true, Ordering::Relaxed);
        }
        Ok(count)
    }
}

/// One request on its own thread: send it, then read the answer, marking
/// progress both ways.
fn exchange(request: reqwest::blocking::RequestBuilder, progress: &Progress, limits: &Limits) -> Reply {
    let mut response = match request.send() {
        Ok(response) => response,
        Err(error) => {
            // `without_url`, and the key is only ever a header.
            eprintln!("manga-cleaner: Nano Banana request failed: {}", error.without_url());
            return Reply::Fatal(Failure::Unreachable);
        }
    };
    progress.moved_now();
    progress.answering.store(true, Ordering::Relaxed);
    let status = response.status().as_u16();
    let retry_after = response.headers().get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok()?.trim().parse().ok())
        .map(Duration::from_secs);
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if progress.abandoned.load(Ordering::Relaxed) {
            return Reply::Fatal(Failure::Unreachable);
        }
        match response.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) if bytes.len() + count > MAX_RESPONSE_BYTES => return Reply::Fatal(Failure::BadAnswer),
            Ok(count) => {
                bytes.extend_from_slice(&chunk[..count]);
                progress.moved_now();
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Reply::Fatal(Failure::Unreachable),
        }
    }
    if status == 200 {
        return match serde_json::from_slice(&bytes) {
            Ok(answer) => Reply::Answer(answer),
            Err(_) => Reply::Fatal(Failure::BadAnswer),
        };
    }
    refused(status, &bytes, retry_after, limits)
}

struct Client {
    endpoint: String,
    /// Marked sensitive, so the HTTP stack keeps it out of its own debug output.
    key: HeaderValue,
    limits: Limits,
    gate: &'static Gate,
    http: reqwest::blocking::Client,
}

impl Client {
    fn new(endpoint: &str, key: &str, limits: Limits, gate: &'static Gate) -> Result<Client, Failure> {
        // Keys are checked when stored, so this is one stored before that check.
        let mut key = HeaderValue::from_str(key).map_err(|_| Failure::KeyRejected)?;
        key.set_sensitive(true);
        // No redirects: the key is a header, and a redirect could carry it off
        // Google. No retries: a resent body may be a second bill. The system
        // proxy is used, so a person behind one can reach Google at all.
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(limits.connect)
            .timeout(limits.edit)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| Failure::Unreachable)?;
        Ok(Client { endpoint: endpoint.to_owned(), key, limits, gate, http })
    }

    /// One request, sent from a thread of its own while this one watches the
    /// stop count, the edit's deadline and how long the link has been quiet.
    /// A request given up on is left to end by itself: its body and answer stop
    /// at their next read, and the connection closes with them.
    fn call(&self, turn: &Turn, model: &str, body: &Value) -> Reply {
        if let Err(failure) = turn.check() {
            return Reply::Fatal(failure);
        }
        let body = body.to_string().into_bytes();
        let progress = Arc::new(Progress::new());
        let length = body.len() as u64;
        let upload = Upload { body, at: 0, progress: Arc::clone(&progress) };
        let request = self
            .http
            .post(format!("{}/{model}:generateContent", self.endpoint))
            .header("content-type", "application/json")
            .header("x-goog-api-key", self.key.clone())
            .body(reqwest::blocking::Body::sized(upload, length));
        let (sender, replies) = mpsc::channel();
        let (watched, limits) = (Arc::clone(&progress), self.limits);
        let spawned = std::thread::Builder::new().name("nano-banana".into()).spawn(move || {
            let _ = sender.send(exchange(request, &watched, &limits));
        });
        if spawned.is_err() {
            return Reply::Fatal(Failure::Unreachable);
        }
        loop {
            match replies.recv_timeout(TICK) {
                Ok(reply) => return reply,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Reply::Fatal(Failure::Unreachable),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let lost = turn.check().err().or_else(|| progress.dead(&self.limits).then_some(Failure::Unreachable));
            if let Some(failure) = lost {
                progress.abandoned.store(true, Ordering::Relaxed);
                return Reply::Fatal(failure);
            }
        }
    }

    /// The edit, with what freeditor learned: drop the system instruction when a
    /// bad request names it. A busy server gets one more try after the wait it
    /// asks for. A key or a model that was refused does not, because it would
    /// only be refused again.
    fn edit(&self, turn: &Turn, model: &str, png: &[u8], region: SendRegion, instruction: Option<&str>)
        -> Result<Vec<u8>, Failure> {
        let mut with_system = true;
        let mut busy_retried = false;
        loop {
            match self.call(turn, model, &request_body(png, region, instruction, with_system)) {
                Reply::Answer(answer) => return read_answer(&answer),
                Reply::Fatal(failure) => return Err(failure),
                Reply::NoSystem if with_system => with_system = false,
                Reply::NoSystem => return Err(Failure::ModelRejected),
                Reply::Busy(failure, _) if busy_retried => return Err(failure),
                Reply::Busy(_, wait) => {
                    busy_retried = true;
                    turn.sleep(wait)?;
                }
            }
        }
    }
}

/// A region edited, and how, for the patch's record.
pub(crate) struct Edited {
    pub rendered: Rendered,
    pub record: Value,
    /// The answer as Google encoded it, kept with the chapter so its
    /// finishing can be changed later with no new request
    /// ([`super::nano_banana_finishing::keep_answer`]).
    pub answer: Vec<u8>,
}

/// Why a render stopped: a catalogue key either way, so a click always gets an answer.
fn model_reason(error: ModelError) -> &'static str {
    match error {
        ModelError::Declined(decline) => decline.reason_key(),
        ModelError::Run(detail) => {
            eprintln!("manga-cleaner: Nano Banana could not prepare a region: {detail}");
            "decline.reason.engineFault"
        }
    }
}

/// Why an answer could not be used.
fn answer_reason(error: AnswerError) -> &'static str {
    match error {
        AnswerError::KeptMark => Failure::KeptMark.reason_key(),
        AnswerError::Render(error) => model_reason(error),
        other => {
            eprintln!("manga-cleaner: Nano Banana answer refused: {other}");
            Failure::BadAnswer.reason_key()
        }
    }
}

/// The page regions that may share a crop with the one being cleaned.
pub(crate) struct Neighbours {
    /// Names the page: its job, chapter and source. An answer is only shared
    /// inside one.
    pub scope: String,
    pub region_id: String,
    /// Where this edit's window starts on the source page. Crops and masks are
    /// kept in source pixels, because the next region reads its own window.
    pub origin: (i64, i64),
    /// The other regions still to be cleaned: id and applied mask, in this
    /// window's pixels, each wholly inside it.
    pub others: Vec<(String, Mask)>,
}

/// An answer that more than one region is waiting on.
struct Shared {
    scope: String,
    model: &'static str,
    instruction: Option<String>,
    /// In source pixels.
    region: SendRegion,
    answer: Vec<u8>,
    /// The regions the answer has not been used for, with the applied mask each
    /// was marked by, in source pixels.
    members: Vec<(String, Mask)>,
    made: Instant,
}

/// Answers kept for later regions. A batch uses an answer within minutes, so a
/// few are enough, and each is a whole answer image as Google encoded it.
const SHARED_KEPT: usize = 8;
const SHARED_TTL: Duration = Duration::from_secs(30 * 60);
static SHARED: Mutex<Vec<Shared>> = Mutex::new(Vec::new());

fn shifted(mask: &Mask, by: (i64, i64)) -> Mask {
    let mut moved = mask.clone();
    moved.bounds.x += by.0;
    moved.bounds.y += by.1;
    moved
}

/// Whether every pixel of `inner` is in `outer`.
fn within(inner: &Mask, outer: &Mask) -> bool {
    let b = inner.bounds;
    (b.y..b.bottom()).all(|y| (b.x..b.right()).all(|x| !inner.contains(x, y) || outer.contains(x, y)))
}

/// The crop of an answer waiting for this region, in source pixels, so the edit
/// can read a window that holds it.
pub(crate) fn shared_crop(scope: &str, config: &Config, region_id: &str) -> Option<Rect> {
    let kept = SHARED.lock().ok()?;
    kept.iter()
        .find(|entry| entry.scope == scope && entry.model == config.model.id && entry.instruction == config.instruction
            && entry.made.elapsed() < SHARED_TTL && entry.members.iter().any(|(id, _)| id == region_id))
        .map(|entry| entry.region.crop)
}

/// The answer waiting for this region, used up by taking it. Not taken when its
/// crop is not in this window or this region's mask is wider than what was
/// marked, because the model was never told to clean the extra.
fn take_shared(share: &Neighbours, config: &Config, applied: &Mask, page: &Raster) -> Option<(Vec<u8>, SendRegion)> {
    let mut kept = SHARED.lock().ok()?;
    kept.retain(|entry| entry.made.elapsed() < SHARED_TTL);
    let at = kept.iter().position(|entry| entry.scope == share.scope && entry.model == config.model.id
        && entry.instruction == config.instruction && entry.members.iter().any(|(id, _)| *id == share.region_id))?;
    let (ox, oy) = share.origin;
    let entry = &mut kept[at];
    let crop = entry.region.crop;
    let local = Rect::new(crop.x - ox, crop.y - oy, crop.w, crop.h);
    if local.x < 0 || local.y < 0 || local.right() > i64::from(page.width) || local.bottom() > i64::from(page.height) {
        return None;
    }
    let member = entry.members.iter().position(|(id, _)| *id == share.region_id)?;
    if !within(&shifted(applied, (ox, oy)), &entry.members[member].1) {
        return None;
    }
    entry.members.remove(member);
    let taken = (entry.answer.clone(), SendRegion { crop: local, ..entry.region });
    if entry.members.is_empty() {
        kept.remove(at);
    }
    Some(taken)
}

/// Keep an answer for the regions that shared its crop.
fn keep_shared(share: &Neighbours, config: &Config, region: SendRegion, answer: &[u8], members: &[usize]) {
    if members.is_empty() {
        return;
    }
    let (ox, oy) = share.origin;
    let Ok(mut kept) = SHARED.lock() else { return };
    kept.retain(|entry| entry.made.elapsed() < SHARED_TTL);
    if kept.len() >= SHARED_KEPT {
        kept.remove(0);
    }
    kept.push(Shared {
        scope: share.scope.clone(),
        model: config.model.id,
        instruction: config.instruction.clone(),
        region: SendRegion { crop: Rect::new(region.crop.x + ox, region.crop.y + oy, region.crop.w, region.crop.h), ..region },
        answer: answer.to_vec(),
        members: members.iter().map(|at| (share.others[*at].0.clone(), shifted(&share.others[*at].1, (ox, oy)))).collect(),
        made: Instant::now(),
    });
}

/// A region edited from `answer`, and how, for the patch's record.
fn finish(config: &Config, prepared: &Prepared, region: SendRegion, answer: &[u8], refused: usize, shared: bool)
    -> Result<Edited, AnswerError> {
    let rendered = prepared.finish(answer)?;
    let crop = region.crop;
    Ok(Edited {
        rendered,
        record: json!({
            "model": config.model.id,
            "image_size": region.tier.wire(),
            "aspect_ratio": region.ratio,
            "crop": [crop.x, crop.y, crop.w, crop.h],
            "output": [region.output.0, region.output.1],
            "crops_refused": refused,
            "instruction": config.instruction.is_some(),
            "shared_answer": shared,
        }),
        answer: answer.to_vec(),
    })
}

/// The region from an answer an earlier region asked for, with this one marked
/// in it: no new request.
fn from_shared(config: &Config, page: &Raster, fitted: &Fitted, finishing: Option<Choice>, share: &Neighbours)
    -> Option<Result<Edited, &'static str>> {
    let applied = applied_mask(fitted, page.width, page.height);
    let (answer, region) = take_shared(share, config, &applied, page)?;
    let prepared = match Prepared::new(page, fitted, region, finishing, &[]) {
        Ok(prepared) => prepared,
        Err(error) => return Some(Err(model_reason(error))),
    };
    Some(finish(config, &prepared, region, &answer, 0, true).map_err(answer_reason))
}

fn render_with(client: &Client, config: &Config, page: &Raster, fitted: &Fitted, finishing: Option<Choice>,
    share: Option<&Neighbours>) -> Result<Edited, &'static str> {
    // A stop from here on ends this edit, even while it waits for its turn.
    let since = client.gate.stops();
    if let Some(share) = share {
        match from_shared(config, page, fitted, finishing, share) {
            Some(Ok(edited)) => return Ok(edited),
            // The answer is used up either way: ask for this region on its own.
            Some(Err(reason)) => eprintln!("manga-cleaner: Nano Banana shared answer did not fit ({reason}), asking again"),
            None => {}
        }
    }
    let others: Vec<Mask> = share.map(|share| share.others.iter().map(|(_, mask)| mask.clone()).collect()).unwrap_or_default();
    let group = group_for(page, fitted, config.model, &others).map_err(model_reason)?;
    let companions: Vec<Mask> = group.members.iter().map(|at| others[*at].clone()).collect();
    let turn = client.gate.enter(since, client.limits.edit).map_err(Failure::reason_key)?;
    // Every answer that could not be used for this crop tries the next one: a
    // refusal, an empty or unreadable answer, one the wrong shape, one that
    // kept the mark. What another crop would not change ends the edit.
    let mut last = Failure::Recited.reason_key();
    for (refused, region) in group.regions.into_iter().take(MAX_CROPS).enumerate() {
        let prepared = Prepared::new(page, fitted, region, finishing, &companions).map_err(model_reason)?;
        let answer = match client.edit(&turn, config.model.id, prepared.marked_png(), region, config.instruction.as_deref()) {
            Ok(answer) => answer,
            Err(failure @ (Failure::Recited | Failure::BadAnswer)) => {
                last = failure.reason_key();
                continue;
            }
            Err(failure) => return Err(failure.reason_key()),
        };
        match finish(config, &prepared, region, &answer, refused, false) {
            Ok(edited) => {
                if let Some(share) = share {
                    keep_shared(share, config, region, &answer, &group.members);
                }
                return Ok(edited);
            }
            Err(AnswerError::Render(error)) => return Err(model_reason(error)),
            Err(error) => last = answer_reason(error),
        }
    }
    Err(last)
}

/// `fitted` on `page`, edited by Gemini with the stored key. `share` names the
/// page's other regions, which ride in the same request when they fit its crop.
pub(crate) fn render(config: &Config, page: &Raster, fitted: &Fitted, finishing: Option<Choice>,
    share: Option<&Neighbours>) -> Result<Edited, &'static str> {
    let key = read_key(&OsStore, &KEY).map_err(Failure::reason_key)?.ok_or(Failure::NoKey.reason_key())?;
    let client = Client::new(ENDPOINT, &key, LIMITS, &GATE).map_err(Failure::reason_key)?;
    render_with(&client, config, page, fitted, finishing, share)
}

/// Why a credential store did not do what it was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreFault {
    /// This system has no credential store at all.
    Missing,
    /// The store is there and failed: locked, refused, or broken.
    Failed,
}

/// Where the key is kept. A trait so the tests can stand in for the OS.
trait KeyStore: Sync {
    fn get(&self) -> Result<Option<String>, StoreFault>;
    fn set(&self, key: &str) -> Result<(), StoreFault>;
    fn delete(&self) -> Result<(), StoreFault>;
    /// Asked without reading the key, so it never prompts.
    fn exists(&self) -> Result<bool, StoreFault>;
}

/// The OS credential store, through `keyring` and the same macOS item writer the
/// cloud credentials use.
struct OsStore;

/// The platforms `Cargo.toml` names a `keyring` backend for. Anywhere else the
/// crate compiles an in-memory stand-in, which is no store at all.
const HAS_STORE: bool = cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux"));

fn fault(error: keyring::Error) -> StoreFault {
    eprintln!("manga-cleaner: Nano Banana key store: {error}");
    if super::secrets::no_store(&error) { StoreFault::Missing } else { StoreFault::Failed }
}

fn entry() -> Result<keyring::Entry, StoreFault> {
    if !HAS_STORE {
        return Err(StoreFault::Missing);
    }
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(fault)
}

impl KeyStore for OsStore {
    fn get(&self) -> Result<Option<String>, StoreFault> {
        match entry()?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(fault(error)),
        }
    }

    fn set(&self, key: &str) -> Result<(), StoreFault> {
        #[cfg(target_os = "macos")]
        let written = crate::macos_keychain::set_password(KEYRING_SERVICE, KEYRING_ACCOUNT, key);
        #[cfg(not(target_os = "macos"))]
        let written = entry()?.set_password(key);
        written.map_err(fault)
    }

    fn delete(&self) -> Result<(), StoreFault> {
        #[cfg(target_os = "macos")]
        let deleted = crate::macos_keychain::delete_password(KEYRING_SERVICE, KEYRING_ACCOUNT);
        #[cfg(not(target_os = "macos"))]
        let deleted = entry()?.delete_credential();
        match deleted {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(fault(error)),
        }
    }

    fn exists(&self) -> Result<bool, StoreFault> {
        if !HAS_STORE {
            return Err(StoreFault::Missing);
        }
        super::secrets::account_exists(KEYRING_ACCOUNT).map_err(fault)
    }
}

/// What this process knows of the key, so a batch reads the store once and not
/// once per region: after an unsigned rebuild every read is a keychain prompt.
#[derive(Default)]
struct KeyMemo {
    /// A key kept only in memory for this run, because the system has no
    /// credential store.
    session: Option<Zeroizing<String>>,
    /// The store's answer, once asked. A failure is kept too, so a cancelled
    /// prompt is not asked again for every region; [`has_key`] forgets it.
    read: Option<Result<Option<Zeroizing<String>>, ()>>,
}

static KEY: Mutex<KeyMemo> = Mutex::new(KeyMemo { session: None, read: None });

/// The key, from memory or from the store. The lock is held across the read, so
/// regions asking at once share one prompt.
fn read_key(store: &dyn KeyStore, memo: &Mutex<KeyMemo>) -> Result<Option<Zeroizing<String>>, Failure> {
    let mut memo = memo.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(key) = &memo.session {
        return Ok(Some(key.clone()));
    }
    let read = memo.read.get_or_insert_with(|| match store.get() {
        Ok(found) => Ok(found.map(Zeroizing::new)),
        // No store, no key: one may yet be kept for the session.
        Err(StoreFault::Missing) => Ok(None),
        Err(StoreFault::Failed) => Err(()),
    });
    read.clone().map_err(|()| Failure::KeyStore)
}

/// Why a key command failed, as the stable code the webview looks up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyError {
    NoStore,
    Invalid,
    Failed,
}

impl KeyError {
    fn code(self) -> &'static str {
        match self {
            KeyError::NoStore => "nanoBanana.key.noStore",
            KeyError::Invalid => "nanoBanana.key.invalid",
            KeyError::Failed => "nanoBanana.key.failed",
        }
    }
}

/// Gemini keys are 39 characters. The bounds are loose so a format change does
/// not lock anyone out, and tight enough that a pasted document is refused here
/// and not later by the HTTP stack or Windows' 2560-byte credential limit.
const KEY_LENGTH: std::ops::RangeInclusive<usize> = 20..=200;

/// A key as pasted, trimmed. Printable ASCII with no spaces only: a line break
/// would make a header the HTTP stack refuses to send.
fn valid_key(key: &str) -> Result<&str, KeyError> {
    let key = key.trim();
    let fits = KEY_LENGTH.contains(&key.len()) && key.bytes().all(|byte| byte.is_ascii_graphic());
    if fits { Ok(key) } else { Err(KeyError::Invalid) }
}

/// What a stored key is kept in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    /// No credential store: the key is in memory for this run only.
    session_only: bool,
}

fn store_key(store: &dyn KeyStore, memo: &Mutex<KeyMemo>, key: &str) -> Result<Stored, KeyError> {
    let key = valid_key(key)?;
    let mut memo = memo.lock().unwrap_or_else(PoisonError::into_inner);
    match store.set(key) {
        Ok(()) => {
            memo.session = None;
            memo.read = Some(Ok(Some(Zeroizing::new(key.to_owned()))));
            Ok(Stored { session_only: false })
        }
        Err(StoreFault::Missing) => {
            memo.session = Some(Zeroizing::new(key.to_owned()));
            memo.read = None;
            Ok(Stored { session_only: true })
        }
        Err(StoreFault::Failed) => {
            // Whether the old key is still there is unknown: ask again next time.
            memo.read = None;
            Err(KeyError::Failed)
        }
    }
}

fn delete_key(store: &dyn KeyStore, memo: &Mutex<KeyMemo>) -> Result<(), KeyError> {
    let mut memo = memo.lock().unwrap_or_else(PoisonError::into_inner);
    memo.session = None;
    match store.delete() {
        Ok(()) => {
            memo.read = Some(Ok(None));
            Ok(())
        }
        // Nothing to forget but the session key; the next check asks again, so
        // Settings still says there is no store.
        Err(StoreFault::Missing) => {
            memo.read = None;
            Ok(())
        }
        Err(StoreFault::Failed) => {
            memo.read = None;
            Err(KeyError::Failed)
        }
    }
}

/// Whether a key is kept. Settings asks this when it opens, which is when a
/// keychain unlocked since a refused read is noticed, so only a key already
/// read is trusted here: a remembered failure or absence is forgotten, and the
/// store is asked without reading the key.
fn has_key(store: &dyn KeyStore, memo: &Mutex<KeyMemo>) -> Result<bool, KeyError> {
    {
        let mut memo = memo.lock().unwrap_or_else(PoisonError::into_inner);
        if memo.session.is_some() || matches!(memo.read, Some(Ok(Some(_)))) {
            return Ok(true);
        }
        memo.read = None;
    }
    store.exists().map_err(|fault| match fault {
        StoreFault::Missing => KeyError::NoStore,
        StoreFault::Failed => KeyError::Failed,
    })
}

/// A key command, off the invoke handler: a store call can wait minutes on a
/// prompt or on D-Bus.
async fn off_thread<T: Send + 'static>(job: impl FnOnce() -> Result<T, KeyError> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(job)
        .await
        .unwrap_or(Err(KeyError::Failed))
        .map_err(|error| error.code().to_owned())
}

/// Whether a key is kept, asked without reading it, so opening Settings never
/// prompts for the keychain.
#[tauri::command]
pub async fn nano_banana_has_key() -> Result<bool, String> {
    off_thread(|| has_key(&OsStore, &KEY)).await
}

/// Store the key. Write-only: nothing ever reads it back to the webview.
#[tauri::command]
pub async fn store_nano_banana_key(key: String) -> Result<Stored, String> {
    let key = Zeroizing::new(key);
    off_thread(move || store_key(&OsStore, &KEY, &key)).await
}

#[tauri::command]
pub async fn delete_nano_banana_key() -> Result<(), String> {
    off_thread(|| delete_key(&OsStore, &KEY)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::engines::nano_banana::Tier;
    use cleaner_core::image::{BitDepth, ColorMode};
    use cleaner_core::mask::{Mask, Rect};
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    /// A fake Gemini: answers each request with what `answer` makes of it (a
    /// whole HTTP response), and keeps what it was sent.
    struct Server {
        endpoint: String,
        seen: Arc<Mutex<Vec<(String, Value)>>>,
    }

    fn serve(replies: Vec<(u16, String)>) -> Server {
        serve_with(replies.len(), move |at, _| http(replies[at].0, "", &replies[at].1))
    }

    fn http(status: u16, headers: &str, body: &str) -> String {
        format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    fn serve_with(count: usize, answer: impl Fn(usize, &Value) -> String + Send + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1beta/models", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for at in 0..count {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 65536];
                let header_end = loop {
                    let read = stream.read(&mut buffer).unwrap();
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        break at + 4;
                    }
                };
                let head = String::from_utf8_lossy(&bytes[..header_end]).to_string();
                let length: usize = head
                    .lines()
                    .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(|v| v.trim().parse().unwrap()))
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let read = stream.read(&mut buffer).unwrap();
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let body: Value = serde_json::from_slice(&bytes[header_end..]).unwrap_or(Value::Null);
                let reply = answer(at, &body);
                log.lock().unwrap().push((head, body));
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Server { endpoint, seen }
    }

    /// No waits between retries, and a gate of its own, so tests running at once
    /// do not queue behind each other or see each other's stops.
    fn limits() -> Limits {
        Limits { retry_delay: Duration::ZERO, ..LIMITS }
    }

    fn own_gate() -> &'static Gate {
        Box::leak(Box::new(Gate::new()))
    }

    fn client_at(endpoint: &str, limits: Limits) -> Client {
        Client::new(endpoint, "test-key", limits, own_gate()).unwrap()
    }

    fn client(server: &Server) -> Client {
        client_at(&server.endpoint, limits())
    }

    /// One region's request, holding the client's gate as a render would.
    fn edit(client: &Client, model: &str, png: &[u8], region: SendRegion, instruction: Option<&str>) -> Result<Vec<u8>, Failure> {
        let turn = client.gate.enter(client.gate.stops(), client.limits.edit).unwrap();
        client.edit(&turn, model, png, region, instruction)
    }

    fn a_region() -> SendRegion {
        SendRegion { crop: Rect::new(0, 0, 8, 8), output: (8, 8), ratio: "4:1", tier: Tier::Half }
    }

    fn image_reply(png: &[u8]) -> String {
        json!({ "candidates": [{ "content": { "parts": [
            { "text": "here" },
            { "inlineData": { "mimeType": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png) } },
        ] } }] }).to_string()
    }

    #[test]
    fn the_request_names_the_chart_size_and_carries_the_key_as_a_header() {
        let server = serve(vec![(200, image_reply(b"abc"))]);
        assert_eq!(edit(&client(&server), "gemini-x", b"png", a_region(), None).unwrap(), b"abc");
        let seen = server.seen.lock().unwrap();
        let (head, body) = &seen[0];
        assert!(head.starts_with("POST /v1beta/models/gemini-x:generateContent "), "{head}");
        assert!(head.to_ascii_lowercase().contains("x-goog-api-key: test-key"));
        assert!(!head.lines().next().unwrap().contains("test-key"), "the key stays out of the URL");
        assert_eq!(body["generationConfig"]["imageConfig"], json!({ "imageSize": "512", "aspectRatio": "4:1" }));
        assert!(body["systemInstruction"].is_object());
        assert_eq!(body["contents"][0]["parts"][1]["inlineData"]["data"], "cG5n");
    }

    fn error_body(status: &str, message: &str, details: Value) -> String {
        json!({ "error": { "code": 400, "status": status, "message": message, "details": details } }).to_string()
    }

    fn names_system() -> String {
        error_body("INVALID_ARGUMENT", "Invalid JSON payload received. Unknown name \"systemInstruction\".", json!([]))
    }

    #[test]
    fn a_bad_request_naming_the_system_instruction_retries_once_without_it() {
        let server = serve(vec![(400, names_system()), (200, image_reply(b"abc"))]);
        assert_eq!(edit(&client(&server), "m", b"png", a_region(), None).unwrap(), b"abc");
        let seen = server.seen.lock().unwrap();
        assert!(seen[0].1["systemInstruction"].is_object());
        assert!(seen[1].1.get("systemInstruction").is_none());
    }

    #[test]
    fn a_second_bad_request_stops() {
        let server = serve(vec![(400, names_system()), (400, names_system())]);
        assert_eq!(edit(&client(&server), "m", b"png", a_region(), None), Err(Failure::ModelRejected));
        assert_eq!(server.seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_bad_request_that_does_not_name_the_system_instruction_is_not_sent_again() {
        for body in ["{}".to_owned(), error_body("INVALID_ARGUMENT", "Request contains an invalid argument.", json!([]))] {
            let server = serve(vec![(400, body)]);
            assert_eq!(edit(&client(&server), "m", b"png", a_region(), None), Err(Failure::ModelRejected));
            assert_eq!(server.seen.lock().unwrap().len(), 1, "a second upload was sent");
        }
    }

    #[test]
    fn google_errors_map_to_their_own_reasons_and_are_never_retried() {
        let key_info = |reason: &str| json!([{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": reason }]);
        let cases = [
            (400, error_body("FAILED_PRECONDITION", "User location is not supported for the API use.", json!([])), Failure::Location),
            (400, error_body("FAILED_PRECONDITION", "Gemini API free tier is not available in your country. Please enable billing on your project in Google AI Studio.", json!([])), Failure::Billing),
            (403, error_body("PERMISSION_DENIED", "Your project has an outstanding billing balance.", json!([])), Failure::Billing),
            (402, error_body("", "Prepay credits depleted.", json!([])), Failure::Billing),
            (429, error_body("RESOURCE_EXHAUSTED", "Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 0, model: gemini-3.1-flash-image", json!([])), Failure::Billing),
            (400, error_body("INVALID_ARGUMENT", "API key expired. Please renew the API key.", key_info("API_KEY_INVALID")), Failure::KeyExpired),
            (400, error_body("INVALID_ARGUMENT", "API key not valid. Please pass a valid API key.", key_info("API_KEY_INVALID")), Failure::KeyRejected),
            (403, error_body("PERMISSION_DENIED", "Your API key was reported as leaked. Please use another API key.", json!([])), Failure::KeyRejected),
            (301, "{}".to_owned(), Failure::Unreachable),
        ];
        for (status, body, failure) in cases {
            let server = serve(vec![(status, body.clone())]);
            assert_eq!(edit(&client(&server), "m", b"png", a_region(), None), Err(failure), "{status} {body}");
            assert_eq!(server.seen.lock().unwrap().len(), 1, "{body}");
        }
    }

    #[test]
    fn a_zero_quota_is_told_apart_from_a_quota_of_ten() {
        let quota = |limit: &str| ErrorBody { text: format!("quota exceeded for metric: x, limit: {limit}, model: m"), ..ErrorBody::default() };
        assert!(quota("0").zero_quota());
        assert!(!quota("10").zero_quota());
        assert!(!quota("0.5").zero_quota());
        assert!(ErrorBody { text: "limit: 0".into(), ..ErrorBody::default() }.zero_quota());
        assert_eq!(seconds("41s"), Some(Duration::from_secs(41)));
        assert_eq!(seconds("0.5s"), Some(Duration::from_millis(500)));
        assert_eq!(seconds("soon"), None);
    }

    #[test]
    fn a_busy_server_is_waited_on_as_it_asks_unless_the_wait_is_long() {
        // RetryInfo names a short wait: waited, and the retry answers.
        let retry_info = error_body("RESOURCE_EXHAUSTED", "Resource has been exhausted.", json!([
            { "@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "0.3s" },
        ]));
        let server = serve(vec![(429, retry_info), (200, image_reply(b"abc"))]);
        let started = Instant::now();
        assert_eq!(edit(&client(&server), "m", b"png", a_region(), None).unwrap(), b"abc");
        assert!(started.elapsed() >= Duration::from_millis(300));

        // A long Retry-After: given up at once, with no second request.
        let reply = http(429, "retry-after: 3600\r\n", "{}");
        let server = serve_with(1, move |_, _| reply.clone());
        assert_eq!(edit(&client(&server), "m", b"png", a_region(), None), Err(Failure::RateLimited));
        assert_eq!(server.seen.lock().unwrap().len(), 1);

        // A 5xx gets the same one retry.
        let server = serve(vec![(503, "{}".into()), (200, image_reply(b"abc"))]);
        assert_eq!(edit(&client(&server), "m", b"png", a_region(), None).unwrap(), b"abc");
    }

    #[test]
    fn a_rejected_key_is_never_retried() {
        for (status, reply) in [(403, "{}"), (401, "{}"), (400, r#"{"error":{"details":[{"reason":"API_KEY_INVALID"}]}}"#)] {
            let server = serve(vec![(status, reply.to_owned())]);
            assert_eq!(edit(&client(&server), "m", b"png", a_region(), None), Err(Failure::KeyRejected));
            assert_eq!(server.seen.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn a_busy_server_gets_one_more_try_only() {
        let server = serve(vec![(429, "{}".into()), (200, image_reply(b"abc"))]);
        assert_eq!(edit(&client(&server), "m", b"png", a_region(), None).unwrap(), b"abc");
        let server = serve(vec![(429, "{}".into()), (429, "{}".into())]);
        assert_eq!(edit(&client(&server), "m", b"png", a_region(), None), Err(Failure::RateLimited));
        assert_eq!(server.seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_lost_connection_is_not_retried_because_the_answer_may_be_billed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1beta/models", listener.local_addr().unwrap());
        let accepted = std::thread::spawn(move || {
            listener.set_nonblocking(false).unwrap();
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
            listener.set_nonblocking(true).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            listener.accept().is_ok()
        });
        let lost = client_at(&endpoint, limits());
        assert_eq!(edit(&lost, "m", b"png", a_region(), None), Err(Failure::Unreachable));
        assert!(!accepted.join().unwrap(), "a second request was sent");
    }

    #[test]
    fn an_unknown_model_says_so() {
        let server = serve(vec![(404, "{}".into())]);
        assert_eq!(edit(&client(&server), "m", b"png", a_region(), None), Err(Failure::ModelRejected));
    }

    #[test]
    fn a_safety_block_is_told_apart_from_an_empty_answer() {
        assert_eq!(read_answer(&json!({ "promptFeedback": { "blockReason": "SAFETY" } })), Err(Failure::Blocked));
        assert_eq!(read_answer(&json!({ "candidates": [{ "finishReason": "IMAGE_SAFETY" }] })), Err(Failure::Blocked));
        assert_eq!(read_answer(&json!({ "candidates": [{ "finishReason": "IMAGE_RECITATION" }] })), Err(Failure::Recited));
        assert_eq!(read_answer(&json!({ "candidates": [{ "finishReason": "NO_IMAGE" }] })), Err(Failure::BadAnswer));
        assert_eq!(read_answer(&json!({})), Err(Failure::BadAnswer));
    }

    #[test]
    fn a_draft_marked_thought_is_not_the_answer() {
        let answer = json!({ "candidates": [{ "content": { "parts": [
            { "thought": true, "inlineData": { "data": "ZHJhZnQ=" } },
            { "inline_data": { "data": "ZmluYWw=" } },
        ] } }] });
        assert_eq!(read_answer(&answer).unwrap(), b"final");
    }

    #[test]
    fn settings_cannot_put_a_path_in_the_request() {
        let config = Config::from_settings(Some(&json!({ MODEL_KEY: "../../evil?x=" })));
        assert_eq!(config.model, Model::DEFAULT);
        let config = Config::from_settings(Some(&json!({ MODEL_KEY: "gemini-nano-banana-2.1" })));
        assert_eq!(config.model.id, "gemini-nano-banana-2.1");
        assert_eq!(Config::from_settings(None).model, Model::DEFAULT);
    }

    #[test]
    fn a_stored_crop_size_is_ignored_because_the_size_is_automatic() {
        // `nanoBananaTier` was a setting before the size became automatic.
        let config = Config::from_settings(Some(&json!({ MODEL_KEY: "gemini-nano-banana-2.1", "nanoBananaTier": "2K" })));
        let server = serve(vec![(200, image_reply(&answer_of(1024, 1024, [200, 200, 200])))]);
        let hole = Rect::new(600, 700, 40, 20);
        let page = page_with_lettering(1200, 1600, hole);
        let fitted = fitted_over(&page, hole);
        render_with(&client(&server), &config, &page, &fitted, None, None).unwrap();
        let seen = server.seen.lock().unwrap();
        assert_eq!(seen[0].1["generationConfig"]["imageConfig"], json!({ "imageSize": "1K", "aspectRatio": "1:1" }));
    }

    #[test]
    fn the_instruction_is_cleaned_cut_and_added_under_the_built_in_prompt() {
        assert_eq!(clean_instruction(None), None);
        assert_eq!(clean_instruction(Some("  \n\t ")), None);
        assert_eq!(clean_instruction(Some(" keep the\u{0}hatching\nlines ")).as_deref(), Some("keep thehatching\nlines"));
        assert_eq!(clean_instruction(Some(&"a".repeat(2000))).unwrap().chars().count(), MAX_INSTRUCTION_CHARS);
        assert_eq!(user_prompt(None), USER_PROMPT);
        let prompt = user_prompt(Some("also erase the small arrow"));
        assert!(prompt.starts_with(USER_PROMPT) && prompt.contains("also erase the small arrow"));
        assert!(prompt.contains("inside the blue-marked region only"));
    }

    #[test]
    fn the_instruction_reaches_the_request() {
        let server = serve(vec![(200, image_reply(b"abc"))]);
        edit(&client(&server), "m", b"png", a_region(), Some("keep the hatching")).unwrap();
        let seen = server.seen.lock().unwrap();
        let text = seen[0].1["contents"][0]["parts"][0]["text"].as_str().unwrap().to_owned();
        assert!(text.contains("keep the hatching"), "{text}");
    }

    #[test]
    fn a_redirect_is_not_followed_so_the_key_stays_with_google() {
        let target = serve(vec![(200, image_reply(b"abc"))]);
        let location = format!("{}/x:generateContent", target.endpoint);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1beta/models", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 65536];
            let _ = stream.read(&mut buffer);
            write!(stream, "HTTP/1.1 307 X\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").unwrap();
        });
        let redirecting = client_at(&endpoint, limits());
        assert_eq!(edit(&redirecting, "m", b"png", a_region(), None), Err(Failure::Unreachable));
        assert!(target.seen.lock().unwrap().is_empty(), "the redirect was followed");
    }

    /// A gray page: a tone with black lettering in a box.
    fn page_with_lettering(width: u32, height: u32, hole: Rect) -> Raster {
        let mut page = Raster {
            width,
            height,
            mode: ColorMode::Gray,
            depth: BitDepth::Eight,
            icc: None,
            palette: None,
            trns: None,
            srgb_intent: None,
            color: Default::default(),
            data: vec![200u8; (width * height) as usize],
        };
        for y in hole.y..hole.bottom() {
            for x in hole.x..hole.right() {
                page.set_sample(x as u32, y as u32, 0, 0);
            }
        }
        page
    }

    fn fitted_over(page: &Raster, rect: Rect) -> Fitted {
        cleaner_core::fit::fit(page, &Mask::filled(rect), 1.0, 0.0, &cleaner_core::fit::EdgeMap::none(page.width, page.height), true)
    }

    fn answer_of(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let data: Vec<u8> = rgb.iter().copied().cycle().take((width * height * 3) as usize).collect();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&data).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn a_region_goes_out_at_chart_size_and_comes_back_only_inside_its_mask() {
        let hole = Rect::new(600, 700, 40, 20);
        let page = page_with_lettering(1200, 1600, hole);
        let fitted = fitted_over(&page, hole);
        let server = serve(vec![(200, image_reply(&answer_of(512, 512, [200, 200, 200])))]);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let edited = render_with(&client(&server), &config, &page, &fitted, None, None).unwrap();

        let seen = server.seen.lock().unwrap();
        assert_eq!(seen[0].1["generationConfig"]["imageConfig"], json!({ "imageSize": "512", "aspectRatio": "1:1" }));
        assert_eq!(edited.record["crop"][2], 512);
        assert_eq!(edited.record["aspect_ratio"], "1:1");

        // Lettering gone inside the mask.
        let rendered = &edited.rendered;
        let bounds = rendered.mask.bounds;
        assert!(bounds.x <= hole.x && bounds.right() >= hole.right());
        let at = |x: i64, y: i64| rendered.pixels.sample((x - bounds.x) as u32, (y - bounds.y) as u32, 0);
        assert_eq!(at(hole.x + 20, hole.y + 10), 200);
    }

    fn recited() -> (u16, String) {
        (200, json!({ "candidates": [{ "finishReason": "IMAGE_RECITATION" }] }).to_string())
    }

    #[test]
    fn a_refused_crop_is_offered_again_at_another_ratio() {
        let hole = Rect::new(600, 700, 40, 20);
        let page = page_with_lettering(1200, 1600, hole);
        let fitted = fitted_over(&page, hole);
        let server = serve(vec![recited(), (200, image_reply(&answer_of(256, 1024, [200, 200, 200])))]);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let edited = render_with(&client(&server), &config, &page, &fitted, None, None).unwrap();
        assert_eq!(edited.record["aspect_ratio"], "1:4");
        assert_eq!(edited.record["crops_refused"], 1);
        let seen = server.seen.lock().unwrap();
        let ratio = |at: usize| seen[at].1["generationConfig"]["imageConfig"]["aspectRatio"].clone();
        assert_eq!((ratio(0), ratio(1)), (json!("1:1"), json!("1:4")));
    }

    #[test]
    fn a_region_refused_at_every_ratio_stops_after_the_limit() {
        let hole = Rect::new(600, 700, 40, 20);
        let page = page_with_lettering(1200, 1600, hole);
        let fitted = fitted_over(&page, hole);
        let server = serve(vec![recited(); MAX_CROPS]);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let refused = render_with(&client(&server), &config, &page, &fitted, None, None).err();
        assert_eq!(refused, Some("decline.reason.nanoBananaBlocked"));
        assert_eq!(server.seen.lock().unwrap().len(), MAX_CROPS);
    }

    #[test]
    fn a_safety_block_is_not_offered_again() {
        let hole = Rect::new(600, 700, 40, 20);
        let page = page_with_lettering(1200, 1600, hole);
        let fitted = fitted_over(&page, hole);
        let server = serve(vec![(200, json!({ "candidates": [{ "finishReason": "IMAGE_SAFETY" }] }).to_string())]);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let refused = render_with(&client(&server), &config, &page, &fitted, None, None).err();
        assert_eq!(refused, Some("decline.reason.nanoBananaBlocked"));
        assert_eq!(server.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_answer_that_kept_the_mark_is_refused() {
        let hole = Rect::new(600, 700, 200, 200);
        let page = page_with_lettering(1200, 1600, hole);
        let fitted = fitted_over(&page, hole);
        let server = serve_with(MAX_CROPS, |_, body| http(200, "", &image_reply(&sent_back(body))));
        let config = Config { model: Model::DEFAULT, instruction: None };
        let refused = render_with(&client(&server), &config, &page, &fitted, None, None).err();
        assert_eq!(refused, Some("decline.reason.nanoBananaKeptMark"));
        assert!(server.seen.lock().unwrap().len() > 1, "only one crop was offered");
    }

    /// Two lettered holes near each other on one page, each fitted on its own.
    fn two_holes() -> (Raster, Rect, Rect) {
        let (a, b) = (Rect::new(600, 700, 40, 20), Rect::new(700, 720, 40, 20));
        let mut page = page_with_lettering(1200, 1600, a);
        for y in b.y..b.bottom() {
            for x in b.x..b.right() {
                page.set_sample(x as u32, y as u32, 0, 0);
            }
        }
        (page, a, b)
    }

    /// A client whose every request is cut off at once, so a test that must make
    /// none fails fast and does not wait on a timeout. The listener stays bound
    /// for the whole run, so no other test can be given its port.
    fn nowhere() -> Client {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1beta/models", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        client_at(&endpoint, limits())
    }

    fn neighbours(scope: &str, region_id: &str, others: Vec<(String, Mask)>) -> Neighbours {
        Neighbours { scope: scope.into(), region_id: region_id.into(), origin: (0, 0), others }
    }

    #[test]
    fn a_neighbour_rides_in_the_same_request_and_is_cleaned_from_its_answer() {
        let (page, a, b) = two_holes();
        let (fit_a, fit_b) = (fitted_over(&page, a), fitted_over(&page, b));
        let config = Config { model: Model::DEFAULT, instruction: None };
        let b_mask = applied_mask(&fit_b, page.width, page.height);

        let server = serve(vec![(200, image_reply(&answer_of(512, 512, [200, 200, 200])))]);
        let share_a = neighbours("t-shared-ride", "a", vec![("b".into(), b_mask)]);
        let first = render_with(&client(&server), &config, &page, &fit_a, None, Some(&share_a)).unwrap();
        assert_eq!(first.record["shared_answer"], false);
        {
            // The one request marked the neighbour as well as this region.
            let seen = server.seen.lock().unwrap();
            let sent = seen[0].1["contents"][0]["parts"][1]["inlineData"]["data"].as_str().unwrap();
            let png = base64::engine::general_purpose::STANDARD.decode(sent).unwrap();
            let marked = image::load_from_memory(&png).unwrap().into_rgb8();
            let crop = first.record["crop"].as_array().unwrap().iter().map(|n| n.as_i64().unwrap()).collect::<Vec<_>>();
            let pixel = marked.get_pixel((b.x + 20 - crop[0]) as u32, (b.y + 10 - crop[1]) as u32);
            assert!(pixel[2] > pixel[0], "the neighbour is marked: {pixel:?}");
        }

        // The neighbour's own turn: there is no server, so any request fails.
        let share_b = neighbours("t-shared-ride", "b", Vec::new());
        let second = render_with(&nowhere(), &config, &page, &fit_b, None, Some(&share_b)).unwrap();
        assert_eq!(second.record["shared_answer"], true);
        let rendered = &second.rendered;
        let at = |x: i64, y: i64| rendered.pixels.sample((x - rendered.mask.bounds.x) as u32, (y - rendered.mask.bounds.y) as u32, 0);
        assert_eq!(at(b.x + 20, b.y + 10), 200, "the lettering is gone");

        // An answer is used once: asking again is a new request.
        let again = serve(vec![(200, image_reply(&answer_of(512, 512, [200, 200, 200])))]);
        let share_again = neighbours("t-shared-ride", "b", Vec::new());
        render_with(&client(&again), &config, &page, &fit_b, None, Some(&share_again)).unwrap();
        assert_eq!(again.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_neighbour_whose_mask_grew_past_what_was_marked_is_sent_again() {
        let (page, a, b) = two_holes();
        let (fit_a, fit_b) = (fitted_over(&page, a), fitted_over(&page, b));
        let config = Config { model: Model::DEFAULT, instruction: None };
        let small = applied_mask(&fit_b, page.width, page.height);
        let server = serve(vec![(200, image_reply(&answer_of(512, 512, [200, 200, 200])))]);
        let share_a = neighbours("t-shared-grew", "b-grew", vec![("b-grew".into(), small.clone())]);
        render_with(&client(&server), &config, &page, &fit_a, None, Some(&share_a)).unwrap();

        // B now reaches 30 px further left than it was marked.
        let wider = fitted_over(&page, Rect::new(b.x - 30, b.y, b.w + 30, b.h));
        let fresh = serve(vec![(200, image_reply(&answer_of(512, 512, [200, 200, 200])))]);
        let share_b = neighbours("t-shared-grew", "b-grew", Vec::new());
        let edited = render_with(&client(&fresh), &config, &page, &wider, None, Some(&share_b)).unwrap();
        assert_eq!(edited.record["shared_answer"], false);
        assert_eq!(fresh.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_answer_is_not_shared_with_another_page_or_another_instruction() {
        let (page, a, b) = two_holes();
        let (fit_a, fit_b) = (fitted_over(&page, a), fitted_over(&page, b));
        let b_mask = applied_mask(&fit_b, page.width, page.height);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let server = serve(vec![(200, image_reply(&answer_of(512, 512, [200, 200, 200])))]);
        let share_a = neighbours("t-shared-scope", "b", vec![("b".into(), b_mask)]);
        render_with(&client(&server), &config, &page, &fit_a, None, Some(&share_a)).unwrap();

        for (scope, instruction) in [("t-other-page", None), ("t-shared-scope", Some("erase it all".to_owned()))] {
            let config = Config { model: Model::DEFAULT, instruction };
            let fresh = serve(vec![(200, image_reply(&answer_of(512, 512, [200, 200, 200])))]);
            let share_b = neighbours(scope, "b", Vec::new());
            let edited = render_with(&client(&fresh), &config, &page, &fit_b, None, Some(&share_b)).unwrap();
            assert_eq!(edited.record["shared_answer"], false, "{scope}");
        }
        // The page and the words are the same again, and the answer is still there.
        let share_b = neighbours("t-shared-scope", "b", Vec::new());
        let edited = render_with(&nowhere(), &config, &page, &fit_b, None, Some(&share_b)).unwrap();
        assert_eq!(edited.record["shared_answer"], true);
    }

    #[test]
    fn a_waiting_answer_names_its_crop_so_the_edit_can_read_a_window_that_holds_it() {
        let (page, a, b) = two_holes();
        let fit_a = fitted_over(&page, a);
        let b_mask = applied_mask(&fitted_over(&page, b), page.width, page.height);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let server = serve(vec![(200, image_reply(&answer_of(512, 512, [200, 200, 200])))]);
        let share_a = neighbours("t-shared-crop", "b", vec![("b".into(), b_mask)]);
        render_with(&client(&server), &config, &page, &fit_a, None, Some(&share_a)).unwrap();
        let crop = shared_crop("t-shared-crop", &config, "b").unwrap();
        assert!(crop.x <= b.x && crop.right() >= b.right());
        assert_eq!(shared_crop("t-shared-crop", &config, "a"), None);
        assert_eq!(shared_crop("t-elsewhere", &config, "b"), None);
    }

    #[test]
    fn a_garbage_answer_is_refused_not_composited() {
        let hole = Rect::new(600, 700, 40, 20);
        let page = page_with_lettering(1200, 1600, hole);
        let fitted = fitted_over(&page, hole);
        let server = serve(vec![(200, image_reply(b"not an image")); MAX_CROPS]);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let refused = render_with(&client(&server), &config, &page, &fitted, None, None).err();
        assert_eq!(refused, Some("decline.reason.nanoBananaBadAnswer"));
    }

    /// The marked crop as it was sent: an answer that kept the mark over the hole
    /// and nowhere else. A blue over the whole answer is a colour cast, which the
    /// kept-mark check lets through.
    fn sent_back(body: &Value) -> Vec<u8> {
        let sent = body["contents"][0]["parts"][1]["inlineData"]["data"].as_str().unwrap();
        base64::engine::general_purpose::STANDARD.decode(sent).unwrap()
    }

    /// A clean answer at exactly the size of the crop that was sent.
    fn gray_like_sent(body: &Value) -> Vec<u8> {
        let sent = image::load_from_memory(&sent_back(body)).unwrap();
        answer_of(sent.width(), sent.height(), GRAY)
    }

    const GRAY: [u8; 3] = [200, 200, 200];

    #[test]
    fn an_answer_that_kept_the_mark_is_offered_again_at_another_crop() {
        let hole = Rect::new(600, 700, 200, 200);
        let page = page_with_lettering(1200, 1600, hole);
        let fitted = fitted_over(&page, hole);
        let server = serve_with(2, |at, body| http(200, "", &image_reply(&if at == 0 { sent_back(body) } else { gray_like_sent(body) })));
        let config = Config { model: Model::DEFAULT, instruction: None };
        let edited = render_with(&client(&server), &config, &page, &fitted, None, None).unwrap();
        assert_eq!(edited.record["crops_refused"], 1);
        assert_eq!(server.seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_shared_answer_that_does_not_fit_its_member_falls_back_to_a_request_of_its_own() {
        let (page, a, b) = two_holes();
        let (fit_a, fit_b) = (fitted_over(&page, a), fitted_over(&page, b));
        let config = Config { model: Model::DEFAULT, instruction: None };
        let b_mask = applied_mask(&fit_b, page.width, page.height);
        // Clean where A is, but the mark left on B: fine for A, refused for B.
        let server = serve_with(1, |_, body| {
            let sent = body["contents"][0]["parts"][1]["inlineData"]["data"].as_str().unwrap();
            let png = base64::engine::general_purpose::STANDARD.decode(sent).unwrap();
            let mut marked = image::load_from_memory(&png).unwrap().into_rgb8();
            let tinted = |pixel: &image::Rgb<u8>| i32::from(pixel[2]) > i32::from(pixel[0]) + 10;
            let first = marked.enumerate_pixels().filter(|(_, _, pixel)| tinted(pixel)).map(|(x, _, _)| x).min().unwrap();
            for (x, _, pixel) in marked.enumerate_pixels_mut() {
                let is_b = tinted(pixel) && x >= first + 70;
                if !is_b {
                    *pixel = image::Rgb(GRAY);
                }
            }
            let mut out = Vec::new();
            image::DynamicImage::ImageRgb8(marked).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
            http(200, "", &image_reply(&out))
        });
        let share_a = neighbours("t-shared-unfit", "a", vec![("b".into(), b_mask)]);
        render_with(&client(&server), &config, &page, &fit_a, None, Some(&share_a)).unwrap();

        let fresh = serve(vec![(200, image_reply(&answer_of(512, 512, GRAY)))]);
        let share_b = neighbours("t-shared-unfit", "b", Vec::new());
        let edited = render_with(&client(&fresh), &config, &page, &fit_b, None, Some(&share_b)).unwrap();
        assert_eq!(edited.record["shared_answer"], false);
        assert_eq!(fresh.seen.lock().unwrap().len(), 1);
    }

    /// A server that takes the request and then says nothing for three seconds,
    /// after writing `head` if any.
    fn silent_after(head: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1beta/models", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 65536];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(head.as_bytes());
            std::thread::sleep(Duration::from_secs(3));
        });
        endpoint
    }

    fn quick() -> Limits {
        Limits { stall: Duration::from_millis(300), generation: Duration::from_millis(300), ..limits() }
    }

    #[test]
    fn a_request_the_model_never_answers_is_given_up() {
        let quiet = client_at(&silent_after(""), quick());
        let started = Instant::now();
        assert_eq!(edit(&quiet, "m", b"png", a_region(), None), Err(Failure::Unreachable));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    #[test]
    fn an_answer_that_stops_coming_is_given_up() {
        let head = "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ncontent-length: 100000\r\n\r\n{\"candi";
        let stalled = client_at(&silent_after(head), quick());
        let started = Instant::now();
        assert_eq!(edit(&stalled, "m", b"png", a_region(), None), Err(Failure::Unreachable));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    #[test]
    fn a_stop_ends_the_edit_in_flight_and_the_one_waiting_for_its_turn() {
        let busy = client_at(&silent_after(""), limits());
        let gate = busy.gate;
        let since = gate.stops();
        std::thread::scope(|scope| {
            let in_flight = scope.spawn(|| edit(&busy, "m", b"png", a_region(), None));
            std::thread::sleep(Duration::from_millis(300));
            let waiting = scope.spawn(|| gate.enter(since, LIMITS.edit).map(|_| ()));
            std::thread::sleep(Duration::from_millis(300));
            assert!(!waiting.is_finished(), "a second edit went in while one was in flight");
            let stopped = Instant::now();
            gate.stop();
            assert_eq!(in_flight.join().unwrap(), Err(Failure::Stopped));
            assert_eq!(waiting.join().unwrap(), Err(Failure::Stopped));
            assert!(stopped.elapsed() < Duration::from_secs(1));
        });
        // An edit that begins after the stop is not stopped by it.
        assert!(gate.enter(gate.stops(), LIMITS.edit).is_ok());
    }

    #[test]
    fn one_edit_holds_the_gate_at_a_time_and_its_deadline_ends_it() {
        let gate = own_gate();
        let first = gate.enter(gate.stops(), LIMITS.edit).unwrap();
        std::thread::scope(|scope| {
            let second = scope.spawn(|| gate.enter(gate.stops(), LIMITS.edit).is_ok());
            std::thread::sleep(Duration::from_millis(300));
            assert!(!second.is_finished());
            drop(first);
            assert!(second.join().unwrap());
        });
        let late = gate.enter(gate.stops(), Duration::ZERO).unwrap();
        assert_eq!(late.check(), Err(Failure::Unreachable));
        assert_eq!(late.sleep(Duration::from_secs(5)), Err(Failure::Unreachable));
    }

    #[test]
    fn the_new_refusals_have_the_keys_the_webview_knows() {
        let keys = [Failure::KeyStore, Failure::Location, Failure::Billing, Failure::KeyExpired, Failure::Stopped].map(Failure::reason_key);
        assert_eq!(keys, [
            "decline.reason.nanoBananaKeyStore",
            "decline.reason.nanoBananaLocation",
            "decline.reason.nanoBananaBilling",
            "decline.reason.nanoBananaKeyExpired",
            "decline.reason.nanoBananaStopped",
        ]);
        let codes = [KeyError::NoStore, KeyError::Invalid, KeyError::Failed].map(KeyError::code);
        assert_eq!(codes, ["nanoBanana.key.noStore", "nanoBanana.key.invalid", "nanoBanana.key.failed"]);
        assert_eq!(serde_json::to_value(Stored { session_only: true }).unwrap(), json!({ "sessionOnly": true }));
    }

    /// A credential store in memory that counts its reads.
    #[derive(Default)]
    struct FakeStore {
        key: Mutex<Option<String>>,
        gets: AtomicUsize,
        /// The system has no store.
        missing: bool,
        failing: AtomicBool,
    }

    impl FakeStore {
        fn holding(key: &str) -> FakeStore {
            FakeStore { key: Mutex::new(Some(key.to_owned())), ..FakeStore::default() }
        }

        fn fault(&self) -> Option<StoreFault> {
            if self.missing {
                Some(StoreFault::Missing)
            } else if self.failing.load(Ordering::SeqCst) {
                Some(StoreFault::Failed)
            } else {
                None
            }
        }
    }

    impl KeyStore for FakeStore {
        fn get(&self) -> Result<Option<String>, StoreFault> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.fault().map_or_else(|| Ok(self.key.lock().unwrap().clone()), Err)
        }

        fn set(&self, key: &str) -> Result<(), StoreFault> {
            if let Some(fault) = self.fault() {
                return Err(fault);
            }
            *self.key.lock().unwrap() = Some(key.to_owned());
            Ok(())
        }

        fn delete(&self) -> Result<(), StoreFault> {
            if let Some(fault) = self.fault() {
                return Err(fault);
            }
            *self.key.lock().unwrap() = None;
            Ok(())
        }

        fn exists(&self) -> Result<bool, StoreFault> {
            self.fault().map_or_else(|| Ok(self.key.lock().unwrap().is_some()), Err)
        }
    }

    const KEY_A: &str = "AIzaSyA-0123456789abcdefghijklmnopqrstu";
    const KEY_B: &str = "AIzaSyB-0123456789abcdefghijklmnopqrstu";

    fn read(store: &FakeStore, memo: &Mutex<KeyMemo>) -> Result<Option<String>, Failure> {
        read_key(store, memo).map(|key| key.map(|key| key.as_str().to_owned()))
    }

    #[test]
    fn the_key_is_read_once_per_process_and_a_store_or_delete_replaces_what_was_read() {
        let (store, memo) = (FakeStore::holding(KEY_A), Mutex::new(KeyMemo::default()));
        assert_eq!(read(&store, &memo), Ok(Some(KEY_A.to_owned())));
        assert_eq!(read(&store, &memo), Ok(Some(KEY_A.to_owned())));
        assert_eq!(store.gets.load(Ordering::SeqCst), 1);

        assert_eq!(store_key(&store, &memo, KEY_B), Ok(Stored { session_only: false }));
        assert_eq!(read(&store, &memo), Ok(Some(KEY_B.to_owned())));
        assert_eq!(delete_key(&store, &memo), Ok(()));
        assert_eq!(read(&store, &memo), Ok(None));
        assert_eq!(store.gets.load(Ordering::SeqCst), 1, "a write was read back");
        assert_eq!(has_key(&store, &memo), Ok(false));
    }

    #[test]
    fn a_failed_read_is_remembered_until_settings_asks_again() {
        let (store, memo) = (FakeStore::holding(KEY_A), Mutex::new(KeyMemo::default()));
        store.failing.store(true, Ordering::SeqCst);
        assert_eq!(read(&store, &memo), Err(Failure::KeyStore));
        assert_eq!(read(&store, &memo), Err(Failure::KeyStore));
        assert_eq!(store.gets.load(Ordering::SeqCst), 1, "one prompt per region");

        // The keychain was unlocked, and Settings opened.
        store.failing.store(false, Ordering::SeqCst);
        assert_eq!(has_key(&store, &memo), Ok(true));
        assert_eq!(read(&store, &memo), Ok(Some(KEY_A.to_owned())));
        assert_eq!(store.gets.load(Ordering::SeqCst), 2);

        // A failed write forgets the old answer instead of serving it.
        store.failing.store(true, Ordering::SeqCst);
        assert_eq!(store_key(&store, &memo, KEY_B), Err(KeyError::Failed));
        assert_eq!(read(&store, &memo), Err(Failure::KeyStore));
    }

    #[test]
    fn with_no_store_the_key_is_kept_for_this_run_only() {
        let (store, memo) = (FakeStore { missing: true, ..FakeStore::default() }, Mutex::new(KeyMemo::default()));
        assert_eq!(has_key(&store, &memo), Err(KeyError::NoStore));
        assert_eq!(read(&store, &memo), Ok(None));
        assert_eq!(store_key(&store, &memo, KEY_A), Ok(Stored { session_only: true }));
        assert_eq!(read(&store, &memo), Ok(Some(KEY_A.to_owned())));
        assert_eq!(has_key(&store, &memo), Ok(true));
        assert_eq!(delete_key(&store, &memo), Ok(()));
        assert_eq!(read(&store, &memo), Ok(None));
        assert_eq!(has_key(&store, &memo), Err(KeyError::NoStore));
    }

    #[test]
    fn a_key_is_checked_before_it_is_stored() {
        assert_eq!(valid_key(&format!("  {KEY_A}\r\n")), Ok(KEY_A));
        for bad in [
            "",
            "AIzaShort",
            "AIzaSyA-0123456789\r\nabcdefghijklmnopqrstu",
            "AIzaSyA-0123456789 abcdefghijklmnopqrstu",
            "AIzaSyA-0123456789\u{e9}bcdefghijklmnopqrstu",
            &"A".repeat(3000),
        ] {
            assert_eq!(valid_key(bad), Err(KeyError::Invalid), "{bad:?}");
        }
        let (store, memo) = (FakeStore::holding(KEY_A), Mutex::new(KeyMemo::default()));
        assert_eq!(store_key(&store, &memo, "a\nb"), Err(KeyError::Invalid));
        assert_eq!(store.key.lock().unwrap().as_deref(), Some(KEY_A));
        // A key stored before the check is refused as a key, not as a lost link.
        assert!(matches!(Client::new(ENDPOINT, "bad\r\nkey", LIMITS, &GATE), Err(Failure::KeyRejected)));
    }

    /// The real API, once, on a real page crop. Costs about $0.05.
    ///
    /// Paths must be absolute: the test runs from `src-tauri`.
    /// GEMINI_API_KEY=... NANO_BANANA_PAGE=/abs/page.png NANO_BANANA_BOX=x,y,w,h NANO_BANANA_OUT=/abs/out.png \
    /// cargo test -p manga-cleaner nano_banana_live -- --ignored --nocapture
    #[test]
    #[ignore = "spends money: needs GEMINI_API_KEY and a page"]
    fn nano_banana_live() {
        let key = std::env::var("GEMINI_API_KEY").unwrap();
        let page = cleaner_core::image::decode(&std::fs::read(std::env::var("NANO_BANANA_PAGE").unwrap()).unwrap()).unwrap();
        let numbers: Vec<i64> = std::env::var("NANO_BANANA_BOX").unwrap().split(',').map(|n| n.parse().unwrap()).collect();
        let fitted = fitted_over(&page, Rect::new(numbers[0], numbers[1], numbers[2] as u32, numbers[3] as u32));
        let config = Config { model: Model::DEFAULT, instruction: None };
        let live = Client::new(ENDPOINT, &key, LIMITS, &GATE).unwrap();
        let edited = render_with(&live, &config, &page, &fitted, None, None).unwrap();
        println!("{}", edited.record);
        let out = std::env::var("NANO_BANANA_OUT").unwrap();
        std::fs::write(out, cleaner_core::image::encode_png_preview(&edited.rendered.pixels).unwrap()).unwrap();
    }
}
