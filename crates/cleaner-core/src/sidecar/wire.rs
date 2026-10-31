//! The shapes that cross the socket, and the error taxonomy they carry.
//!
//! Every type here is one JSON document in [`super`]'s protocol, named for the
//! endpoint it belongs to. The protocol itself - endpoints, statuses, when each
//! error kind is correct - is documented once, in [`super`], because a reader
//! implementing the Python half needs it as prose and not as a list of structs.
//!
//! ## Why the pixels are raw bytes and not a PNG
//!
//! An [`Image`] is base64 over **interleaved 8-bit samples**, with the width,
//! the height and the channel count stated beside them. No codec is named on
//! the wire at all.
//!
//! That is the one decision in this file worth defending, because a PNG would
//! be a third the size. It is refused because a PNG carries a
//! colour mode, a bit depth, a palette, a `tRNS` chunk, an ICC profile and an
//! sRGB intent, and every one of them is a thing the two halves of this
//! protocol could disagree about silently. libvips was reversed from this
//! project over exactly one such
//! disagreement - `spngload.c` destroying the palette at load. A sidecar
//! written in a language whose imaging library "helpfully" promotes grayscale
//! to RGB, or attaches a working-space profile on save, would return pixels
//! that composite wrong in a way no test in this repository would catch,
//! because the page would still be the right size and roughly the right colour.
//!
//! Raw samples cannot carry any of that. The page's own mode, depth, palette
//! and profile never leave this process: [`crate::engines::flux`] converts to
//! 8-bit RGB on the way out and writes the answer back into the page's own
//! samples on the way in, which is the same discipline
//! [`crate::engines::model`] applies to a tensor. What the sidecar sees is a
//! rectangle of numbers.
//!
//! The cost is bandwidth, and over loopback it is not a cost: a 768² crop is
//! 1.7 MB of samples and 2.4 MB once base64 has taken its third, against a
//! render that is budgeted ten to sixty
//! seconds for.

use serde::{Deserialize, Serialize};

/// The protocol revision this build speaks. A sidecar answering anything else
/// on `GET /v1/health` is refused rather than negotiated with - there is one
/// version, and the day there are two is the day this becomes a decision
/// somebody has to make deliberately.
pub const PROTOCOL: u32 = 1;

/* ------------------------------------------------------------------ */
/* Pixels                                                              */
/* ------------------------------------------------------------------ */

/// A rectangle of interleaved 8-bit samples, base64'd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// 3 for `rgb8`, 1 for `gray8`. Stated as well as implied by `encoding`, so
    /// a reply whose two disagree is caught rather than reshaped.
    pub channels: u32,
    pub encoding: Encoding,
    /// Base64, standard alphabet, padded. `width × height × channels` bytes
    /// once decoded, and refused when it is not.
    pub data: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Rgb8,
    Gray8,
}

impl Encoding {
    pub fn channels(self) -> u32 {
        match self {
            Encoding::Rgb8 => 3,
            Encoding::Gray8 => 1,
        }
    }
}

impl Image {
    pub fn new(width: u32, height: u32, encoding: Encoding, samples: &[u8]) -> Image {
        Image {
            width,
            height,
            channels: encoding.channels(),
            encoding,
            data: b64::encode(samples),
        }
    }

