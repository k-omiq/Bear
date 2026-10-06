//! The script gate: clean Japanese, leave English alone.
//!
//! The asymmetry is the whole design:
//!
//! > Low confidence → drop the box and flag it, because leaving Japanese is
//! > recoverable and painting over English is not.
//!
//! So the gate's default is **do not clean**, and a region reaches the pipeline
//! only on a confident CJK verdict. Everything the gate refuses appears in
//! review with a *clean anyway* action, because a page that reports "cleaned"
//! with text still on it and no way to find it is worse than one that reports
//! what it skipped.

use std::path::Path;

use crate::balloon::Detected;
use crate::detect::{Region, Segmentation};
use crate::image::Raster;

pub mod hayai;
pub mod lines;
pub mod ocr;
mod osd;

pub use lines::{Orientation, TextLine};
pub use hayai::{Hayai, HayaiError};
pub use ocr::{Ocr, OcrError, Reading, cjk_share};
pub use osd::{LineScript, Osd, OsdError};

/// What a run does with text the balloon question puts outside a balloon.
///
/// The rule is *"Gate does not decide. Region is cleaned only if the user opts
/// in, and always enters review."* The default is the first half and
/// [`OutsideText::Clean`] is the opt-in - a property of the run, chosen in the
/// tool window beside the engine that kind of text starts on, and off unless
/// somebody turned it on. Sound effects
/// and lettering over art are still what §3 conceded rather than classified,
/// so the opt-in is not a script verdict: nothing is read, everything the
/// balloon question left outside goes to the ladder, and the person who
/// turned it on is the one who decided that was right for this chapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutsideText {
    /// Hold it back for review under *"text outside a speech bubble"*.
    #[default]
    Review,
    /// Clean it, on the run's own say-so.
    Clean,
}

impl OutsideText {
    /// The seam's name for it: `outsideBubbles` on `runClean`, `"review"` or
    /// `"clean"`. Anything else - absent, or a value this build does not know -
    /// is the default, because the default is the direction that leaves text
    /// rather than the one that paints over it.
    pub fn from_arg(name: Option<&str>) -> OutsideText {
        match name {
            Some("clean") => OutsideText::Clean,
            _ => OutsideText::Review,
        }
    }
}

/// What the gate decided, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Confidently CJK. Clean it.
    Clean { script: String },
    /// Confidently something else - Latin, Cyrillic, Greek. Leave it, and say
    /// so: this is the case that must never be silent.
    NotJapanese { script: String },
    /// The model said too little to act on. Held back and flagged, the same as
    /// a refusal, because acting on a guess here is the unrecoverable
    /// direction.
    Uncertain,
    /// Out of a balloon, where §3 says the gate does not decide at all.
    OutOfBalloon,
    /// Out of a balloon, and the run opted in. §3's table has always read
    /// *"cleaned only if the user opts in"* for this row; [`OutsideText::Clean`]
    /// is that opt-in. No script was read - the gate does not decide out of a
    /// balloon whichever way the run leans - so the verdict carries none.
    OptedIn,
    /// The text reader read the region and found no lettering: art the
    /// detector or the mask took for text. Held back, because cleaning art
    /// is the unrecoverable direction ([`text_checked`]).
    NotText,
}

impl Verdict {
    pub fn cleans(&self) -> bool {
        matches!(self, Verdict::Clean { .. } | Verdict::OptedIn)
    }

    /// The i18n key review lists this under. `review.reason.*` in
    /// `src/lib/i18n/en.js`, which is the fixed vocabulary - the first two names
    /// here were invented alongside catalogue entries that already said the same
    /// thing, and the catalogue's names win.
    pub fn reason_key(&self) -> Option<&'static str> {
        match self {
            Verdict::Clean { .. } | Verdict::OptedIn => None,
            Verdict::NotJapanese { .. } => Some("review.reason.gateSkippedNotJapanese"),
            Verdict::Uncertain => Some("review.reason.gateSkippedLowConfidence"),
            Verdict::OutOfBalloon => Some("review.reason.gateSkippedOutsideBubble"),
            Verdict::NotText => Some("review.reason.gateSkippedNotText"),
        }
    }

    /// The `gateSkipCause` the seam carries, which the interface turns back into
    /// [`Verdict::reason_key`]'s key. `None` for a verdict that cleans.
    ///
    /// Three causes, not two. `src/lib/model/types.js` had `'low-confidence'`
    /// and `'outside-bubble'`, and folding `NotJapanese` into the first of them
    /// would state the opposite of what happened: the gate was *confident*, and
    /// being confident is why it left the region alone.
    pub fn skip_cause(&self) -> Option<&'static str> {
        match self {
            Verdict::Clean { .. } | Verdict::OptedIn => None,
            Verdict::NotJapanese { .. } => Some("not-japanese"),
            Verdict::Uncertain => Some("low-confidence"),
            Verdict::OutOfBalloon => Some("outside-bubble"),
            Verdict::NotText => Some("not-text"),
        }
    }
}

