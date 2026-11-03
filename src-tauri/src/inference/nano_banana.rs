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
//! - The key lives in the OS credential store. It never reaches the webview,
//!   `settings.json` or a log.
//! - No automatic run reaches this rung (`run.rs#reachable_automatically`).
//!   A person names it per region, or confirms a batch with its cost.

use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use cleaner_core::engines::model::{Error as ModelError, Rendered};
use cleaner_core::engines::model::applied_mask;
use cleaner_core::engines::nano_banana::{group_for, AnswerError, Model, Prepared, SendRegion};
use cleaner_core::engines::postprocess::Choice;
use cleaner_core::fit::Fitted;
use cleaner_core::image::Raster;
use cleaner_core::mask::{Mask, Rect};
use serde_json::{json, Value};

use super::secrets::KEYRING_SERVICE;

pub const MODEL_KEY: &str = "nanoBananaModel";
/// What the person asks Nano Banana to do beyond the built-in removal.
pub const PROMPT_KEY: &str = "nanoBananaPrompt";
/// The longest instruction sent, in characters.
pub const MAX_INSTRUCTION_CHARS: usize = 600;

const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const KEYRING_ACCOUNT: &str = "nano-banana-api-key";
const TIMEOUT: Duration = Duration::from_secs(300);
const RETRY_DELAY: Duration = Duration::from_secs(4);
/// A 2K answer in base64 with room to spare.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
/// How many crops of one region are offered before giving up. A refused crop is
/// not billed, so this bounds time and not money.
const MAX_CROPS: usize = 4;

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
    KeyRejected,
    ModelRejected,
    RateLimited,
    Unreachable,
    /// A safety filter said no. Manga trips it, and another crop would not change
    /// what the page shows.
    Blocked,
    /// `IMAGE_RECITATION`: refused for this crop, not billed. The same sound effect
    /// over screentone was refused at 1:1 and 9:16 and cleaned at 1:4 and 3:4.
    Recited,
    BadAnswer,
    KeptMark,
}

impl Failure {
    pub(crate) fn reason_key(self) -> &'static str {
        match self {
            Failure::NoKey => "decline.reason.nanoBananaNoKey",
            Failure::KeyRejected => "decline.reason.nanoBananaKeyRejected",
            Failure::ModelRejected => "decline.reason.nanoBananaModelRejected",
            Failure::RateLimited => "decline.reason.nanoBananaRateLimited",
            Failure::Unreachable => "decline.reason.nanoBananaUnreachable",
            Failure::Blocked | Failure::Recited => "decline.reason.nanoBananaBlocked",
            Failure::BadAnswer => "decline.reason.nanoBananaBadAnswer",
            Failure::KeptMark => "decline.reason.nanoBananaKeptMark",
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
    /// A 400 that is not about the key: the model may not take a system
    /// instruction, which freeditor met and answered the same way.
    BadRequest,
    /// A 429 or a 5xx: worth one more try, since the server said it made nothing.
    Busy(Failure),
    /// Not retried. A lost connection may have lost an answer already billed.
    Fatal(Failure),
}

struct Client {
    endpoint: String,
    key: String,
    retry_delay: Duration,
    http: reqwest::blocking::Client,
}

impl Client {
    fn new(endpoint: &str, key: String, retry_delay: Duration) -> Result<Client, Failure> {
        // No redirects: the key is a header, and a redirect could carry it off Google.
        let http = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Failure::Unreachable)?;
        Ok(Client { endpoint: endpoint.to_owned(), key, retry_delay, http })
    }