    /// The samples, checked against the geometry the same document declares.
    ///
    /// Three ways to be wrong and one answer: a reply this cannot trust is a
    /// fault, never a patch. Silently accepting a short buffer would composite
    /// uninitialised memory into a user's page.
    pub fn samples(&self) -> Option<Vec<u8>> {
        if self.channels != self.encoding.channels() {
            return None;
        }
        let want = (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(self.channels as usize)?;
        let bytes = b64::decode(&self.data)?;
        (bytes.len() == want).then_some(bytes)
    }
}

/* ------------------------------------------------------------------ */
/* What the sidecar reports about its own memory                       */
/* ------------------------------------------------------------------ */

/// Rule 9's fifth guard, made
/// into numbers the sidecar reports and this process records.
///
/// Rule 9 is explicit that returning to the floor between regions is *"a
/// measurement, not an assumption"*, and a measurement needs an
/// instrument. This is the instrument: the current reading before and after a
/// region says whether the buffers went, `peak` says what the high-water was,
/// and `cache` says whether the cap took.
///
/// **Which quantity these fields hold, and why it is not the resident set
/// size.** An earlier revision of this type said the peak was *"one `getrusage`
/// call anywhere this rung can run"*. That was the wrong instrument, and it was
/// wrong in the unsafe direction. On macOS, resident set size counts no
/// unified-memory buffer: MLX allocates the model through Metal, the kernel
/// accounts those pages to the process under the `IOAccelerator` tag, and
/// `ru_maxrss` does not see them. Measured on one Apple M5, a 512² FLUX render
/// peaked at **6.58 GB** of physical footprint against **2.88 GB** of resident,
/// and a 1024² render at **11.46 GB** against **2.90 GB** - the resident
/// reading barely moved while the real cost grew by nearly 5 GB. Rule 9's
/// subject is the user's machine, not our accounting, so these fields now carry
/// the **physical footprint** - the figure `/usr/bin/time -l` prints as
/// `peak memory footprint` and Activity Monitor shows as memory used.
///
/// The names are historical and the [`Self::instrument`] field is what settles
/// the question, because a field whose meaning has to be remembered is a field
/// that will be misread again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReport {
    /// The process's memory use **now**, in bytes, in whichever quantity
    /// [`Self::instrument`] names. Optional because not every runtime can ask
    /// its own operating system for it cheaply, and a field the sidecar has to
    /// fake is worse than one it may omit.
    #[serde(default)]
    pub rss_bytes: Option<u64>,
    /// The high-water mark since the process started, in bytes, in whichever
    /// quantity [`Self::instrument`] names. **Required.** It is the number the
    /// parent's own cap is checked against, which is exactly why it must be the
    /// footprint and not the resident set.
    pub peak_rss_bytes: u64,
    /// What the backend's buffer cache is holding, in bytes, where the backend
    /// can say. This is the number rule 9's second guard is about, and the
    /// difference between capping it and purging it is visible only here.
    #[serde(default)]
    pub cache_bytes: Option<u64>,
    /// Names the quantity the two fields above hold - `"phys_footprint"`
    /// normally, or `"ru_maxrss"` from a sidecar that could not read a Mach
    /// ledger. Absent from an older sidecar, which is itself the signal that
    /// the numbers may be resident readings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    /// Physical footprint now, under its own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint_bytes: Option<u64>,
    /// Peak physical footprint, under its own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_footprint_bytes: Option<u64>,
    /// Resident set size now. Kept so the disagreement between the two
    /// instruments stays auditable rather than being averaged away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_bytes: Option<u64>,
    /// Peak resident set size - the quantity once published as the headline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_resident_bytes: Option<u64>,
    /// The inference backend's own high-water mark, where it can say. A third
    /// instrument, agreeing with neither of the others: it counts only what the
    /// backend asked for, so it misses the driver's overhead, and it can exceed
    /// the footprint because it charges buffers that were recycled rather than
    /// held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_peak_bytes: Option<u64>,
}