/// The labels this gate treats as "clean it".
///
/// **Wider than "Japanese", deliberately.** The model's own confusion mass is
/// Chinese↔Japanese, citing SIW-13, and Phase 0 spike 8 measured a hiragana
/// column coming back `HanS_vert`. Separating Han from kana is a problem this model cannot solve
/// and the product does not need solved: the question the gate is asked is
/// *"is this the Latin typesetting a localiser added?"*, and against that
/// question every CJK label is the same answer.
///
/// Korean and Chinese pages use this same cleaning path. The identifier has
/// explicit horizontal and vertical Hangul labels, so they are positive
/// evidence just like its simplified- and traditional-Han labels.
const CJK_LABELS: [&str; 8] = [
    "Japanese",
    "Japanese_vert",
    "HanS",
    "HanS_vert",
    "HanT",
    "HanT_vert",
    "Hangul",
    "Hangul_vert",
];

fn is_cjk(label: &str) -> bool {
    let base = label.strip_suffix("-dn").unwrap_or(label);
    CJK_LABELS.contains(&base)
}

/// A label that means "I read something, but not a script": `Common` is
/// punctuation and digits, `Joined` and `NULL` are structural. A line that
/// returns one of these has not identified a script and must not be counted as
/// a vote against one.
fn is_abstention(label: &str) -> bool {
    let base = label.strip_suffix("-dn").unwrap_or(label);
    matches!(base, "Common" | "Joined" | "NULL" | "Broken")
}

/// How much evidence a line needs before its verdict counts. One collapsed
/// timestep is a single glyph's worth of opinion; the short columns that
/// came back `Common` and `HanS_vert` in Phase 0 spike 8 were all at this
/// level.
const MIN_LINE_STRENGTH: usize = 2;

/// The scripts a `NotJapanese` verdict may name and be **believed without a
/// second opinion**.
///
/// The script identifier is a 3.7 MB LSTM over a 48-pixel strip, and its
/// failures are not evenly spread. On the alphabets it was trained heavily on -
/// the Latin a localiser types, the Cyrillic and Greek and Hangul and Hebrew
/// and Arabic and Thai that a page might genuinely be in, and Tesseract's own
/// `Fraktur` - a confident answer is an answer. On the long tail it is not: a
/// tall thin column of kana comes back `Tibetan` or `Syriac`, confidently,
/// because those scripts are *also* tall and thin and the model has barely
/// seen them. Measured on the user's own scans: 15.png at 512,1011 read
/// `Tibetan`, 04.png at 786,923 read `Syriac`, and both are ordinary dialogue.
///
/// So the list is an allow-list rather than a deny-list, and it is short on
/// purpose. A script that is not on it is not a refusal - it is a verdict this
/// model is not entitled to reach alone, and [`ScriptGate::judge`] puts it to
/// the reader instead. The `-dn` suffix is the model's own "dotted normalised"
/// variant of a label and means the same script.
const TRUSTED_SCRIPTS: [&str; 7] = [
    "Latin",
    "Cyrillic",
    "Greek",
    "Fraktur",
    "Arabic",
    "Hebrew",
    "Thai",
];

fn is_trusted(label: &str) -> bool {
    let base = label.strip_suffix("-dn").unwrap_or(label);
    TRUSTED_SCRIPTS.contains(&base)
}

/// How much of a reading has to be written in a Japanese block before it
/// overturns the identifier ([`ocr::cjk_share`]).
///
/// **0.6, and it is a floor rather than a threshold anyone tuned.** The reader
/// invents nothing outside its vocabulary, which is Japanese, so a genuine
/// Japanese balloon comes back at or very near 1.0 and there is a wide empty
/// band underneath. What the share is really guarding against is the reader
/// hallucinating a few characters over Latin lettering or over screentone: that
/// produces short, mixed, low-share readings, and 0.6 sits well above them
/// without sitting so high that one stray digit in a balloon refuses it.
/// Nobody has swept it.
const MIN_CJK_SHARE: f32 = 0.6;

/// How many **Japanese** characters a reading needs before it is evidence at
/// all. A single character is what the reader emits when it has found nothing
/// and is guessing at the paper - and a single kana would pass the share test
/// at 1.0. Counted over the Japanese blocks alone, because a kana and a full
/// stop is still one character of evidence.
const MIN_RESCUE_CHARS: usize = 2;

/// The `script` a rescued verdict carries.
///
/// **Not `"Japanese"`.** A patch's provenance and the probe both read this
/// string, and a rescue is a different claim from an identification: it says a
/// reader whose whole vocabulary is Japanese produced Japanese text here, not
/// that a script identifier recognised the script. Anyone auditing a page that
/// was cleaned when it should not have been needs to be able to tell the two
/// apart without re-running anything.
pub const RESCUED_SCRIPT: &str = "Japanese_ocr";