    fn call(&self, model: &str, body: &Value) -> Reply {
        let sent = self
            .http
            .post(format!("{}/{model}:generateContent", self.endpoint))
            .header("content-type", "application/json")
            .header("x-goog-api-key", &self.key)
            .body(body.to_string())
            .send();
        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                // `without_url`, and the key is only ever a header.
                eprintln!("manga-cleaner: Nano Banana request failed: {}", error.without_url());
                return Reply::Fatal(Failure::Unreachable);
            }
        };
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        if response.take(MAX_RESPONSE_BYTES).read_to_end(&mut bytes).is_err() {
            return Reply::Fatal(Failure::Unreachable);
        }
        if status == 200 {
            return match serde_json::from_slice(&bytes) {
                Ok(answer) => Reply::Answer(answer),
                Err(_) => Reply::Fatal(Failure::BadAnswer),
            };
        }
        let text = String::from_utf8_lossy(&bytes);
        eprintln!("manga-cleaner: Nano Banana answered {status}: {}", text.chars().take(300).collect::<String>());
        match status {
            401 | 403 => Reply::Fatal(Failure::KeyRejected),
            400 if text.contains("API_KEY_INVALID") => Reply::Fatal(Failure::KeyRejected),
            400 => Reply::BadRequest,
            404 => Reply::Fatal(Failure::ModelRejected),
            429 => Reply::Busy(Failure::RateLimited),
            _ => Reply::Busy(Failure::Unreachable),
        }
    }

    /// The edit, with what freeditor learned: drop the system instruction on a
    /// bad request. A busy server gets one more try. A key or a model that was
    /// refused does not, because it would only be refused again.
    fn edit(&self, model: &str, png: &[u8], region: SendRegion, instruction: Option<&str>) -> Result<Vec<u8>, Failure> {
        let mut with_system = true;
        let mut busy_retried = false;
        loop {
            match self.call(model, &request_body(png, region, instruction, with_system)) {
                Reply::Answer(answer) => return read_answer(&answer),
                Reply::Fatal(failure) => return Err(failure),
                Reply::BadRequest if with_system => with_system = false,
                Reply::BadRequest => return Err(Failure::ModelRejected),
                Reply::Busy(failure) if busy_retried => return Err(failure),
                Reply::Busy(_) => {
                    busy_retried = true;
                    std::thread::sleep(self.retry_delay);
                }
            }
        }
    }
}

/// A region edited, and how, for the patch's record.
pub(crate) struct Edited {
    pub rendered: Rendered,
    pub record: Value,
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
/// few are enough, and each is a whole decoded image.
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
    -> Result<Edited, &'static str> {
    let rendered = prepared.finish(answer).map_err(|error| match error {
        AnswerError::KeptMark => Failure::KeptMark.reason_key(),
        AnswerError::Render(error) => model_reason(error),
        other => {
            eprintln!("manga-cleaner: Nano Banana answer refused: {other}");
            Failure::BadAnswer.reason_key()
        }
    })?;
    let crop = region.crop;
    Ok(Edited {
        rendered,
        record: json!({
            "model": config.model.id,
            "image_size": region.tier.wire(),
            "aspect_ratio": region.ratio,
            "crop": [crop.x, crop.y, crop.w, crop.h],
            "crops_refused": refused,
            "instruction": config.instruction.is_some(),
            "shared_answer": shared,
        }),
    })
}

fn render_with(client: &Client, config: &Config, page: &Raster, fitted: &Fitted, finishing: Option<Choice>,
    share: Option<&Neighbours>) -> Result<Edited, &'static str> {
    // An answer an earlier region asked for, with this one marked in it: no new request.
    if let Some(share) = share {
        let applied = applied_mask(fitted, page.width, page.height);
        if let Some((answer, region)) = take_shared(share, config, &applied, page) {
            let prepared = Prepared::new(page, fitted, region, finishing, &[]).map_err(model_reason)?;
            return finish(config, &prepared, region, &answer, 0, true);
        }
    }
    let others: Vec<Mask> = share.map(|share| share.others.iter().map(|(_, mask)| mask.clone()).collect()).unwrap_or_default();
    let group = group_for(page, fitted, config.model, &others).map_err(model_reason)?;
    let companions: Vec<Mask> = group.members.iter().map(|at| others[*at].clone()).collect();
    for (refused, region) in group.regions.into_iter().take(MAX_CROPS).enumerate() {
        let prepared = Prepared::new(page, fitted, region, finishing, &companions).map_err(model_reason)?;
        let answer = match client.edit(config.model.id, prepared.marked_png(), region, config.instruction.as_deref()) {
            Ok(answer) => answer,
            Err(Failure::Recited) => continue,
            Err(failure) => return Err(failure.reason_key()),
        };
        let edited = finish(config, &prepared, region, &answer, refused, false)?;
        if let Some(share) = share {
            keep_shared(share, config, region, &answer, &group.members);
        }
        return Ok(edited);
    }
    Err(Failure::Recited.reason_key())
}