/* ------------------------------------------------------------------ */
/* GET /v1/health                                                      */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub protocol: u32,
    pub sidecar_version: String,
    pub state: State,
    /// The backend the sidecar was spawned for. `None` before it has resolved
    /// one, which is a startup state and not a configuration.
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<Model>,
    /// What the selected model's quantized weights occupy. **The floor, and
    /// never the budget** - rule 9's fourth guard, which is why this and
    /// `working_set_bytes` are two fields and not one sum.
    #[serde(default)]
    pub weights_bytes: Option<u64>,
    /// Everything the parameter count does not predict: latents, reference
    /// tokens, the VAE's peak and whatever the allocator keeps. Absent means
    /// *unbounded*, and an unbounded backend is refused rather than defaulted.
    #[serde(default)]
    pub working_set_bytes: Option<u64>,
    /// How `working_set_bytes` was arrived at. The same distinction
    /// [`crate::memory::Basis`] keeps for the same reason.
    #[serde(default)]
    pub basis: Option<Basis>,
    /// Which of the contract's guards are in force, by name.
    #[serde(default)]
    pub applied: Vec<String>,
    /// Which backends **this virtual environment could actually run**, by wire
    /// name - the ids whose Python packages import.
    ///
    /// A question only the sidecar can answer. The parent finds an install by
    /// looking for a `pyvenv.cfg` and an interpreter beside it
    /// ([`super::interpreter_in`]) and cannot see which packages are inside, and
    /// since [`super::backend::Backend::Sdnq`] a venv may legitimately hold
    /// either backend's dependencies or both. Reported here so a backend that
    /// cannot be imported is refused **from the handshake**, before a model load
    /// is attempted, rather than surfacing as an `ImportError` at open.
    ///
    /// **Empty means "this sidecar does not answer the question"**, not "nothing
    /// is installed": the field was added after the protocol shipped, so an
    /// older sidecar omits it and must not thereby be refused. A caller checks
    /// membership only when the list is non-empty.
    #[serde(default)]
    pub backends: Vec<String>,
    #[serde(default)]
    pub memory: Option<MemoryReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Serving, with no model resident. The state after startup and after
    /// `POST /v1/release`.
    Idle,
    /// Building the model. `GET /v1/health` must keep answering through this.
    Loading,
    /// A model is resident and nothing is in flight.
    Ready,
    /// A render is in flight. A second one is answered `409 busy`, never
    /// queued - one region in flight is rule 6, and a queue inside the sidecar
    /// is a queue this process cannot see the depth of.
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Basis {
    /// Instrumented on this machine, under this harness.
    Measured,
    /// Stated by whoever packaged the model. Honest, and not a measurement -
    /// keeping the two apart is what matters, since conflating them is what
    /// happens when the two
    /// are written down as the same kind of thing.
    Declared,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    /// `"int4"`, `"q4_k_m"`, `"bf16"` - the packaging's own word, uninterpreted.
    #[serde(default)]
    pub quantization: Option<String>,
    /// Where the weights came from, for the provenance record. A repository
    /// name or a path; never a URL this process will fetch.
    #[serde(default)]
    pub source: Option<String>,
}

/* ------------------------------------------------------------------ */
/* POST /v1/open                                                       */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRequest {
    pub backend: String,
    pub model: String,
    pub limits: Limits,
    /// The guards this build will not run without, by name. The sidecar applies
    /// them and echoes the ones it applied; anything missing from the echo
    /// fails the open. That is rule 9's *"a guard that does not name its
    /// backend is a guard for one backend"* turned into something a machine
    /// checks - the reference implementation's four guards were all
    /// individually defensible and all silently absent, and an echo is what
    /// would have caught that.
    pub require: Vec<String>,
}

/// The numbers the gate arrived at, in bytes, sent to the process that has to
/// honour them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Everything the sidecar may hold, resident, at any moment.
    pub total_bytes: u64,
    /// Of that, what the backend's buffer cache may keep between regions.
    pub cache_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenReply {
    pub applied: Vec<String>,
    #[serde(default)]
    pub weights_bytes: Option<u64>,
    #[serde(default)]
    pub working_set_bytes: Option<u64>,
    #[serde(default)]
    pub basis: Option<Basis>,
    #[serde(default)]
    pub memory: Option<MemoryReport>,
}