pub struct ScriptGate {
    osd: Osd,
    /// The rescue reader, when the run has one. `None` is the shipped default
    /// and the behaviour that predates it: the weights are optional, and a run
    /// without them must reach exactly the verdicts it reached before this
    /// existed.
    ocr: Option<Ocr>,
}

impl ScriptGate {
    pub fn open(
        model: &Path,
        labels_json: &Path,
        preference: crate::accel::Preference,
    ) -> Result<ScriptGate, OsdError> {
        Ok(ScriptGate { osd: Osd::open(model, labels_json, preference)?, ocr: None })
    }

    /// Attach the rescue reader.
    ///
    /// A separate step rather than an argument to [`ScriptGate::open`] for the
    /// same reason [`crate::detect::Pipeline::with_picks`] is one: every caller
    /// that does not ask for it - the spikes, the tests, a machine where the
    /// weights were never downloaded - goes on gating exactly as before.
    pub fn with_ocr(mut self, ocr: Ocr) -> ScriptGate {
        self.ocr = Some(ocr);
        self
    }

    /// Whether a reader is attached. What `open_gate` in the adapter is asked
    /// after opening one beside a reader that would not open.
    pub fn has_reader(&self) -> bool {
        self.ocr.is_some()
    }

    /// Where the session landed and why.
    ///
    /// The same accessor [`crate::detect::Detector`] and
    /// [`crate::balloon::BalloonDetector`] carry, and for the same reason: a
    /// caller reporting which provider each of a run's models chose should not
    /// have to reach past this type to the session inside it. The gate's answer
    /// is not written into a patch's provenance - the gate produces no pixels -
    /// but it is a model that opened, and diagnostics name all of them.
    pub fn selection(&self) -> &crate::accel::Selection {
        self.osd.selection()
    }

    /// Whether the owner should give this session back at its next safe point.
    /// [`crate::detect::Detector::spent`] states the trade.
    pub fn spent(&self) -> bool {
        // The reader lives inside this gate and is released with it, so a
        // spent reader lease is this gate's own signal to give the session back.
        self.osd.spent() || self.ocr.as_ref().is_some_and(|ocr| ocr.spent())
    }

    /// Give this gate's sessions up for good, because one of them has failed in
    /// a way another run cannot survive - a reset adapter reported as
    /// device-removed. [`crate::registry::Lease::poison`] states what that
    /// costs and why it is not the same signal as an unload the user asked for.
    ///
    /// Only the identifier is poisoned, and the rescue reader needs no poison of
    /// its own: [`ScriptGate::spent`] is an `or` over both, so a poisoned
    /// identifier is already this gate answering `true`, and the reader is
    /// opened inside the gate and released with it rather than parked
    /// separately. A device that took one of them down took both, and the gate
    /// is what the caller gives back.
    pub fn poison(&self) {
        self.osd.poison();
    }

    /// Decide one region.
    ///
    /// Both balloon answers come from outside: §3 scopes the gate by whether
    /// the text sits in a balloon, and `11-phase0-spikes.md` §6 found that the
    /// text detector does not answer that question - so the module that does,
    /// [`crate::balloon`], hands its answers in here rather than this module
    /// guessing.
    ///
    /// - `inside` is [`crate::balloon::in_bubble`]: the balloon detector, or
    ///   the paper around the lettering where the detector said *outside*. The
    ///   caller computes it once per region and uses the same answer for the
    ///   engine pick and the stored detection, so the gate cannot hold back as
    ///   *outside* a region the rest of the run treats as inside, or the
    ///   reverse.
    /// - `detected` is the balloon detector's own grade
    ///   ([`crate::balloon::detected`]), which decides nothing here but whether
    ///   the reader may rescue a failed verdict ([`rescue_applies`]).
    ///
    /// `outside` is the run's answer to §3's *"cleaned only if the user opts
    /// in"*. It changes nothing for a region the balloon question puts inside
    /// a balloon: those are gated on script exactly as before.
    pub fn judge(
        &mut self,
        page: &Raster,
        seg: &Segmentation,
        region: &Region,
        detected: Detected,
        inside: bool,
        outside: OutsideText,
    ) -> Result<Verdict, OsdError> {
        if !inside {
            return Ok(match outside {
                OutsideText::Review => Verdict::OutOfBalloon,
                OutsideText::Clean => Verdict::OptedIn,
            });
        }

        let verdict = self.identify(page, seg, region)?;
        if !rescue_applies(&verdict, detected) {
            return Ok(verdict);
        }
        // A reader that fails is a reader that said nothing, and a rescue that
        // says nothing leaves the identifier's verdict standing - which is the
        // recoverable direction, and the direction this module defaults to
        // everywhere else. Nothing here can turn a missing or broken optional
        // model into a failed page.
        let Some(ocr) = self.ocr.as_mut() else { return Ok(verdict) };
        let reading = ocr.read(page, region.text_bounds()).ok();
        Ok(rescued(verdict, reading.as_ref()))
    }