/// `fitted` on `page`, edited by Gemini with the stored key. `share` names the
/// page's other regions, which ride in the same request when they fit its crop.
pub(crate) fn render(config: &Config, page: &Raster, fitted: &Fitted, finishing: Option<Choice>,
    share: Option<&Neighbours>) -> Result<Edited, &'static str> {
    let key = read_key().map_err(|_| Failure::Unreachable.reason_key())?
        .ok_or(Failure::NoKey.reason_key())?;
    let client = Client::new(ENDPOINT, key, RETRY_DELAY).map_err(Failure::reason_key)?;
    render_with(&client, config, page, fitted, finishing, share)
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|_| "OS credential store initialization failed".to_string())
}

fn read_key() -> Result<Option<String>, String> {
    match entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("OS credential store read failed".to_string()),
    }
}

/// Whether a key is stored, asked without reading it, so opening Settings never
/// prompts for the keychain.
#[tauri::command]
pub async fn nano_banana_has_key() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        super::secrets::macos_account_exists(KEYRING_ACCOUNT)
    }
    #[cfg(not(target_os = "macos"))]
    {
        read_key().map(|key| key.is_some())
    }
}

/// Store the key. Write-only: nothing ever reads it back to the webview.
#[tauri::command]
pub async fn store_nano_banana_key(key: String) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("the key is empty".to_string());
    }
    #[cfg(target_os = "macos")]
    let written = crate::macos_keychain::set_password(KEYRING_SERVICE, KEYRING_ACCOUNT, key);
    #[cfg(not(target_os = "macos"))]
    let written = entry()?.set_password(key);
    written.map_err(|_| "OS credential store write failed".to_string())
}