/* ------------------------------------------------------------------ */
/* POST /v1/render                                                     */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderRequest {
    /// The region's id, echoed into the sidecar's own log so two processes'
    /// records of one region can be lined up.
    pub region: String,
    /// The crop, at the page's native resolution, including real surrounding
    /// page pixels.
    pub image: Image,
    /// Where the text is, as a single-channel image - **advisory, and named
    /// `hint` so that it cannot be read as anything else.** §4a's table is the
    /// reason: no candidate model for this rung takes a mask, `FLUX.2 [klein]`
    /// edits by reference image, and §5.2 already settled what that means -
    /// *"a mask sent as a reference image is advisory"*. The boundary is
    /// enforced by our compositor, on this side, and §6's metric is what
    /// catches the failures.
    pub hint: Image,
    /// What to remove and what to keep. Sent rather than held in the sidecar so
    /// that changing it is a change to this repository and not to a file the
    /// user installed.
    pub prompt: String,
    pub steps: u32,
    /// Fixed per region and reported in the provenance, because a rung whose
    /// output cannot be reproduced cannot be reviewed.
    pub seed: u64,
    pub guidance: f32,
    /// How long the caller will wait, in milliseconds. Advisory: the caller
    /// gives up on its own clock whatever the sidecar does with this. Sending
    /// it lets a sidecar that knows it cannot finish say so instead of being
    /// killed.
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderReply {
    /// The edited crop, at exactly the geometry that was sent. A reply of any
    /// other size is refused: this rung's model regenerates the whole crop, so
    /// a resize would mean compositing resampled pixels back over the page,
    /// and §5.2 step 4 is explicit that any resample is a last resort with a
    /// named filter.
    pub image: Image,
    pub elapsed_ms: u64,
    pub memory: MemoryReport,
}

/* ------------------------------------------------------------------ */
/* Failure                                                             */
/* ------------------------------------------------------------------ */

/// The body of every non-2xx reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub kind: ErrorKind,
    /// English, for the log. **Never shown to a user** - no English crosses the
    /// seam, and this text comes from
    /// a file the user installed, which is one more reason it cannot be
    /// rendered.
    #[serde(default)]
    pub detail: Option<String>,
}

/// Why a request failed, as the sidecar sees it.
///
/// The taxonomy exists so that this process can tell **a decline from a
/// fault**, which is a distinction kept everywhere else: a decline
/// routes the region down the ladder and lists it in review, a fault fails the
/// page. [`ErrorKind::declines`] is that judgement in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 401. A wrong or missing token. A fault, and a loud one: the port is
    ///      loopback and the secret is 24 bytes from the OS, so this means the
    ///      sidecar on that port is not the one we spawned.
    Unauthorized,
    /// 409. A render is already in flight. A fault rather than a retry, because
    ///      one region in flight is this side's discipline (rule 6) and a second
    ///      request means this side lost count.
    Busy,
    /// 412. `POST /v1/render` before `POST /v1/open`.
    NotOpen,
    /// 422. The request's geometry, encoding or base64 did not check out.
    BadRequest,
    /// 501. The sidecar cannot bound the allocator this backend uses. Rule 9's
    ///      first guard, refused from the far side - a **decline**, because the
    ///      remedy is another rung and not another attempt.
    UnboundedBackend,
    /// 503. The weights are not on disk. A decline, and a quiet one: the user
    ///      installed the sidecar and has not fetched a model, which is an ordinary
    ///      state on the way to a working rung.
    WeightsMissing,
    /// 507. The allocation cap fired. **Rule 9's third guard working exactly as
    ///      specified** - the engine's limit is set so that exceeding it raises,
    ///      which routes the region to rung 2 and loses one region, rather than
    ///      being absorbed into the memory compressor, which loses the machine.
    OutOfMemory,
    /// 500. Anything else.
    Internal,
}

impl ErrorKind {
    /// Whether this is a decline - the region routes down the ladder and the
    /// run continues - rather than a fault that fails the page.
    pub fn declines(self) -> bool {
        matches!(
            self,
            ErrorKind::UnboundedBackend | ErrorKind::WeightsMissing | ErrorKind::OutOfMemory
        )
    }
}

/* ------------------------------------------------------------------ */
/* base64                                                              */
/* ------------------------------------------------------------------ */