    /// The script identifier's own verdict, line by line: everything `judge`
    /// did before the reader existed, unchanged.
    fn identify(
        &mut self,
        page: &Raster,
        seg: &Segmentation,
        region: &Region,
    ) -> Result<Verdict, OsdError> {
        let orientation = lines::orientation(seg, region.masking);
        let split = lines::split(seg, region.masking, orientation);
        if split.is_empty() {
            return Ok(Verdict::Uncertain);
        }

        let mut cjk = 0usize;
        let mut other: Vec<(String, usize)> = Vec::new();
        let mut best_cjk_label = String::new();

        for line in &split {
            let tight = lines::tighten(seg, line, orientation);
            let Some(script) = self.osd.identify(page, tight, orientation)? else {
                continue;
            };
            if script.strength < MIN_LINE_STRENGTH || is_abstention(&script.label) {
                continue;
            }
            if is_cjk(&script.label) {
                cjk += script.strength;
                if best_cjk_label.is_empty() {
                    best_cjk_label = script.label;
                }
            } else {
                match other.iter_mut().find(|(label, _)| *label == script.label) {
                    Some((_, n)) => *n += script.strength,
                    None => other.push((script.label, script.strength)),
                }
            }
        }

        let non_cjk: usize = other.iter().map(|(_, n)| n).sum();
        if cjk == 0 && non_cjk == 0 {
            return Ok(Verdict::Uncertain);
        }
        if cjk > non_cjk {
            return Ok(Verdict::Clean { script: best_cjk_label });
        }
        if non_cjk > cjk {
            // Ties break on the lower label so two runs agree.
            other.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let label = other.first().map(|(l, _)| l.clone()).unwrap_or_default();
            return Ok(Verdict::NotJapanese { script: label });
        }
        // Equal evidence both ways is not a verdict.
        Ok(Verdict::Uncertain)
    }
}

/// Whether the reader is allowed near this region at all.
///
/// Three conditions, and each one is a separate refusal to widen:
///
/// - **The balloon detector must be confident**, not the paper reading and not
///   a weak box. [`Detected::TextInBubble`] is the model saying *this text is
///   in a speech balloon* from either of the two directions
///   [`crate::balloon::Detected`] documents. A reader whose vocabulary is
///   Japanese, pointed at a sound effect or a caption over art, will read
///   something - so the answer to "may I read this" cannot come from the reader.
/// - **The identifier must have failed**, not disagreed. A confident `Latin`
///   is the case §3 exists for and is never second-guessed here.
/// - **A named script must be one the identifier is not good at.** See
///   [`TRUSTED_SCRIPTS`].
///
/// `Clean`, `OutOfBalloon` and `OptedIn` are not rescuable: the first needs no
/// help and the last two are not script verdicts.
pub fn rescue_applies(verdict: &Verdict, detected: Detected) -> bool {
    if detected != Detected::TextInBubble {
        return false;
    }
    match verdict {
        Verdict::Uncertain => true,
        Verdict::NotJapanese { script } => !is_trusted(script),
        Verdict::Clean { .. } | Verdict::OutOfBalloon | Verdict::OptedIn | Verdict::NotText => false,
    }
}

/// Whether a Hayai reading may rescue a failed verdict in a balloon only weaker
/// evidence found: [`Detected::Bubble`], a balloon shape near the text, or
/// [`Detected::Outside`] that the paper reading turned *inside* (a region the
/// run judged outside never reaches a script verdict; it is `OutOfBalloon`).
/// Not a confident *text in balloon*, which [`rescue_applies`] covers. It keeps
/// manga-ocr out of it, because a reader with a Japanese vocabulary reads
/// something into anything. Hayai says how sure it is, so it may look, and
/// [`shape_rescued_by`] asks for a sure reading (on a public-domain Taiwanese
/// comic, a Chinese balloon the detector graded outside read at 0.85).
pub fn shape_rescue_applies(verdict: &Verdict, detected: Detected) -> bool {
    detected != Detected::TextInBubble && rescue_applies(verdict, Detected::TextInBubble)
}

/// [`rescued_by`], for a reading at [`OVERRULE_CONFIDENCE`] or more.
pub fn shape_rescued_by(verdict: Verdict, reading: Option<&hayai::Reading>) -> Verdict {
    match reading {
        Some(reading) if reading.confidence >= OVERRULE_CONFIDENCE => rescued_by(verdict, Some(reading)),
        _ => verdict,
    }
}

/// Whether a Hayai reading may overrule a *confident* non-CJK verdict on
/// this region: [`rescue_applies`]' first condition, and a `NotJapanese` from a
/// script [`TRUSTED_SCRIPTS`] names. The identifier reads hand-drawn kana as
/// `Latin` with confidence (Black Jack ch. 1 p. 10, ちゅるるっ in a balloon), and
/// [`rescue_applies`] never lets the reader near a trusted verdict. What
/// [`overruled_by`] asks of the reading is stricter than a rescue for that reason.
pub fn overrule_applies(verdict: &Verdict, detected: Detected) -> bool {
    detected == Detected::TextInBubble && matches!(verdict, Verdict::NotJapanese { script } if is_trusted(script))
}