#[tauri::command]
pub async fn delete_nano_banana_key() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let deleted = crate::macos_keychain::delete_password(KEYRING_SERVICE, KEYRING_ACCOUNT);
    #[cfg(not(target_os = "macos"))]
    let deleted = entry()?.delete_credential();
    match deleted {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("OS credential store deletion failed".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::engines::nano_banana::Tier;
    use cleaner_core::image::{BitDepth, ColorMode};
    use cleaner_core::mask::{Mask, Rect};
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A fake Gemini: answers each request with the next canned reply, and keeps
    /// what it was sent.
    struct Server {
        endpoint: String,
        seen: Arc<Mutex<Vec<(String, Value)>>>,
    }

    fn serve(replies: Vec<(u16, String)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1beta/models", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for (status, reply) in replies {
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
                log.lock().unwrap().push((head, serde_json::from_slice(&bytes[header_end..]).unwrap_or(Value::Null)));
                write!(stream, "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
        });
        Server { endpoint, seen }
    }

    fn client(server: &Server) -> Client {
        Client::new(&server.endpoint, "test-key".to_owned(), Duration::ZERO).unwrap()
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
        assert_eq!(client(&server).edit("gemini-x", b"png", a_region(), None).unwrap(), b"abc");
        let seen = server.seen.lock().unwrap();
        let (head, body) = &seen[0];
        assert!(head.starts_with("POST /v1beta/models/gemini-x:generateContent "), "{head}");
        assert!(head.to_ascii_lowercase().contains("x-goog-api-key: test-key"));
        assert!(!head.lines().next().unwrap().contains("test-key"), "the key stays out of the URL");
        assert_eq!(body["generationConfig"]["imageConfig"], json!({ "imageSize": "512", "aspectRatio": "4:1" }));
        assert!(body["systemInstruction"].is_object());
        assert_eq!(body["contents"][0]["parts"][1]["inlineData"]["data"], "cG5n");
    }

    #[test]
    fn a_bad_request_retries_once_without_the_system_instruction() {
        let server = serve(vec![(400, "{}".into()), (200, image_reply(b"abc"))]);
        assert_eq!(client(&server).edit("m", b"png", a_region(), None).unwrap(), b"abc");
        let seen = server.seen.lock().unwrap();
        assert!(seen[0].1["systemInstruction"].is_object());
        assert!(seen[1].1.get("systemInstruction").is_none());
    }

    #[test]
    fn a_second_bad_request_stops() {
        let server = serve(vec![(400, "{}".into()), (400, "{}".into())]);
        assert_eq!(client(&server).edit("m", b"png", a_region(), None), Err(Failure::ModelRejected));
        assert_eq!(server.seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_rejected_key_is_never_retried() {
        for (status, reply) in [(403, "{}"), (401, "{}"), (400, r#"{"error":{"details":[{"reason":"API_KEY_INVALID"}]}}"#)] {
            let server = serve(vec![(status, reply.to_owned())]);
            assert_eq!(client(&server).edit("m", b"png", a_region(), None), Err(Failure::KeyRejected));
            assert_eq!(server.seen.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn a_busy_server_gets_one_more_try_only() {
        let server = serve(vec![(429, "{}".into()), (200, image_reply(b"abc"))]);
        assert_eq!(client(&server).edit("m", b"png", a_region(), None).unwrap(), b"abc");
        let server = serve(vec![(429, "{}".into()), (429, "{}".into())]);
        assert_eq!(client(&server).edit("m", b"png", a_region(), None), Err(Failure::RateLimited));
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
        let lost = Client::new(&endpoint, "k".into(), Duration::ZERO).unwrap();
        assert_eq!(lost.edit("m", b"png", a_region(), None), Err(Failure::Unreachable));
        assert!(!accepted.join().unwrap(), "a second request was sent");
    }

    #[test]
    fn an_unknown_model_says_so() {
        let server = serve(vec![(404, "{}".into())]);
        assert_eq!(client(&server).edit("m", b"png", a_region(), None), Err(Failure::ModelRejected));
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
        client(&server).edit("m", b"png", a_region(), Some("keep the hatching")).unwrap();
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
        let redirecting = Client::new(&endpoint, "test-key".into(), Duration::ZERO).unwrap();
        assert!(redirecting.edit("m", b"png", a_region(), None).is_err());
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
        let server = serve(vec![(200, image_reply(&answer_of(512, 512, [150, 170, 230])))]);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let refused = render_with(&client(&server), &config, &page, &fitted, None, None).err();
        assert_eq!(refused, Some("decline.reason.nanoBananaKeptMark"));
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

    /// A client whose every request is refused at once, so a test that must make
    /// none fails fast and does not wait on a timeout.
    fn nowhere() -> Client {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1beta/models", listener.local_addr().unwrap());
        drop(listener);
        Client::new(&endpoint, "k".into(), Duration::ZERO).unwrap()
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
        let server = serve(vec![(200, image_reply(b"not an image"))]);
        let config = Config { model: Model::DEFAULT, instruction: None };
        let refused = render_with(&client(&server), &config, &page, &fitted, None, None).err();
        assert_eq!(refused, Some("decline.reason.nanoBananaBadAnswer"));
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
        let live = Client::new(ENDPOINT, key, RETRY_DELAY).unwrap();
        let edited = render_with(&live, &config, &page, &fitted, None, None).unwrap();
        println!("{}", edited.record);
        let out = std::env::var("NANO_BANANA_OUT").unwrap();
        std::fs::write(out, cleaner_core::image::encode_png_preview(&edited.rendered.pixels).unwrap()).unwrap();
    }
}