/// Standard base64, padded - RFC 4648 §4.
///
/// Forty lines rather than a dependency, on the same reasoning the hand-written
/// HTTP client in [`super::http`] carries. There is one alphabet, one padding
/// rule and no configuration, and the round trip is pinned by a test over every
/// byte value and every length modulo three.
pub mod b64 {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
            out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
            out.push(if chunk.len() > 1 { ALPHABET[(n >> 6 & 63) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
        }
        out
    }

    /// `None` for anything that is not well-formed base64. Whitespace is
    /// **not** skipped: a JSON string is not a MIME body, nothing this protocol
    /// speaks to has a reason to wrap at 76 columns, and quietly accepting it
    /// would mean quietly accepting whatever else got into the field.
    pub fn decode(text: &str) -> Option<Vec<u8>> {
        let bytes = text.as_bytes();
        // `% 4` rather than `usize::is_multiple_of`, which reads better and is
        // stable only from 1.87 - past this workspace's declared 1.82 floor.
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        for quad in bytes.chunks(4) {
            let pad = quad.iter().filter(|c| **c == b'=').count();
            if pad > 2 || (pad > 0 && quad[3] != b'=') || (pad == 2 && quad[2] != b'=') {
                return None;
            }
            let mut n = 0u32;
            for (index, byte) in quad.iter().enumerate() {
                let value = if *byte == b'=' && index >= 4 - pad {
                    0
                } else {
                    ALPHABET.iter().position(|c| c == byte)? as u32
                };
                n = n << 6 | value;
            }
            out.push((n >> 16) as u8);
            if pad < 2 {
                out.push((n >> 8) as u8);
            }
            if pad < 1 {
                out.push(n as u8);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_every_byte_and_every_tail_length() {
        let all: Vec<u8> = (0..=255u8).collect();
        for length in 0..=all.len() {
            let slice = &all[..length];
            let text = b64::encode(slice);
            assert_eq!(text.len() % 4, 0, "length {length} is not a whole quantum");
            assert_eq!(b64::decode(&text).as_deref(), Some(slice), "length {length}");
        }
    }

    #[test]
    fn base64_matches_the_alphabet_the_other_half_will_use() {
        // The three canonical vectors, so a Python `base64.b64encode` and this
        // cannot differ about padding without a test saying so.
        assert_eq!(b64::encode(b"f"), "Zg==");
        assert_eq!(b64::encode(b"fo"), "Zm8=");
        assert_eq!(b64::encode(b"foo"), "Zm9v");
    }

    #[test]
    fn malformed_base64_is_refused_rather_than_salvaged() {
        for bad in ["Zg=", "Zg===", "Z g==", "Zm9v\n", "Z===", "!!!!"] {
            assert_eq!(b64::decode(bad), None, "{bad:?} decoded");
        }
    }

    /// A reply whose declared geometry and actual payload disagree is a fault.
    /// Accepting the short buffer would composite whatever followed it in
    /// memory into the user's page.
    #[test]
    fn an_image_is_checked_against_its_own_declared_geometry() {
        let good = Image::new(2, 2, Encoding::Rgb8, &[0u8; 12]);
        assert_eq!(good.samples().map(|s| s.len()), Some(12));

        let mut short = good.clone();
        short.data = b64::encode(&[0u8; 9]);
        assert_eq!(short.samples(), None);

        let mut lying = good.clone();
        lying.channels = 1;
        assert_eq!(lying.samples(), None, "a channel count the encoding contradicts");
    }

    /// The three that route a region to rung 2, and the four that fail the
    /// page. Written out rather than derived, because getting one of them the
    /// wrong way round is the difference between losing a region and losing a
    /// page.
    #[test]
    fn the_error_taxonomy_separates_declines_from_faults() {
        for kind in [ErrorKind::UnboundedBackend, ErrorKind::WeightsMissing, ErrorKind::OutOfMemory]
        {
            assert!(kind.declines(), "{kind:?}");
        }
        for kind in [
            ErrorKind::Unauthorized,
            ErrorKind::Busy,
            ErrorKind::NotOpen,
            ErrorKind::BadRequest,
            ErrorKind::Internal,
        ] {
            assert!(!kind.declines(), "{kind:?}");
        }
    }

    /// The wire spelling is part of the protocol: the Python half will match on
    /// these strings.
    #[test]
    fn the_error_kinds_spell_themselves_in_snake_case() {
        let json = serde_json::to_string(&ErrorKind::OutOfMemory).unwrap();
        assert_eq!(json, "\"out_of_memory\"");
        let parsed: ErrorKind = serde_json::from_str("\"weights_missing\"").unwrap();
        assert_eq!(parsed, ErrorKind::WeightsMissing);
    }
}