/// How sure a reading has to be to overrule a trusted script verdict, and how
/// much of it has to be kana or Hangul. Han alone never overrules: it is what a
/// reader trained on CJK falls back to over unfamiliar strokes, and a Latin
/// balloon read as Han would be cleaned under a language the user may not have
/// chosen. 0.75 sits under ちゅるるっ's 0.80 mean as the run crops it (text
/// bounds plus the reader's margin) and over bare art, which read at 0.66 or
/// less in the spike (spikes/hayai-ocr/README.md); one chapter, not a sweep.
const OVERRULE_CONFIDENCE: f32 = 0.75;
const OVERRULE_SHARE: f32 = 0.8;

/// What a Hayai reading does to a verdict [`overrule_applies`] let it look at:
/// a confident reading that is mostly kana or Hangul cleans under the script it
/// read; anything else leaves the identifier's verdict standing.
pub fn overruled_by(verdict: Verdict, reading: Option<&hayai::Reading>) -> Verdict {
    let Some(reading) = reading else { return verdict };
    if !reading.is_text() || reading.confidence < OVERRULE_CONFIDENCE {
        return verdict;
    }
    match reading.cjk_script(OVERRULE_SHARE) {
        Some(hayai::CjkScript::Japanese) => Verdict::Clean { script: RESCUED_SCRIPT.to_owned() },
        Some(hayai::CjkScript::Hangul) => Verdict::Clean { script: RESCUED_HANGUL.to_owned() },
        Some(hayai::CjkScript::Han) | None => verdict,
    }
}

/// The `script` of a verdict [`hayai`] rescued, by the script it read. Not
/// the identifier's labels, for the reason [`RESCUED_SCRIPT`] gives.
pub const RESCUED_HANGUL: &str = "Hangul_ocr";
pub const RESCUED_HAN: &str = "Han_ocr";

/// What a Hayai reading does to a verdict [`rescue_applies`] let it look at:
/// lettering in a CJK script cleans, under the script it read; anything else
/// leaves the identifier's verdict standing, as [`rescued`] does.
pub fn rescued_by(verdict: Verdict, reading: Option<&hayai::Reading>) -> Verdict {
    let Some(reading) = reading else { return verdict };
    if !reading.is_text() {
        return verdict;
    }
    match reading.cjk_script(MIN_CJK_SHARE) {
        Some(hayai::CjkScript::Japanese) => Verdict::Clean { script: RESCUED_SCRIPT.to_owned() },
        Some(hayai::CjkScript::Hangul) => Verdict::Clean { script: RESCUED_HANGUL.to_owned() },
        Some(hayai::CjkScript::Han) => Verdict::Clean { script: RESCUED_HAN.to_owned() },
        None => verdict,
    }
}

/// The text check on a region about to be cleaned. A reading that is not
/// lettering holds it back as [`Verdict::NotText`]; a reader that failed or
/// was not there changes nothing. It only ever holds back: no verdict that
/// holds a region becomes one that cleans it.
///
/// The caller decides which regions are checked: those cleaned without a
/// script reading ([`Verdict::OptedIn`], the outside-bubble opt-in and every
/// region under the All text policy) and those no text box claimed, whose
/// only evidence is the mask.
pub fn text_checked(verdict: Verdict, reading: Option<&hayai::Reading>) -> Verdict {
    match reading {
        Some(reading) if verdict.cleans() && !reading.is_lettering() => Verdict::NotText,
        _ => verdict,
    }
}

/// What a reading does to a verdict the reader was allowed to look at.
///
/// Taken as an `Option<&Reading>` rather than read from a session, so the whole
/// decision - including "the reader was not there" and "the reader failed"  - 
/// is one pure function with tests that need no weights.
fn rescued(verdict: Verdict, reading: Option<&Reading>) -> Verdict {
    let Some(reading) = reading else { return verdict };
    // Counted over Japanese characters only: a reading of one kana and a
    // full stop is one character of evidence, not two.
    let long_enough =
        reading.text.chars().filter(|c| ocr::is_japanese_block(*c)).count() >= MIN_RESCUE_CHARS;
    if long_enough && ocr::cjk_share(&reading.text) >= MIN_CJK_SHARE {
        Verdict::Clean { script: RESCUED_SCRIPT.to_owned() }
    } else {
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cjk_label_the_model_can_emit_is_recognised() {
        for label in [
            "Japanese",
            "Japanese_vert",
            "HanS_vert",
            "HanT",
            "Hangul",
            "Hangul_vert",
            "Hangul-dn",
            "Japanese-dn",
        ] {
            assert!(is_cjk(label), "{label}");
        }
        for label in ["Latin", "Cyrillic", "Greek", "Fraktur"] {
            assert!(!is_cjk(label), "{label}");
        }
    }

    #[test]
    fn an_abstention_is_not_a_vote_against_japanese() {
        // A punctuation-only column returns `Common`. A
        // punctuation-only box is classified Japanese on CJK punctuation alone,
        // so `Common` must not count as evidence of Latin.
        for label in ["Common", "Joined", "NULL", "Broken"] {
            assert!(is_abstention(label), "{label}");
        }
        assert!(!is_abstention("Latin"));
        assert!(!is_abstention("Japanese_vert"));
    }

    #[test]
    fn a_verdict_carries_the_key_review_lists_it_under() {
        assert_eq!(Verdict::Clean { script: "Japanese_vert".into() }.reason_key(), None);
        assert!(Verdict::Uncertain.reason_key().is_some());
        assert!(Verdict::NotJapanese { script: "Latin".into() }.reason_key().is_some());
        assert!(Verdict::OutOfBalloon.reason_key().is_some());
        assert!(!Verdict::Uncertain.cleans());
        assert!(Verdict::Clean { script: "Japanese".into() }.cleans());
        // The opt-in cleans and carries no cause: it is not a gate skip and
        // must not be counted as one.
        assert!(Verdict::OptedIn.cleans());
        assert_eq!(Verdict::OptedIn.reason_key(), None);
        assert_eq!(Verdict::OptedIn.skip_cause(), None);
    }

    #[test]
    fn the_opt_in_is_read_from_the_seam_and_defaults_to_review() {
        assert_eq!(OutsideText::from_arg(Some("clean")), OutsideText::Clean);
        assert_eq!(OutsideText::from_arg(Some("review")), OutsideText::Review);
        assert_eq!(OutsideText::from_arg(Some("yes")), OutsideText::Review);
        assert_eq!(OutsideText::from_arg(None), OutsideText::Review);
        assert_eq!(OutsideText::default(), OutsideText::Review);
    }

    /// Three causes, three keys, and no two verdicts sharing either. A fold of
    /// `NotJapanese` into `low-confidence` - the shape the interface's original
    /// two-value union forced - would report the opposite of what happened.
    #[test]
    fn every_held_back_verdict_has_its_own_cause_and_key() {
        let held = [
            Verdict::NotJapanese { script: "Latin".into() },
            Verdict::Uncertain,
            Verdict::OutOfBalloon,
        ];
        let causes: Vec<_> = held.iter().map(|v| v.skip_cause().unwrap()).collect();
        let keys: Vec<_> = held.iter().map(|v| v.reason_key().unwrap()).collect();
        for list in [&causes, &keys] {
            let mut sorted = list.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), 3, "two verdicts report as one: {list:?}");
        }
        assert_eq!(Verdict::Clean { script: "Japanese".into() }.skip_cause(), None);
    }

    fn reading(text: &str) -> Reading {
        Reading { text: text.to_owned(), score: -0.1 }
    }

    /// The allow-list is the whole of the rescue's scope on a `NotJapanese`,
    /// and the two labels that started this are both outside it.
    #[test]
    fn the_scripts_the_identifier_reads_well_are_believed_and_the_rest_are_not() {
        for label in ["Latin", "Cyrillic", "Greek", "Fraktur", "Thai", "Latin-dn"] {
            assert!(is_trusted(label), "{label}");
        }
        // The two the user's pages actually produced over kana, and the CJK
        // labels, which never reach the rescue because they are `Clean`.
        for label in ["Tibetan", "Syriac", "Japanese", "HanS_vert", "Hangul", "Devanagari", ""] {
            assert!(!is_trusted(label), "{label}");
        }
    }

    /// The reader is only ever asked about a region the balloon detector put
    /// *confidently* in a bubble, and only about a verdict that failed.
    #[test]
    fn only_a_failed_verdict_inside_a_confident_balloon_is_offered_to_the_reader() {
        let uncertain = Verdict::Uncertain;
        let tibetan = Verdict::NotJapanese { script: "Tibetan".into() };
        let latin = Verdict::NotJapanese { script: "Latin".into() };

        assert!(rescue_applies(&uncertain, Detected::TextInBubble));
        assert!(rescue_applies(&tibetan, Detected::TextInBubble));
        // A script §3 is actually about is never second-guessed.
        assert!(!rescue_applies(&latin, Detected::TextInBubble));

        // A weak balloon box or none at all is not enough, however the gate
        // read the script. `Detected::Bubble` is a statement about a shape near
        // the text; the reader must not be pointed at a sound effect.
        for detected in [Detected::Bubble, Detected::Outside] {
            assert!(!rescue_applies(&uncertain, detected), "{detected:?}");
            assert!(!rescue_applies(&tibetan, detected), "{detected:?}");
        }

        // Nothing that already decided is reopened.
        for verdict in [
            Verdict::Clean { script: "Japanese_vert".into() },
            Verdict::OutOfBalloon,
            Verdict::OptedIn,
        ] {
            assert!(!rescue_applies(&verdict, Detected::TextInBubble), "{verdict:?}");
        }
    }

    /// The rescue decision itself, over every shape of reading, with no model
    /// anywhere near it.
    #[test]
    fn a_reading_rescues_only_when_it_is_long_enough_and_japanese_enough() {
        let held = Verdict::Uncertain;
        let clean = Verdict::Clean { script: RESCUED_SCRIPT.into() };

        assert_eq!(rescued(held.clone(), Some(&reading("こんにちは"))), clean);
        assert_eq!(rescued(held.clone(), Some(&reading("漢字だ"))), clean);
        // Two characters is the floor, and it is inclusive.
        assert_eq!(rescued(held.clone(), Some(&reading("あい"))), clean);

        // One character is a guess at the paper, not a reading.
        assert_eq!(rescued(held.clone(), Some(&reading("あ"))), held);
        assert_eq!(rescued(held.clone(), Some(&reading("あ."))), held, "punctuation is not evidence");
        assert_eq!(rescued(held.clone(), Some(&reading("あ 1"))), held);
        assert_eq!(rescued(held.clone(), Some(&reading(""))), held);
        // Latin the reader hallucinated over lettering it could not read.
        assert_eq!(rescued(held.clone(), Some(&reading("HELLO"))), held);
        // A mixture under the share: two of five is 0.4.
        assert_eq!(rescued(held.clone(), Some(&reading("あいABC"))), held);
        // Three of four is 0.75 and clears it.
        assert_eq!(rescued(held.clone(), Some(&reading("あいうA"))), clean);

        // No reader, or a reader that failed, leaves the verdict alone - which
        // is the behaviour of every machine that never downloaded the weights.
        assert_eq!(rescued(held.clone(), None), held);
        let not_japanese = Verdict::NotJapanese { script: "Syriac".into() };
        assert_eq!(rescued(not_japanese.clone(), None), not_japanese);
        assert_eq!(rescued(not_japanese, Some(&reading("セリフだ"))), clean);
    }

    #[test]
    fn punctuation_changes_the_one_kana_rescue_floor() {
        let held = Verdict::Uncertain;
        for text in ["あ!", "あ."] {
            assert_eq!(rescued(held.clone(), Some(&reading(text))), held, "{text}");
        }
        for text in ["あ。", "あ、"] {
            assert_eq!(rescued(held.clone(), Some(&reading(text))),
                Verdict::Clean { script: RESCUED_SCRIPT.to_owned() }, "{text}");
        }
    }

    /// A rescued region says in its own verdict that a reader put it there.
    /// The string reaches provenance and the probe, and a rescue that called
    /// itself `Japanese` would be indistinguishable from an identification.
    #[test]
    fn a_rescued_verdict_names_the_reader_and_still_cleans() {
        let verdict = rescued(Verdict::Uncertain, Some(&reading("やめろ")));
        assert_eq!(verdict, Verdict::Clean { script: RESCUED_SCRIPT.into() });
        assert!(verdict.cleans());
        assert_eq!(verdict.skip_cause(), None);
        assert_eq!(verdict.reason_key(), None);
        assert_ne!(RESCUED_SCRIPT, "Japanese");
        // And it is not a label the identifier can emit, so nothing that reads
        // the identifier's vocabulary can confuse the two.
        assert!(!CJK_LABELS.contains(&RESCUED_SCRIPT));
    }

    /// The constants the decision rests on, pinned so a sweep of either is a
    /// visible change rather than a silent one.
    #[test]
    fn the_rescue_thresholds_are_the_ones_the_decision_log_records() {
        assert_eq!(MIN_CJK_SHARE, 0.6);
        assert_eq!(MIN_RESCUE_CHARS, 2);
        assert_eq!(ocr::MAX_TOKENS, 64);
    }

    fn hayai(text: &str, confidence: f32) -> hayai::Reading {
        hayai::Reading { text: text.to_owned(), confidence, floor: confidence }
    }

    /// A Hayai rescue names the script it read, so a Korean or Chinese balloon
    /// is held to that language's selection rather than passed as Japanese.
    #[test]
    fn a_hayai_rescue_names_the_script_it_read() {
        let cases = [
            ("オレもさ", Some(RESCUED_SCRIPT)),
            ("이게무슨일이야", Some(RESCUED_HANGUL)),
            ("這就是我的力量", Some(RESCUED_HAN)),
            ("WHAT", None),
        ];
        for (text, script) in cases {
            let verdict = rescued_by(Verdict::Uncertain, Some(&hayai(text, 0.95)));
            match script {
                Some(script) => assert_eq!(verdict, Verdict::Clean { script: script.to_owned() }, "{text}"),
                None => assert_eq!(verdict, Verdict::Uncertain, "{text}"),
            }
        }
        // Art never rescues, and no reading leaves the verdict alone.
        assert_eq!(rescued_by(Verdict::Uncertain, Some(&hayai("ド", 0.6))), Verdict::Uncertain);
        assert_eq!(rescued_by(Verdict::Uncertain, None), Verdict::Uncertain);
    }

    /// A trusted Latin verdict yields only to a confident kana or Hangul
    /// reading, and only in a balloon.
    #[test]
    fn a_confident_kana_reading_overrules_a_trusted_latin() {
        let latin = Verdict::NotJapanese { script: "Latin".into() };
        assert!(overrule_applies(&latin, Detected::TextInBubble));
        assert!(!overrule_applies(&latin, Detected::Bubble));
        assert!(!overrule_applies(&Verdict::NotJapanese { script: "Han".into() }, Detected::TextInBubble));
        assert!(!overrule_applies(&Verdict::Uncertain, Detected::TextInBubble));

        let kana = Verdict::Clean { script: RESCUED_SCRIPT.to_owned() };
        assert_eq!(overruled_by(latin.clone(), Some(&hayai("ちゅるるっ", 0.80))), kana);
        assert_eq!(overruled_by(latin.clone(), Some(&hayai("안녕하세요", 0.9))),
            Verdict::Clean { script: RESCUED_HANGUL.to_owned() });
        // Not sure enough, Han only, Latin read as Latin, or no reading at all.
        assert_eq!(overruled_by(latin.clone(), Some(&hayai("ちゅるるっ", 0.7))), latin);
        assert_eq!(overruled_by(latin.clone(), Some(&hayai("這就是我的力量", 0.95))), latin);
        assert_eq!(overruled_by(latin.clone(), Some(&hayai("WHAT", 0.99))), latin);
        assert_eq!(overruled_by(latin.clone(), Some(&hayai("OK ちゅ", 0.95))), latin);
        assert_eq!(overruled_by(latin.clone(), None), latin);
    }

    /// In a balloon only the weaker grade found, the reader rescues when it is sure.
    #[test]
    fn a_sure_reading_rescues_in_a_weak_balloon() {
        assert!(shape_rescue_applies(&Verdict::Uncertain, Detected::Bubble));
        assert!(!shape_rescue_applies(&Verdict::Uncertain, Detected::TextInBubble));
        assert!(shape_rescue_applies(&Verdict::Uncertain, Detected::Outside));
        assert!(!shape_rescue_applies(&Verdict::OutOfBalloon, Detected::Outside));
        assert!(!shape_rescue_applies(&Verdict::NotJapanese { script: "Latin".into() }, Detected::Bubble));
        let line = "哥哥啊! 我們不要分家了?";
        assert_eq!(shape_rescued_by(Verdict::Uncertain, Some(&hayai(line, 0.85))),
            Verdict::Clean { script: RESCUED_HAN.to_owned() });
        assert_eq!(shape_rescued_by(Verdict::Uncertain, Some(&hayai(line, 0.6))), Verdict::Uncertain);
        assert_eq!(shape_rescued_by(Verdict::Uncertain, None), Verdict::Uncertain);
    }

    /// The text check only ever holds back, and only a region that would clean.
    #[test]
    fn the_text_check_holds_art_and_nothing_else() {
        let art = hayai("ド", 0.6);
        let text = hayai("シャコーン", 0.9);
        assert_eq!(text_checked(Verdict::OptedIn, Some(&art)), Verdict::NotText);
        assert_eq!(text_checked(Verdict::OptedIn, Some(&text)), Verdict::OptedIn);
        assert_eq!(text_checked(Verdict::OptedIn, None), Verdict::OptedIn);
        let clean = Verdict::Clean { script: "Japanese".into() };
        assert_eq!(text_checked(clean.clone(), Some(&art)), Verdict::NotText);
        assert_eq!(text_checked(clean.clone(), Some(&text)), clean);
        assert_eq!(text_checked(Verdict::Uncertain, Some(&art)), Verdict::Uncertain);
        assert_eq!(text_checked(Verdict::OutOfBalloon, Some(&text)), Verdict::OutOfBalloon);
        // A low Hangul reading is a stylised sound effect, not art.
        assert_eq!(text_checked(Verdict::OptedIn, Some(&hayai("컹", 0.44))), Verdict::OptedIn);
        assert_eq!(text_checked(Verdict::OptedIn, Some(&hayai("우", 0.2))), Verdict::NotText);
        assert_eq!(text_checked(Verdict::OptedIn, Some(&hayai("ア", 0.5))), Verdict::NotText);
        // A long low reading is dense lettering; a long reading under the floor is not.
        assert_eq!(text_checked(Verdict::OptedIn, Some(&hayai("現不早了, 我天再来!", 0.45))), Verdict::OptedIn);
        assert_eq!(text_checked(Verdict::OptedIn, Some(&hayai("ゼラミコ", 0.18))), Verdict::NotText);
        assert!(!Verdict::NotText.cleans());
        assert_eq!(Verdict::NotText.skip_cause(), Some("not-text"));
    }
}
